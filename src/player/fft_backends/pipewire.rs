use gio::{self, prelude::*};
use glib::clone;
use mpd::status::AudioFormat;
use pipewire as pw;
use pw::{properties::properties, spa};
use ringbuffer::{AllocRingBuffer, RingBuffer};
use spa::param::format::{MediaSubtype, MediaType};
use spa::param::{audio::AudioFormat as SpaAudioFormat, format_utils};
use spa::pod::Pod;
use std::convert::TryInto;
use std::mem;
use std::{
    cell::RefCell,
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use crate::{player::Player, utils::settings_manager};

// Based on https://gitlab.freedesktop.org/pipewire/pipewire-rs/-/raw/main/pipewire/examples/audio-capture.rs
// Our PipeWire backend involves two threads:
// - A capture thread, run by a pw::main_loop::MainLoop, and
// - The actual FFT thread, similar to the existing FIFO backend.
// The capture thread writes into ring buffers to decouple FPS and window size
// from PipeWire configuration, similar to the FIFO backend, except that in
// the FIFO backend, the ringbuffer is implemented internally by BufReader.
//
// The PipeWire backend makes use of runtime-configurable parameters.
//
// Stream connection flow:
// 1. Start a new thread for PipeWire
// 2. Get the last-connected device name from our GSettings backend. A Device node in PipeWire has two
// "names", a unique node name that we'll use to connect the stream, and a friendly (user-facing) nickname.
// In GSettings we'll store the unique node name.
// 3. Query all currently-connected devices.
// 4. If the last-connected device name is not blank and exists in the list of devices from step 3,
// connect the stream to it, then set current_device to its index in the list.
// 5. If such device does not exist or the name is an empty string, set GSettings key to empty string
// (signifying "auto"), the current_device to -1, then autoconnect the stream using media category.
// This usually happens with removable playback devices like USB DACs.
//
// To implement device selection in UI:
// 1. After the stream has already been connected, save the device list (both unique and friendly names) plus the current
// device into the backend struct, then fire a param-changed signal with key "devices".
// 2. If the preferences UI is already open, it will receive this signal and get the "devices" and "current-device"
// params. The acquired param values will be used to initialise the device dropdown.
// 3. If there is no preferences UI open yet, the signal will be ignored. When the UI is opened later, it will
// try to fetch the params by itself upon creation. If by this time the backend has not finished connecting the stream,
// simply return Nones and disable the corresponding UI elements, until notified otherwise by the signal in step 1.

use super::backend::{FftBackendExt, FftBackendImpl, FftStatus};

struct Terminate;

struct UserData {
    format: spa::param::audio::AudioInfoRaw,
    cursor_move: bool,
}

#[derive(Debug, Clone, Copy)]
enum PipeWireMsg {
    Status(FftStatus),
    DevicesChanged,
    CurrentDeviceChanged,
}

#[derive(Debug, Clone)]
struct OutputNode {
    pub node_name: String,
    pub display_name: String,
}

pub struct PipeWireFftBackend {
    pw_handle: RefCell<Option<gio::JoinHandle<()>>>,
    pw_sender: RefCell<Option<pw::channel::Sender<Terminate>>>,
    fft_handle: RefCell<Option<gio::JoinHandle<()>>>,
    fg_handle: RefCell<Option<glib::JoinHandle<()>>>,
    devices: Arc<Mutex<Vec<OutputNode>>>,
    curr_device: Arc<Mutex<i32>>,
    player: Player,
    stop_flag: Arc<AtomicBool>,
}

impl PipeWireFftBackend {
    pub fn new(player: Player) -> Self {
        Self {
            pw_handle: RefCell::default(),
            pw_sender: RefCell::default(),
            fft_handle: RefCell::default(),
            fg_handle: RefCell::default(),
            devices: Arc::new(Mutex::new(Vec::new())),
            curr_device: Arc::new(Mutex::new(-1)),
            player,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl FftBackendImpl for PipeWireFftBackend {
    fn name(&self) -> &'static str {
        "pipewire"
    }

    fn player(&self) -> &Player {
        &self.player
    }

    fn get_param(&self, key: &str) -> Option<glib::Variant> {
        match key {
            "devices" => Some(
                self.devices
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|dev| dev.display_name.clone())
                    .collect::<Vec<String>>()
                    .to_variant(),
            ),
            "current-device" => Some((*self.curr_device.lock().unwrap()).to_variant()),
            _ => None,
        }
    }

    fn set_param(&self, key: &str, val: glib::Variant) {
        match key {
            // "devices" => {
            //     if let Some(devices) = val.get::<Vec<String>>() {
            //         let new_len = devices.len() as i32;
            //         self.devices.replace(devices);
            //         self.curr_device.set(0);
            //         self.emit_param_changed("devices", &val);
            //         if self.curr_device.get() >= new_len {
            //             let new_val = new_len - 1;
            //             self.curr_device.set(new_val);
            //             self.emit_param_changed("current-device", &new_val.to_variant());
            //         }
            //     }
            // },
            "current-device" => {
                if let Some(new_idx) = val.get::<i32>() {
                    let max_idx = self.devices.lock().unwrap().len() as i32 - 1;
                    let final_val = max_idx.min(new_idx);
                    *self.curr_device.lock().unwrap() = final_val;
                    let settings = settings_manager().child("client");
                    if final_val < 0 {
                        let _ = settings.set_string("pipewire-last-device", "");
                    } else {
                        let _ = settings.set_string(
                            "pipewire-last-device",
                            &(*self.devices.lock().unwrap())[final_val as usize].node_name,
                        );
                    }
                }
            }
            _ => {}
        }
    }

    fn start(self: Rc<Self>, output: Arc<Mutex<(Vec<f32>, Vec<f32>)>>) -> Result<(), ()> {
        let stop_flag = self.stop_flag.clone();
        stop_flag.store(false, Ordering::Relaxed);
        let should_start =
            { self.pw_handle.borrow().is_none() && self.fft_handle.borrow().is_none() };
        if should_start {
            let devices = self.devices.clone();
            let curr_device = self.curr_device.clone();

            let player_settings = settings_manager().child("player");
            let n_samples = player_settings.uint("visualizer-fft-samples") as usize;
            let n_bins = player_settings.uint("visualizer-spectrum-bins") as usize;
            let (fg_sender, fg_receiver) = async_channel::unbounded::<PipeWireMsg>();
            let (pw_sender, pw_receiver) = pw::channel::channel::<Terminate>();
            let samples = {
                let mut samples: (AllocRingBuffer<f32>, AllocRingBuffer<f32>) = (
                    AllocRingBuffer::new(n_samples),
                    AllocRingBuffer::new(n_samples),
                );
                samples.0.fill_with(|| 0.0);
                samples.1.fill_with(|| 0.0);
                Arc::new(Mutex::new(samples))
            };
            let format: Arc<Mutex<AudioFormat>> = Arc::new(Mutex::new(AudioFormat {
                rate: 0,
                bits: 0,
                chans: 0,
            }));
            // Give the PipeWire thread one copy of each
            let pw_samples = samples.clone();
            let pw_format = format.clone();
            self.pw_sender.replace(Some(pw_sender));

            let pw_handle = gio::spawn_blocking(clone!(
                #[strong]
                fg_sender,
                move || {
                    // Get list of devices
                    println!("PipeWire: getting list of devices");
                    {
                        let mainloop = pw::main_loop::MainLoop::new(None)
                            .expect("get_devices: Unable to create a new PipeWire mainloop");
                        let context = pw::context::Context::new(&mainloop)
                            .expect("get_devices: Unable to get PipeWire context");
                        let mainloop: Arc<pipewire::main_loop::MainLoop> = Arc::new(mainloop);
                        let core = context.connect(None).unwrap();
                        let registry = core.get_registry().unwrap();

                        let devices_clone = devices.clone();
                        let _listener = registry
                            .add_listener_local()
                            .global(move |global| {
                                if global.type_ == pw::types::ObjectType::Node {
                                    let props = global.props.as_ref().unwrap();
                                    let node_name = props.get("node.name").unwrap();
                                    if props
                                        .get("application.name")
                                        .is_some_and(|name| name == "Music Player Daemon")
                                    {
                                        devices_clone.lock().unwrap().push(OutputNode {
                                            node_name: node_name.to_owned(),
                                            display_name: format!("MPD PipeWire ({node_name})"),
                                        });
                                    } else if props.get("media.class").is_some_and(|mclass| {
                                        mclass == "Audio/Sink" || mclass == "Stream/Output/Audio"
                                    }) {
                                        println!("Found new PipeWire output node: {}", &node_name);
                                        devices_clone.lock().unwrap().push(OutputNode {
                                            node_name: node_name.to_owned(),
                                            display_name: props
                                                .get("node.description")
                                                .unwrap_or(node_name)
                                                .to_owned(),
                                        });
                                    }
                                }
                            })
                            .register();

                        // Force roundtrip to return results once all globals have been received
                        let mainloop_clone = mainloop.clone();

                        // Queue a sync signal after processing all of the above so the loop knows when to stop.
                        let target_seq = core
                            .sync(0)
                            .expect("Cannot force PipeWire object enumeration roundtrip");

                        let roundtrip_listener = core
                            .add_listener_local()
                            .done(move |id, seq| {
                                println!("Sync done signal received with seq: {}", seq.seq());
                                if id == pw::core::PW_ID_CORE && seq.seq() == target_seq.seq() {
                                    // println!("All globalobjects have been received, quitting get_devices mainloop");
                                    mainloop_clone.quit();
                                }
                            })
                            .register();

                        // Will block until all objects have been enumerated
                        mainloop.run();
                        roundtrip_listener.unregister();
                        {
                            let mut device_lock = devices.lock().unwrap();
                            device_lock.truncate(64);
                        }
                    }
                    let settings = settings_manager().child("client");
                    let _last_device = settings.string("pipewire-last-device");
                    let last_device = _last_device.as_str();
                    {
                        let devices_lock = devices.lock().unwrap();
                        let mut curr_device = curr_device.lock().unwrap();
                        if !devices_lock.is_empty() {
                            if let Some(device_idx) = devices_lock
                                .iter()
                                .position(|elem| elem.node_name == last_device)
                            {
                                *curr_device = device_idx as i32;
                            } else {
                                *curr_device = -1;
                            }
                        }
                    }

                    // Notify main thread of param changes
                    let _ = fg_sender.send_blocking(PipeWireMsg::DevicesChanged);
                    let _ = fg_sender.send_blocking(PipeWireMsg::CurrentDeviceChanged);

                    // Now we can finally start the capture stream & FFT thread

                    // PipeWire capture thread
                    let Ok(pw_loop) = pw::main_loop::MainLoop::new(None) else {
                        let _ = fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                        return;
                    };
                    let _receiver = pw_receiver.attach(pw_loop.loop_(), {
                        let pw_loop = pw_loop.clone();
                        move |_| pw_loop.quit()
                    });

                    let Ok(context) = pw::context::Context::new(&pw_loop) else {
                        let _ = fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                        return;
                    };
                    let Ok(core) = context.connect(None) else {
                        let _ = fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                        return;
                    };

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

                    let props: pw::properties::Properties;
                    {
                        let curr_device_lock = curr_device.lock().unwrap();
                        let devices = devices.lock().unwrap();
                        if *curr_device_lock >= 0 {
                            let node_name =
                                devices[*curr_device_lock as usize].node_name.to_owned();
                            println!("Connecting PipeWire stream to node '{}'", &node_name);
                            props = properties! {
                                *pw::keys::MEDIA_TYPE => "Audio",
                                *pw::keys::MEDIA_CATEGORY => "Capture",
                                *pw::keys::MEDIA_ROLE => "Music",
                                *pw::keys::TARGET_OBJECT => node_name,
                                *pw::keys::STREAM_CAPTURE_SINK => "true",
                                *pw::keys::STREAM_MONITOR => "true",
                            };
                        } else {
                            println!("Autoconnecting PipeWire stream");
                            props = properties! {
                                *pw::keys::MEDIA_TYPE => "Audio",
                                *pw::keys::MEDIA_CATEGORY => "Capture",
                                *pw::keys::MEDIA_ROLE => "Music",
                                *pw::keys::STREAM_CAPTURE_SINK => "true",
                                *pw::keys::STREAM_MONITOR => "true",
                            };
                        };
                    }

                    let Ok(pw_stream) = pw::stream::Stream::new(&core, "audio-capture", props)
                    else {
                        let _ = fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                        return;
                    };

                    let Ok(_listener) = pw_stream
                        .add_local_listener_with_user_data(data)
                        // After connecting the stream, the server will want to configure some parameters on the stream
                        .param_changed(clone!(
                            #[strong]
                            fg_sender,
                            move |_, user_data, id, param| {
                                // NULL means to clear the format
                                let Some(param) = param else {
                                    return;
                                };
                                if id != pw::spa::param::ParamType::Format.as_raw() {
                                    return;
                                }

                                let (media_type, media_subtype) =
                                    match format_utils::parse_format(param) {
                                        Ok(v) => v,
                                        Err(_) => return,
                                    };

                                if media_type != MediaType::Audio
                                    || media_subtype != MediaSubtype::Raw
                                {
                                    println!("Not MediaType::Audio || MediaSubtype::Raw, skipping");
                                    return;
                                }

                                println!("Setting up stream format");
                                let Ok(_) = user_data.format.parse(param) else {
                                    println!("Failed to parse format");
                                    let _ = fg_sender
                                        .send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                                    return;
                                };
                            }
                        ))
                        .process(move |stream, user_data| match stream.dequeue_buffer() {
                            None => {}
                            Some(mut buffer) => {
                                let buffer_data = buffer.datas_mut();
                                if buffer_data.is_empty() {
                                    // println!("buffer_data is empty. Skipping");
                                    return;
                                }

                                let data = &mut buffer_data[0];
                                let n_samples_avail =
                                    data.chunk().size() / (mem::size_of::<f32>() as u32);
                                let n_channels = user_data.format.channels();
                                // println!("Locking pw_format...");
                                {
                                    if let Ok(mut format_lock) = pw_format.lock() {
                                        // println!("Locked pw_format");
                                        *format_lock = AudioFormat {
                                            rate: user_data.format.rate(),
                                            chans: n_channels as u8,
                                            bits: match user_data.format.format() {
                                                SpaAudioFormat::F32BE | SpaAudioFormat::F32LE => 32,
                                                _ => unimplemented!(), // Might support these directly in the future but for now we're only
                                                                       // taking in float32le.
                                                                       // SpaAudioFormat::F64BE | SpaAudioFormat::F64LE => 64,
                                                                       // SpaAudioFormat::S16 | SpaAudioFormat::S16BE | SpaAudioFormat::S16LE | SpaAudioFormat::U16 | SpaAudioFormat::U16BE | SpaAudioFormat::U16LE => 16,
                                                                       // SpaAudioFormat::S24 | SpaAudioFormat::S24BE | SpaAudioFormat::S24LE | SpaAudioFormat::U24 | SpaAudioFormat::U24BE | SpaAudioFormat::U24LE => 24,
                                            },
                                        };
                                    }
                                }
                                // println!("Unlocked pw_format");

                                if let Some(samples) = data.data() {
                                    // println!("Found samples");
                                    // ASSUME THERE ARE AT LEAST TWO CHANNELS.
                                    // I'm not gatekeeping but audiophiles listen to at least stereo sound :)
                                    // println!("Locking buffer...");
                                    let mut locked_buffer = pw_samples.lock().unwrap();
                                    // println!("Locked buffer");
                                    for n in (0..n_samples_avail).step_by(n_channels as usize) {
                                        let l_start = n as usize * mem::size_of::<f32>();
                                        let l_end = l_start + mem::size_of::<f32>();
                                        let r_end = l_end + mem::size_of::<f32>();

                                        locked_buffer.0.push(f32::from_le_bytes(
                                            samples[l_start..l_end].try_into().unwrap(),
                                        ));
                                        locked_buffer.1.push(f32::from_le_bytes(
                                            samples[l_end..r_end].try_into().unwrap(),
                                        ));
                                    }
                                    user_data.cursor_move = true;
                                }
                                // println!("Unlocked buffer");
                            }
                        })
                        // .state_changed(|stream, user_data, state1, state2| {
                        //     println!("Steam state changed: {state1:?} -> {state2:?}");
                        // })
                        .register()
                    else {
                        let _ = fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                        return;
                    };

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

                    let Ok(_) = pw_stream.connect(
                        spa::utils::Direction::Input,
                        None,
                        pw::stream::StreamFlags::AUTOCONNECT
                            | pw::stream::StreamFlags::MAP_BUFFERS
                            | pw::stream::StreamFlags::RT_PROCESS
                            | pw::stream::StreamFlags::NO_CONVERT,
                        &mut params,
                    ) else {
                        println!("Failed to connect PipeWire stream");
                        let _ = fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                        return;
                    };
                    println!("Stream connected");
                    pw_loop.run();
                }
            ));
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
                    // println!("FFT: locking format");
                    {
                        let Ok(format_lock) = format.lock() else {
                            println!("PipeWire FFT: unable to lock format");
                            let _ =
                                fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                            return;
                        };
                        // Skip processing until format is nonzero
                        if format_lock.rate == 0 || format_lock.chans == 0 {
                            continue;
                        }
                        // Copy ringbuffer to our static ones. Take care to read backward from the latest sample.
                        if let Ok(ringbuffers) = samples.lock() {
                            for pos in 0..n_samples {
                                fft_buf_left[n_samples - pos - 1] = *ringbuffers
                                    .0
                                    .get_signed(-1 - pos as isize)
                                    .unwrap_or(&0.0_f32);
                                fft_buf_right[n_samples - pos - 1] = *ringbuffers
                                    .1
                                    .get_signed(-1 - pos as isize)
                                    .unwrap_or(&0.0_f32);
                            }
                        }
                        // These should be applied on-the-fly
                        let bin_mode =
                            if player_settings.boolean("visualizer-spectrum-use-log-bins") {
                                super::fft::BinMode::Logarithmic
                            } else {
                                super::fft::BinMode::Linear
                            };
                        let min_freq = player_settings.uint("visualizer-spectrum-min-hz") as f32;
                        let max_freq = player_settings.uint("visualizer-spectrum-max-hz") as f32;
                        let curr_step_weight =
                            player_settings.double("visualizer-spectrum-curr-step-weight") as f32;
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
                        // println!("FFT: Locking output");
                        if let Ok(mut output_lock) = output.lock() {
                            // println!("Locked output");
                            if output_lock.0.len() != n_bins || output_lock.1.len() != n_bins {
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
                                output_lock.1[i] = curr_step_right[i] * curr_step_weight * 5.0
                                    + output_lock.1[i] * (1.0 - curr_step_weight);
                            }
                            // println!("FFT L: {:?}\tR: {:?}", &output_lock.0, &output_lock.1);
                        } else {
                            println!("FFT: Failed to lock output for writing");
                            let _ =
                                fg_sender.send_blocking(PipeWireMsg::Status(FftStatus::Invalid));
                            return;
                        }
                    }
                    // println!("FFT: Unlocked output & format. Now sleeping...");
                    let fps = player_settings.uint("visualizer-fps") as f32;
                    thread::sleep(Duration::from_millis((1000.0 / fps).floor() as u64));
                }
            });
            self.fft_handle.replace(Some(fft_handle));
            self.set_status(FftStatus::Reading);

            if let Some(old_handle) =
                self.fg_handle
                    .replace(Some(glib::MainContext::default().spawn_local(clone!(
                        #[weak(rename_to = this)]
                        self,
                        async move {
                            use futures::prelude::*;
                            // Allow receiver to be mutated, but keep it at the same memory address.
                            // See Receiver::next doc for why this is needed.
                            let mut receiver = std::pin::pin!(fg_receiver);
                            let player = this.player();
                            while let Some(msg) = receiver.next().await {
                                match msg {
                                    PipeWireMsg::Status(new_status) => {
                                        player.set_fft_status(new_status);
                                    }
                                    PipeWireMsg::DevicesChanged => {
                                        this.emit_param_changed(
                                            "devices",
                                            &this.get_param("devices").unwrap(),
                                        );
                                    }
                                    PipeWireMsg::CurrentDeviceChanged => {
                                        this.emit_param_changed(
                                            "current-device",
                                            &this.get_param("current-device").unwrap(),
                                        );
                                    }
                                }
                            }
                        }
                    ))))
            {
                old_handle.abort();
            }
            Ok(())
        } else {
            Err(())
        }
    }

    fn stop(&self, block: bool) {
        let fft_stop = self.stop_flag.clone();
        fft_stop.store(true, Ordering::Relaxed);
        if let Some(sender) = self.pw_sender.take() {
            println!("Stopping PipeWire thread...");
            if sender.send(Terminate).is_ok() {
                if let Some(handle) = self.pw_handle.take() {
                    if block {
                        let _ = glib::MainContext::default().block_on(handle);
                    }
                }
                if let Some(handle) = self.fft_handle.take() {
                    if block {
                        let _ = glib::MainContext::default().block_on(handle);
                    }
                }
            }
        }
        self.set_status(FftStatus::ValidNotReading);
        self.devices.lock().unwrap().clear();
        *self.curr_device.lock().unwrap() = -1;
    }
}
