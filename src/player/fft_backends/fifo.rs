use gio::{self, prelude::*};
use glib::clone;
use std::{
    cell::RefCell,
    rc::Rc,
    str::FromStr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use mpd::status::AudioFormat;

use super::backend::{FftBackendExt, FftBackendImpl, FftStatus};
use crate::{player::Player, utils::settings_manager};

#[derive(Debug)]
pub struct FifoFftBackend {
    fft_handle: RefCell<Option<gio::JoinHandle<()>>>,
    fg_handle: RefCell<Option<glib::JoinHandle<()>>>,
    player: Player,
    stop_flag: Arc<AtomicBool>,
}

impl FifoFftBackend {
    pub fn new(player: Player) -> Self {
        Self {
            fft_handle: RefCell::default(),
            fg_handle: RefCell::default(),
            player,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl FftBackendImpl for FifoFftBackend {
    fn name(&self) -> &'static str {
        "fifo"
    }

    fn player(&self) -> &Player {
        &self.player
    }

    /// FIFO backend does not make use of runtime configuration
    fn get_param(&self, _key: &str) -> Option<glib::Variant> {
        None
    }

    /// FIFO backend does not make use of runtime configuration
    fn set_param(&self, _key: &str, _val: glib::Variant) {}

    fn start(self: Rc<Self>, output: Arc<Mutex<(Vec<f32>, Vec<f32>)>>) -> Result<(), ()> {
        self.stop_flag.store(false, Ordering::Relaxed);
        let curr_status = self.status();
        println!("Current status: {curr_status:?}");
        if curr_status != FftStatus::Reading && curr_status != FftStatus::Stopping {
            let stop_flag = self.stop_flag.clone();
            let (sender, receiver) = async_channel::unbounded::<FftStatus>();
            let fft_handle = gio::spawn_blocking(move || {
                println!("Starting FIFO backend");
                let settings = settings_manager();
                let player_settings = settings.child("player");
                // Will require starting a new thread to account for path and format changes
                if let Ok(format) = AudioFormat::from_str(
                    settings.child("client").string("mpd-fifo-format").as_str(),
                ) {
                    // These settings require a restart
                    let n_samples = player_settings.uint("visualizer-fft-samples") as usize;
                    let n_bins = player_settings.uint("visualizer-spectrum-bins") as usize;
                    if let Ok(mut reader) = super::fft::try_open_pipe(
                        settings.child("client").string("mpd-fifo-path").as_str(),
                        &format,
                        n_samples,
                    ) {
                        // Allocate the following once only
                        let mut fft_buf_left: Vec<f32> = vec![0.0; n_samples];
                        let mut fft_buf_right: Vec<f32> = vec![0.0; n_samples];
                        let mut curr_step_left: Vec<f32> = vec![0.0; n_bins];
                        let mut curr_step_right: Vec<f32> = vec![0.0; n_bins];
                        let mut was_reading: bool = false;
                        'outer: loop {
                            // These should be applied on-the-fly
                            let bin_mode =
                                if player_settings.boolean("visualizer-spectrum-use-log-bins") {
                                    super::fft::BinMode::Logarithmic
                                } else {
                                    super::fft::BinMode::Linear
                                };
                            let fps = player_settings.uint("visualizer-fps") as f32;
                            let min_freq =
                                player_settings.uint("visualizer-spectrum-min-hz") as f32;
                            let max_freq =
                                player_settings.uint("visualizer-spectrum-max-hz") as f32;
                            let curr_step_weight = player_settings
                                .double("visualizer-spectrum-curr-step-weight")
                                as f32;
                            match super::fft::get_stereo_pcm(
                                &mut fft_buf_left,
                                &mut fft_buf_right,
                                &mut reader,
                                &format,
                                fps,
                                true,
                            ) {
                                Ok(()) => {
                                    // Compute outside of mutex lock please
                                    super::fft::get_magnitudes(
                                        &format,
                                        &mut fft_buf_left,
                                        &mut curr_step_left,
                                        n_bins as u32,
                                        bin_mode,
                                        min_freq,
                                        max_freq,
                                    );
                                    super::fft::get_magnitudes(
                                        &format,
                                        &mut fft_buf_right,
                                        &mut curr_step_right,
                                        n_bins as u32,
                                        bin_mode,
                                        min_freq,
                                        max_freq,
                                    );
                                    // Replace last frame
                                    if let Ok(mut output_lock) = output.lock() {
                                        if output_lock.0.len() != n_bins
                                            || output_lock.1.len() != n_bins
                                        {
                                            output_lock.0.clear();
                                            output_lock.1.clear();
                                            for _ in 0..n_bins {
                                                output_lock.0.push(0.0);
                                                output_lock.1.push(0.0);
                                            }
                                        }
                                        for i in 0..n_bins {
                                            output_lock.0[i] = curr_step_left[i] * curr_step_weight
                                                + output_lock.0[i] * (1.0 - curr_step_weight);
                                            output_lock.1[i] = curr_step_right[i]
                                                * curr_step_weight
                                                + output_lock.1[i] * (1.0 - curr_step_weight);
                                        }
                                        // println!("FFT L: {:?}\tR: {:?}", &output_lock.0, &output_lock.1);
                                    } else {
                                        panic!("Unable to lock FFT data mutex");
                                    }
                                }
                                Err(e) => match e.kind() {
                                    std::io::ErrorKind::UnexpectedEof
                                    | std::io::ErrorKind::WouldBlock => {
                                        was_reading = false;
                                        let _ = sender.send_blocking(FftStatus::ValidNotReading);
                                    }
                                    _ => {
                                        println!("FFT ERR: {:?}", &e);
                                        break 'outer;
                                    }
                                },
                            }
                            // Placed here such that we can use the first iteration to verify
                            // that the settings are correct.
                            if stop_flag.load(Ordering::Relaxed) {
                                println!("Stopping thread...");
                                return;
                            } else if !was_reading {
                                was_reading = true;
                                let _ = sender.send_blocking(FftStatus::Reading);
                            }
                            thread::sleep(Duration::from_millis((1000.0 / fps).floor() as u64));
                        }
                    }
                }
                // All graceful thread shutdowns are inside the loop. If we've reached here then
                // it's an error.
                let _ = sender.send_blocking(FftStatus::Invalid);
            });
            self.fft_handle.replace(Some(fft_handle));

            let player = self.player();
            if let Some(old_handle) =
                self.fg_handle
                    .replace(Some(glib::MainContext::default().spawn_local(clone!(
                        #[weak]
                        player,
                        async move {
                            use futures::prelude::*;
                            // Allow receiver to be mutated, but keep it at the same memory address.
                            // See Receiver::next doc for why this is needed.
                            let mut receiver = std::pin::pin!(receiver);
                            while let Some(new_status) = receiver.next().await {
                                player.set_fft_status(new_status);
                            }
                        }
                    ))))
            {
                old_handle.abort();
            }

            return Ok(());
        } else {
            println!("Another FIFO thread is already running");
        }
        Err(())
    }

    fn stop(&self, block: bool) {
        self.stop_flag.store(true, Ordering::Relaxed);
        if let Some(handle) = self.fft_handle.take() {
            if block {
                let stop_future = glib::MainContext::default().spawn_local(async move {
                    let _ = handle.await;
                });
                let _ = glib::MainContext::default().block_on(stop_future);
            }
        }
        // In case the thread is dead to begin with
        self.player().set_fft_status(FftStatus::ValidNotReading);
        if let Some(old_handle) = self.fg_handle.take() {
            old_handle.abort();
        }
    }
}
