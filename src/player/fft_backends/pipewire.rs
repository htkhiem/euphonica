use std::{cell::RefCell, sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex}, thread, time::Duration};
use gio::prelude::SettingsExt;
use mpd::status::AudioFormat;
use pipewire as pw;
use pw::{properties::properties, spa};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::{format_utils, audio::AudioFormat as SpaAudioFormat};
use spa::pod::Pod;
use std::convert::TryInto;
use std::mem;
use ringbuffer::{AllocRingBuffer, RingBuffer};

use crate::utils::settings_manager;

// Based on https://gitlab.freedesktop.org/pipewire/pipewire-rs/-/raw/main/pipewire/examples/audio-capture.rs
// Our PipeWire backend involves two threads:
// - A capture thread, run by a pw::main_loop::MainLoop, and
// - The actual FFT thread, similar to the existing FIFO backend.
// The capture thread writes into ring buffers to decouple FPS and window size
// from PipeWire configuration, similar to the FIFO backend, except that in
// the FIFO backend, the ringbuffer is implemented internally by BufReader.
use super::backend::{FftBackend, FftStatus};

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    cursor_move: bool,
}

// Our message to the pipewire loop, this tells it to terminate.
struct Terminate;

pub struct PipeWireFftBackend {
    pw_sender: RefCell<Option<pw::channel::Sender<Terminate>>>,
    pw_handle: RefCell<Option<gio::JoinHandle<()>>>,
    fft_stop: Arc<AtomicBool>,
    fft_handle: RefCell<Option<gio::JoinHandle<()>>>,

}

impl Default for PipeWireFftBackend {
    fn default() -> Self {
        pw::init();
        Self {
            pw_sender: RefCell::new(None),
            pw_handle: RefCell::new(None),
            fft_stop: Arc::new(AtomicBool::new(false)),
            fft_handle: RefCell::new(None)
        }
    }
}

impl FftBackend for PipeWireFftBackend {
    fn start(&self, output: Arc<Mutex<(Vec<f32>, Vec<f32>)>>) -> Result<(), ()> {
        let stop_flag = self.fft_stop.clone();
        stop_flag.store(false, Ordering::Relaxed);
        let should_start = {
            self.pw_handle.borrow().is_none() && self.fft_handle.borrow().is_none()
        };
        if should_start {
            let settings = settings_manager();
            let player_settings = settings.child("player");
            let n_samples = player_settings.uint("visualizer-fft-samples") as usize;
            let n_bins = player_settings.uint("visualizer-spectrum-bins") as usize;
            let (pw_sender, pw_receiver) = pw::channel::channel::<Terminate>();
            let samples = {
                let mut samples: (AllocRingBuffer<f32>, AllocRingBuffer<f32>) = (
                    AllocRingBuffer::new(n_samples), AllocRingBuffer::new(n_samples)
                );
                samples.0.fill_with(|| 0.0);
                samples.1.fill_with(|| 0.0);
                Arc::new(Mutex::new(samples))
            };
            let format: Arc<Mutex<AudioFormat>> = Arc::new(Mutex::new(AudioFormat {rate: 0, bits: 0, chans: 0}));
            // Give the PipeWire thread one copy of each
            let pw_samples = samples.clone();
            let pw_format = format.clone();
            self.pw_sender.replace(Some(pw_sender));
            // Start the PipeWire capture thread
            let pw_handle = gio::spawn_blocking(move || {
                let pw_loop = pw::main_loop::MainLoop::new(None).expect("PipeWire FFT thread crashed");
                let _receiver = pw_receiver.attach(pw_loop.loop_(), {
                    let pw_loop = pw_loop.clone();
                    move |_| pw_loop.quit()
                });

                let context = pw::context::Context::new(&pw_loop).expect("PipeWire FFT thread crashed");
                let core = context.connect(None).expect("PipeWire FFT thread crashed");

                let data = UserData {
                    format: Default::default(),
                    cursor_move: false,
                };

                /* Create a simple stream, the simple stream manages the core and remote
                 * objects for you if you don't need to deal with them.
                 *
                 * If you plan to autoconnect your stream, you need to provide at least
                 * media, category and role properties.
                 *
                 * Pass your events and a user_data pointer as the last arguments. This
                 * will inform you about the stream state. The most important event
                 * you need to listen to is the process event where you need to produce
                 * the data.
                 */
                let props = properties! {
                    *pw::keys::MEDIA_TYPE => "Audio",
                    *pw::keys::MEDIA_CATEGORY => "Capture",
                    *pw::keys::MEDIA_ROLE => "Music",
                };

                let pw_stream = pw::stream::Stream::new(&core, "audio-capture", props).expect("PipeWire FFT thread crashed");

                let _listener = pw_stream
                    .add_local_listener_with_user_data(data)
                // After connecting the stream, the server will want to configure some parameters on the stream
                    .param_changed(|_, user_data, id, param| {
                        // NULL means to clear the format
                        let Some(param) = param else {
                            return;
                        };
                        if id != pw::spa::param::ParamType::Format.as_raw() {
                            return;
                        }

                        let (media_type, media_subtype) = match format_utils::parse_format(param) {
                            Ok(v) => v,
                            Err(_) => return,
                        };

                        if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                            return;
                        }

                        user_data
                            .format
                            .parse(param)
                            .expect("Failed to parse param 'changed' to AudioInfoRaw");
                    })
                    .process(move |stream, user_data| match stream.dequeue_buffer() {
                        None => {return;},
                        Some(mut buffer) => {
                            let buffer_data = buffer.datas_mut();
                            if buffer_data.is_empty() {
                                return;
                            }

                            let data = &mut buffer_data[0];
                            let n_samples_avail = data.chunk().size() / (mem::size_of::<f32>() as u32);
                            let n_channels = user_data.format.channels();
                            if let Ok(mut format_lock) = pw_format.lock() {
                                *format_lock = AudioFormat {
                                    rate: user_data.format.rate(),
                                    chans: n_channels as u8,
                                    bits: match user_data.format.format() {
                                        SpaAudioFormat::F32BE | SpaAudioFormat::F32LE => 32,
                                        _ => unimplemented!()
                                        // Might support these directly in the future but for now we're only
                                        // taking in float32le.
                                        // SpaAudioFormat::F64BE | SpaAudioFormat::F64LE => 64,
                                        // SpaAudioFormat::S16 | SpaAudioFormat::S16BE | SpaAudioFormat::S16LE | SpaAudioFormat::U16 | SpaAudioFormat::U16BE | SpaAudioFormat::U16LE => 16,
                                        // SpaAudioFormat::S24 | SpaAudioFormat::S24BE | SpaAudioFormat::S24LE | SpaAudioFormat::U24 | SpaAudioFormat::U24BE | SpaAudioFormat::U24LE => 24,
                                    }
                                };
                            }
                            if let Some(samples) = data.data() {
                                // ASSUME THERE ARE AT LEAST TWO CHANNELS.
                                // I'm not gatekeeping but audiophiles listen to at least stereo sound :)
                                let mut locked_buffer = pw_samples.lock().unwrap();
                                for n in (0..n_samples_avail).step_by(n_channels as usize) {
                                    let l_start = n as usize * mem::size_of::<f32>();
                                    let l_end = l_start + mem::size_of::<f32>();
                                    let r_end = l_end + mem::size_of::<f32>();

                                    locked_buffer.0.push(f32::from_le_bytes(samples[l_start..l_end].try_into().unwrap()));
                                    locked_buffer.1.push(f32::from_le_bytes(samples[l_end..r_end].try_into().unwrap()));
                                }
                                user_data.cursor_move = true;
                            }
                        }
                    })
                    .register().expect("PipeWire FFT thread crashed");

                /* Make one parameter with the supported formats. The SPA_PARAM_EnumFormat
                 * id means that this is a format enumeration (of 1 value).
                 * We leave the channels and rate empty to accept the native graph
                 * rate and channels. */
                let mut audio_info = spa::param::audio::AudioInfoRaw::new();
                audio_info.set_format(spa::param::audio::AudioFormat::F32LE);
                let obj = pw::spa::pod::Object {
                    type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
                    id: pw::spa::param::ParamType::EnumFormat.as_raw(),
                    properties: audio_info.into(),
                };
                let values: Vec<u8> = pw::spa::pod::serialize::PodSerializer::serialize(
                    std::io::Cursor::new(Vec::new()),
                    &pw::spa::pod::Value::Object(obj),
                )
                    .unwrap()
                    .0
                    .into_inner();

                let mut params = [Pod::from_bytes(&values).unwrap()];

                /* Now connect this stream. We ask that our process function is
                 * called in a realtime thread. */
                pw_stream.connect(
                    spa::utils::Direction::Input,
                    None,
                    pw::stream::StreamFlags::AUTOCONNECT
                        | pw::stream::StreamFlags::MAP_BUFFERS
                        | pw::stream::StreamFlags::RT_PROCESS,
                    &mut params,
                ).expect("PipeWire FFT thread crashed");
                pw_loop.run();
            });
            self.pw_handle.replace(Some(pw_handle));

            // Run FFT thread
            let fft_handle = gio::spawn_blocking(move || {
                let settings = settings_manager();
                let player_settings = settings.child("player");
                // Allocate the following once only
                let mut fft_buf_left: Vec<f32> = vec![0.0; n_samples];
                let mut fft_buf_right: Vec<f32> = vec![0.0; n_samples];
                let mut curr_step_left: Vec<f32> = vec![0.0; n_bins];
                let mut curr_step_right: Vec<f32> = vec![0.0; n_bins];
                'outer: loop {
                    if stop_flag.load(Ordering::Relaxed) {
                        break 'outer;
                    }
                    // Just get our own copy to minimise blocking
                    let format_lock = {
                        format.lock().expect("Unable to read format of PipeWire stream from FFT thread").clone()
                    };
                    // Skip processing until format is nonzero
                    if format_lock.rate == 0 || format_lock.chans == 0 { continue; }
                    // Copy ringbuffer to our static ones. Take care to read backward from the latest sample.
                    if let Ok(ringbuffers) = samples.lock() {
                        for pos in 0..n_samples {
                            fft_buf_left[n_samples - pos - 1] = *ringbuffers.0.get_signed(-1 - pos as isize).unwrap_or(&(0.0 as f32));
                            fft_buf_right[n_samples - pos - 1] = *ringbuffers.1.get_signed(-1 - pos as isize).unwrap_or(&(0.0 as f32));
                        }
                    }
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
                    // Compute outside of output mutex lock please

                    super::fft::get_magnitudes(
                        &format_lock,
                        &mut fft_buf_left,
                        &mut curr_step_left,
                        n_bins as u32,
                        bin_mode,
                        min_freq,
                        max_freq,
                    );
                    super::fft::get_magnitudes(
                        &format_lock,
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
                            // FIXME: To line up with FIFO backend we should scale this backend's magnitudes
                            // up by 5x.
                            output_lock.0[i] = curr_step_left[i] * curr_step_weight * 5.0
                                + output_lock.0[i] * (1.0 - curr_step_weight);
                            output_lock.1[i] = curr_step_right[i]
                                * curr_step_weight * 5.0
                                + output_lock.1[i] * (1.0 - curr_step_weight);
                        }
                        // println!("FFT L: {:?}\tR: {:?}", &output_lock.0, &output_lock.1);
                    } else {
                        panic!("Unable to lock FFT data mutex");
                    }
                    thread::sleep(Duration::from_millis((1000.0 / fps).floor() as u64));
                }
            });
            self.fft_handle.replace(Some(fft_handle));
            Ok(())
        }
        else {
            Err(())
        }
    }

    fn stop(&self) {
        let fft_stop = self.fft_stop.clone();
        fft_stop.store(true, Ordering::Relaxed);
        if let Some(sender) = self.pw_sender.take() {
            if sender.send(Terminate).is_ok() {
                if let Some(handle) = self.pw_handle.take() {
                    let _ = glib::MainContext::default().block_on(handle);
                }
                if let Some(handle) = self.fft_handle.take() {
                    let _ = glib::MainContext::default().block_on(handle);
                }
            }
        }
    }

    fn status(&self) -> FftStatus {
        if self.pw_handle.borrow().as_ref().is_some() {
            FftStatus::Reading
        }
        else {
            FftStatus::Invalid
        }
    }
}
