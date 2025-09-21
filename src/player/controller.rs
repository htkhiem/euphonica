extern crate mpd;
use crate::{
    application::EuphonicaApplication,
    cache::{get_image_cache_path, sqlite, Cache, CacheState},
    client::{ClientState, ConnectionState, MpdWrapper},
    common::{CoverSource, QualityGrade, Song},
    config::APPLICATION_ID,
    meta_providers::models::Lyrics,
    utils::{prettify_audio_format, settings_manager, strip_filename_linux}
};
use async_lock::OnceCell as AsyncOnceCell;
use mpris_server::{
    zbus::{self, fdo},
    LocalPlayerInterface, LocalRootInterface, LocalServer, LoopStatus, Metadata as MprisMetadata,
    PlaybackRate, PlaybackStatus as MprisPlaybackStatus, Property, Signal as MprisSignal, Time,
    TrackId, Volume,
};

use adw::subclass::prelude::*;
use glib::{clone, closure_local, subclass::Signal, BoxedAnyObject};
use gtk::gdk::{self, Texture};
use gtk::{gio, glib, prelude::*};
use mpd::{
    error::Error as MpdError, song::PosIdChange, status::{AudioFormat, State, Status}, ReplayGain, SaveMode, Subsystem
};
use lru::LruCache;
use nohash_hasher::NoHashHasher;
use std::{
    hash::BuildHasherDefault,
    cell::{Cell, OnceCell, RefCell},
    ops::Deref, path::PathBuf,
    rc::Rc, sync::{Arc, Mutex, OnceLock}, vec::Vec,

};

use super::fft_backends::{
    backend::{FftBackendExt, FftStatus}, FifoFftBackend, PipeWireFftBackend
};

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaPlaybackState")]
pub enum PlaybackState {
    #[default]
    Stopped,
    Playing,
    Paused,
}

impl From<PlaybackState> for MprisPlaybackStatus {
    fn from(ps: PlaybackState) -> Self {
        match ps {
            PlaybackState::Stopped => Self::Stopped,
            PlaybackState::Playing => Self::Playing,
            PlaybackState::Paused => Self::Paused,
        }
    }
}

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaPlaybackFlow")]
pub enum PlaybackFlow {
    #[default]
    Sequential, // Plays through the queue once
    Repeat,       // Loops through the queue
    Single,       // Plays one song then stops & waits for click to play next song
    RepeatSingle, // Loops current song
}

impl FftStatus {
    pub fn get_description(&self) -> &'static str {
        // TODO: translatable
        match self {
            Self::Invalid => "Invalid",
            Self::ValidNotReading => "Sleeping",
            Self::Stopping => "Stopping",
            Self::Reading => "Reading",
        }
    }
}

pub enum SwapDirection {
    Up,
    Down,
}

impl PlaybackFlow {
    pub fn from_status(st: &mpd::status::Status) -> Self {
        if st.repeat {
            if st.single {
                PlaybackFlow::RepeatSingle
            } else {
                PlaybackFlow::Repeat
            }
        } else {
            if st.single {
                PlaybackFlow::Single
            } else {
                PlaybackFlow::Sequential
            }
        }
    }

    pub fn icon_name(&self) -> &'static str {
        match self {
            &PlaybackFlow::Sequential => "playlist-consecutive-symbolic",
            &PlaybackFlow::Repeat => "playlist-repeat-symbolic",
            &PlaybackFlow::Single => "stop-sign-outline-symbolic",
            &PlaybackFlow::RepeatSingle => "playlist-repeat-song-symbolic",
        }
    }

    pub fn next_in_cycle(&self) -> Self {
        match self {
            &PlaybackFlow::Sequential => PlaybackFlow::Repeat,
            &PlaybackFlow::Repeat => PlaybackFlow::Single,
            &PlaybackFlow::Single => PlaybackFlow::RepeatSingle,
            &PlaybackFlow::RepeatSingle => PlaybackFlow::Sequential,
        }
    }

    // TODO: translatable
    pub fn description(&self) -> &'static str {
        match self {
            &PlaybackFlow::Sequential => "Sequential",
            &PlaybackFlow::Repeat => "Repeat Queue",
            &PlaybackFlow::Single => "Single Song",
            &PlaybackFlow::RepeatSingle => "Repeat Current Song",
        }
    }
}

impl From<PlaybackFlow> for LoopStatus {
    fn from(pf: PlaybackFlow) -> Self {
        match pf {
            PlaybackFlow::RepeatSingle => Self::Track,
            PlaybackFlow::Repeat => Self::Playlist,
            PlaybackFlow::Sequential | PlaybackFlow::Single => Self::None,
        }
    }
}

impl From<LoopStatus> for PlaybackFlow {
    fn from(ls: LoopStatus) -> Self {
        match ls {
            LoopStatus::Track => PlaybackFlow::RepeatSingle,
            LoopStatus::Playlist => PlaybackFlow::Repeat,
            LoopStatus::None => PlaybackFlow::Sequential,
        }
    }
}

fn cycle_replaygain(curr: ReplayGain) -> ReplayGain {
    match curr {
        ReplayGain::Off => ReplayGain::Auto,
        ReplayGain::Auto => ReplayGain::Track,
        ReplayGain::Track => ReplayGain::Album,
        ReplayGain::Album => ReplayGain::Off,
    }
}

fn get_replaygain_icon_name(mode: ReplayGain) -> &'static str {
    match mode {
        ReplayGain::Off => "rg-off-symbolic",
        ReplayGain::Auto => "rg-auto-symbolic",
        ReplayGain::Track => "rg-track-symbolic",
        ReplayGain::Album => "rg-album-symbolic",
    }
}

mod imp {
    use std::num::NonZero;

    use super::*;
    use crate::{application::EuphonicaApplication, common::CoverSource, meta_providers::models::Lyrics};
    use glib::{
        ParamSpec, ParamSpecBoolean, ParamSpecDouble, ParamSpecEnum, ParamSpecFloat, ParamSpecInt,
        ParamSpecString, ParamSpecUInt, ParamSpecUInt64
    };

    use once_cell::sync::Lazy;

    pub struct Player {
        pub state: Cell<PlaybackState>,
        pub position: Cell<f64>,
        pub queue: gio::ListStore,
        // Cache song infos so we can reuse them on queue updates.
        // Song IDs are u32s anyway, and I don't think there's any risk of a HashDoS attack
        // from a self-hosted music server so we'll just use identity hash for speed.
        pub song_cache: RefCell<LruCache::<u32, Song, BuildHasherDefault<NoHashHasher<u32>>>>,
        pub lyric_lines: gtk::StringList,  // Line by line for display. May be empty.
        pub lyrics: RefCell<Option<Lyrics>>,
        pub queue_len: Cell<u32>,
        pub current_song: RefCell<Option<Song>>,
        pub current_lyric_line: Cell<u32>,
        pub format: RefCell<Option<AudioFormat>>,
        pub bitrate: Cell<u32>,
        pub flow: Cell<PlaybackFlow>,
        pub random: Cell<bool>,
        pub consume: Cell<bool>,
        pub replaygain: Cell<ReplayGain>,
        pub crossfade: Cell<f64>,
        pub mixramp_db: Cell<f32>,
        pub mixramp_delay: Cell<f64>,
        // Rounded version, for sending to MPD.
        // Changes not big enough to cause an integer change
        // will not be sent to MPD.
        pub volume: Cell<i8>,
        pub client: OnceCell<Rc<MpdWrapper>>,
        // Direct reference to the cache object for fast path to
        // album arts (else we'd have to wait for signals, then
        // loop through the whole queue & search for songs matching
        // that album URI to update their arts).
        pub cache: OnceCell<Rc<Cache>>,
        // Handle to seekbar polling task
        pub poller_handle: RefCell<Option<glib::JoinHandle<()>>>,
        pub mpris_server: AsyncOnceCell<LocalServer<super::Player>>,
        pub mpris_enabled: Cell<bool>,
        pub pipewire_restart_between_songs: Cell<bool>,
        pub app: OnceCell<EuphonicaApplication>,
        pub supports_playlists: Cell<bool>,
        // For receiving frequency levels from FFT thread
        pub fft_backend: RefCell<Option<Rc<dyn FftBackendExt>>>,
        pub fft_status: Cell<FftStatus>,
        pub fft_data: Arc<Mutex<(Vec<f32>, Vec<f32>)>>, // Binned magnitudes, in stereo
        pub use_visualizer: Cell<bool>,
        pub fft_backend_idx: Cell<i32>,
        pub outputs: gio::ListStore,
        // Player controller doesn't actually keep a reference to the texture itself.
        // This enum is merely to help decide whether we should fire a notify signal
        // to the bar & pane.
        pub cover_source: Cell<CoverSource>,
        pub saved_to_history: Cell<bool>,
        pub is_foreground: Cell<bool>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Player {
        const NAME: &'static str = "EuphonicaPlayer";
        type Type = super::Player;

        fn new() -> Self {
            // 0 = fifo
            // 1 = pipewire

            let res = Self {
                state: Cell::new(PlaybackState::Stopped),
                position: Cell::new(0.0),
                lyric_lines: gtk::StringList::new(&[]),
                lyrics: RefCell::new(None),
                random: Cell::new(false),
                consume: Cell::new(false),
                supports_playlists: Cell::new(false),
                replaygain: Cell::new(ReplayGain::Off),
                crossfade: Cell::new(0.0),
                mixramp_db: Cell::new(0.0),
                mixramp_delay: Cell::new(0.0),
                queue: gio::ListStore::new::<Song>(),
                song_cache: RefCell::new(LruCache::with_hasher(NonZero::new(1024).unwrap(), BuildHasherDefault::default())),
                queue_len: Cell::new(0),
                current_song: RefCell::new(None),
                current_lyric_line: Cell::default(),
                format: RefCell::new(None),
                bitrate: Cell::default(),
                flow: Cell::default(),
                client: OnceCell::new(),
                cache: OnceCell::new(),
                volume: Cell::new(0),
                poller_handle: RefCell::new(None),
                mpris_server: AsyncOnceCell::new(),
                mpris_enabled: Cell::new(false),
                pipewire_restart_between_songs: Cell::new(false),
                app: OnceCell::new(),
                fft_backend: RefCell::new(None),
                fft_status: Cell::default(),
                fft_data: Arc::new(Mutex::new((
                    vec![
                        0.0;
                        settings_manager()
                            .child("player")
                            .uint("visualizer-spectrum-bins") as usize
                    ],
                    vec![
                        0.0;
                        settings_manager()
                            .child("player")
                            .uint("visualizer-spectrum-bins") as usize
                    ],
                ))),
                use_visualizer: Cell::new(false),
                fft_backend_idx: Cell::new(0),
                outputs: gio::ListStore::new::<BoxedAnyObject>(),
                cover_source: Cell::default(),
                saved_to_history: Cell::new(false),
                is_foreground: Cell::new(false)
            };
            res
        }
    }

    impl Default for Player {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ObjectImpl for Player {
        fn constructed(&self) {
            self.parent_constructed();
            self.fft_backend.replace(Some(self.obj().init_fft_backend()));
            let settings = settings_manager();
            settings
                .child("client")
                .bind("mpd-visualizer-pcm-source", self.obj().as_ref(), "fft-backend-idx")
                .get_only()
                .mapping(|var, _| {
                    if let Some(name) = var.get::<String>() {
                        match name.as_str() {
                            "fifo" => Some(0i32.to_value()),
                            "pipewire" => Some(1i32.to_value()),
                            _ => unimplemented!()
                        }
                    }
                    else {
                        None
                    }
                })
                .build();

            settings
                .child("ui")
                .bind("use-visualizer", self.obj().as_ref(), "use-visualizer")
                .get_only()
                .build();

            settings
                .child("client")
                .bind("pipewire-restart-between-songs", self.obj().as_ref(), "pipewire-restart-between-songs")
                .get_only()
                .build();

            self.obj().maybe_start_fft_thread();
        }

        fn dispose(&self) {
            self.obj().maybe_stop_fft_thread(true);
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecEnum::builder::<PlaybackState>("playback-state")
                        .read_only()
                        .build(),
                    ParamSpecEnum::builder::<PlaybackFlow>("playback-flow")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("replaygain").read_only().build(), // use icon name directly to simplify implementation
                    ParamSpecDouble::builder("crossfade").build(),              // seconds
                    ParamSpecFloat::builder("mixramp-db").build(),
                    ParamSpecDouble::builder("mixramp-delay").build(), // seconds
                    ParamSpecBoolean::builder("random").build(),
                    ParamSpecBoolean::builder("consume").build(),
                    ParamSpecBoolean::builder("supports-playlists").build(),
                    ParamSpecBoolean::builder("use-visualizer").build(),
                    ParamSpecDouble::builder("position").build(),
                    ParamSpecUInt::builder("current-lyric-line").read_only().build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecString::builder("album").read_only().build(),
                    ParamSpecUInt64::builder("duration").read_only().build(),
                    ParamSpecUInt::builder("queue-id").read_only().build(),
                    ParamSpecUInt::builder("queue-len").read_only().build(),  // Always available, even when queue hasn't been fetched yet
                    ParamSpecEnum::builder::<QualityGrade>("quality-grade")
                        .read_only()
                        .build(),
                    ParamSpecUInt::builder("bitrate")
                        .read_only()
                        .build(),
                    ParamSpecEnum::builder::<FftStatus>("fft-status")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("format-desc").read_only().build(),
                    ParamSpecInt::builder("fft-backend-idx").build(),
                    ParamSpecBoolean::builder("pipewire-restart-between-songs").build()
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "playback-state" => self.state.get().to_value(),
                "playback-flow" => self.flow.get().to_value(),
                "random" => self.random.get().to_value(),
                "consume" => self.consume.get().to_value(),
                "supports-playlists" => self.supports_playlists.get().to_value(),
                "use-visualizer" => self.use_visualizer.get().to_value(),
                "crossfade" => self.crossfade.get().to_value(),
                "mixramp-db" => self.mixramp_db.get().to_value(),
                "mixramp-delay" => self.mixramp_delay.get().to_value(),
                "replaygain" => get_replaygain_icon_name(self.replaygain.get()).to_value(),
                "position" => obj.position().to_value(),
                "current-lyric-line" => self.current_lyric_line.get().to_value(),
                // These are proxies for Song properties
                "title" => obj.title().to_value(),
                "artist" => obj.artist().to_value(),
                "album" => obj.album().to_value(),
                "duration" => obj.duration().to_value(),
                "queue-len" => self.queue_len.get().to_value(),
                "queue-id" => obj.queue_id().unwrap_or(u32::MAX).to_value(),
                "quality-grade" => obj.quality_grade().to_value(),
                "bitrate" => self.bitrate.get().to_value(),
                "fft-status" => obj.fft_status().to_value(),
                "format-desc" => obj.format_desc().to_value(),
                "fft-backend-idx" => self.fft_backend_idx.get().to_value(),
                "pipewire-restart-between-songs" => self.pipewire_restart_between_songs.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "crossfade" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_crossfade(v);
                    }
                }
                "mixramp-db" => {
                    if let Ok(v) = value.get::<f32>() {
                        obj.set_mixramp_db(v);
                    }
                }
                "mixramp-delay" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_mixramp_delay(v);
                    }
                }
                "position" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_position(v);
                    }
                }
                "random" => {
                    if let Ok(state) = value.get::<bool>() {
                        obj.set_random(state);
                        // Don't actually set the property here yet.
                        // Idle status will update it later.
                    }
                }
                "consume" => {
                    if let Ok(state) = value.get::<bool>() {
                        obj.set_consume(state);
                        // Don't actually set the property here yet.
                        // Idle status will update it later.
                    }
                }
                "supports-playlists" => {
                    if let Ok(state) = value.get::<bool>() {
                        self.supports_playlists.replace(state);
                        obj.notify("supports-playlists");
                    }
                }
                "use-visualizer" => {
                    if let Ok(state) = value.get::<bool>() {
                        self.use_visualizer.replace(state);
                        obj.notify("use-visualizer");

                        if state {
                            // Visualiser turned on. Start FFT thread.
                            self.obj().maybe_start_fft_thread();
                        } else {
                            // Visualiser turned off. FFT thread should
                            // have stopped by itself. Join & yeet handle.
                            self.obj().maybe_stop_fft_thread(false);
                        }
                    }
                }
                "fft-backend-idx" => {
                    if let Ok(new) = value.get::<i32>() {
                        let old = self.fft_backend_idx.replace(new);

                        if old != new {
                            println!("Switching FFT backend...");
                            self.obj().maybe_stop_fft_thread(true);
                            self.fft_backend.replace(Some(self.obj().init_fft_backend()));
                            self.obj().maybe_start_fft_thread();
                            self.obj().notify("fft-backend-idx");
                        }
                    }
                }
                "pipewire-restart-between-songs" => {
                    if let Ok(state) = value.get::<bool>() {
                        let old = self.pipewire_restart_between_songs.replace(state);
                        if old != state {
                            self.obj().notify("pipewire-restart-between-songs");
                        }
                    }
                }
                _ => unimplemented!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("outputs-changed")
                        .build(),
                    // Reserved for EXTERNAL changes (i.e. changes made by this client won't
                    // emit this).
                    Signal::builder("volume-changed")
                        .param_types([i8::static_type()])
                        .build(),
                    Signal::builder("history-changed")
                        .build(),
                    // For simplicity we'll always use the hires version
                    Signal::builder("cover-changed")
                        .param_types([Option::<gdk::Texture>::static_type()])
                        .build(),
                    Signal::builder("fft-param-changed")
                        .param_types([String::static_type(), String::static_type(), glib::Variant::static_type()])
                        .build()
                ]
            })
        }
    }
}

glib::wrapper! {
    pub struct Player(ObjectSubclass<imp::Player>);
}

impl Default for Player {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Player {
    fn init_fft_backend(&self) -> Rc<dyn FftBackendExt> {
        let client_settings = settings_manager().child("client");
        match client_settings.enum_("mpd-visualizer-pcm-source") {
            0 => Rc::new(FifoFftBackend::new(self.clone())),
            1 => Rc::new(PipeWireFftBackend::new(self.clone())),
            _ => unimplemented!(),
        }
    }

    /// If a backend name is specified, will only get the parameter from that backend. If that
    /// backend is not the currently-active one, returns None.
    /// If no backend name is specified, will try to fetch the parameter from the currently-active backend.
    /// This is useful for universal parameters shared by all backends, though there aren't any (yet).
    pub fn get_fft_param(&self, backend_name: Option<&str>, key: &str) -> Option<glib::Variant> {
        if let Some(backend) = self.imp().fft_backend.borrow().as_ref() {
            if backend_name.is_some_and(|name| backend.name() == name) || backend_name.is_none() {
                backend.get_param(key)
            } else {
                None
            }
        } else {
            None
        }
    }

    /// If a backend name is specified, will only set the parameter for that backend. If that
    /// backend is not the currently-active one, this is a noop.
    /// If no backend name is specified, will try to set the parameter for the currently-active backend.
    /// This is useful for universal parameters shared by all backends, though there aren't any (yet).
    pub fn set_fft_param(&self, backend_name: Option<&str>, key: &str, val: glib::Variant) {
        if let Some(backend) = self.imp().fft_backend.borrow().as_ref() {
            if backend_name.is_some_and(|name| backend.name() == name) || backend_name.is_none() {
                backend.set_param(key, val);
            }
        }
    }

    /// Lazily get an MPRIS server. This will always be invoked near the start anyway
    /// by the initial call to update_status().
    async fn get_mpris(&self) -> zbus::Result<&LocalServer<Self>> {
        self.imp()
            .mpris_server
            .get_or_try_init(|| async {
                let server = LocalServer::new("io.github.htkhiem.Euphonica", self.clone()).await?;
                glib::spawn_future_local(server.run());
                Ok(server)
            })
            .await
    }

    pub fn is_foreground(&self) -> bool {
        self.imp().is_foreground.get()
    }

    pub fn set_is_foreground(&self, mode: bool) {
        self.imp().is_foreground.set(mode);
        // If running in foreground mode, maybe start FFT thread and seekbar polling.
        if mode {
            println!("Player controller: entering foreground mode");
            // Don't block polling: some shells' MPRIS applets have seekbars
            // self.unblock_polling();
            // self.maybe_start_polling();
            self.maybe_start_fft_thread();
        }
        else {
            println!("Player controller: entering background mode");
            // self.block_polling();
            // self.stop_polling();
            self.maybe_stop_fft_thread(true);
        }
    }

    // Start a thread to read raw PCM data from MPD's named pipe output, transform them
    // to the frequency domain, then return the frequency magnitudes.
    // On each FFT frame (not screen frame):
    // 1. Read app preferences.
    //    - If visualiser is disabled or stop flag is true, then stop this thread.
    //    - Else, read the specified number of samples from the named pipe.
    //      This may have changed from the last frame by the user.
    //    - Get the number of frequencies set by the user. Again this can be changed on-the-fly.
    // 2. Perform FFT & extrapolate to the marker frequencies.
    // 3. Send results back to main thread via the async channel.
    fn maybe_start_fft_thread(&self) {
        if self.imp().use_visualizer.get() && self.imp().is_foreground.get() {
            let output = self.imp().fft_data.clone();
            if let Some(backend) = self.imp().fft_backend.borrow().as_ref() {
                let _ = backend.clone().start(output);
            }
        }
    }

    fn maybe_stop_fft_thread(&self, block: bool) {
        if let Some(backend) = self.imp().fft_backend.borrow().as_ref() {
            backend.stop(block);
        }
    }

    pub fn restart_fft_thread(&self) {
        self.maybe_stop_fft_thread(true);
        self.maybe_start_fft_thread();
    }

    pub fn fft_data(&self) -> Arc<Mutex<(Vec<f32>, Vec<f32>)>> {
        self.imp().fft_data.clone()
    }

    pub fn outputs(&self) -> gio::ListStore {
        self.imp().outputs.clone()
    }

    pub fn clear(&self) {
        self.imp().queue.remove_all();
        self.imp().song_cache.borrow_mut().clear();
        self.imp().outputs.remove_all();
        self.update_status(&mpd::Status::default());
    }

    pub fn populate(&self) {
        // Don't fetch queue until Queue View is navigated to.
        if let Some(status) = self.client().get_status(false) {
            self.update_status(&status);
        }
        if let Some(outputs) = self.client().get_outputs() {
            self.update_outputs(outputs);
        }
    }

    pub fn setup(
        &self,
        application: EuphonicaApplication,
        client: Rc<MpdWrapper>,
        cache: Rc<Cache>,
    ) {
        let client_state = client.clone().get_client_state();
        let _ = self.imp().client.set(client);

        cache.get_cache_state().connect_closure(
            "album-art-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, uri: String, thumb: bool, tex: gdk::Texture| {
                    // Update logic:
                    // - Match by full URI first, then
                    // - Match by folder URI only if there is no current cover.
                    if thumb {return;}
                    if let Some(song) = this.imp().current_song.borrow().as_ref() {
                        if song.get_uri() == &uri {
                            // Always do this to force upgrade to embedded cover from folder cover
                            this.imp().cover_source.set(CoverSource::Embedded);
                            this.emit_by_name::<()>("cover-changed", &[&Some(tex)]);
                        } else if this.imp().cover_source.get() != CoverSource::Embedded {
                            if strip_filename_linux(song.get_uri()) == &uri {
                                this.imp().cover_source.set(CoverSource::Folder);
                                this.emit_by_name::<()>("cover-changed", &[&Some(tex)]);
                            }
                        }
                    }
                }
            ),
        );
        // Same logic should also work for clearing
        cache.get_cache_state().connect_closure(
            "album-art-cleared",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, uri: String| {
                    if let Some(song) = this.imp().current_song.borrow().as_ref() {
                        match this.imp().cover_source.get() {
                            CoverSource::Embedded => {
                                if song.get_uri() == &uri {
                                    this.imp().cover_source.set(CoverSource::None);
                                    this.emit_by_name::<()>("cover-changed", &[&Option::<gdk::Texture>::None]);
                                }
                            }
                            CoverSource::Folder => {
                                if strip_filename_linux(song.get_uri()) == &uri {
                                    this.imp().cover_source.set(CoverSource::None);
                                    this.emit_by_name::<()>("cover-changed", &[&Option::<gdk::Texture>::None]);
                                }
                            }
                            _ => {}
                        }
                    }
                }
            ),
        );
        cache.get_cache_state().connect_closure(
            "song-lyrics-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, uri: String| {
                    if let Some(song) = this.imp().current_song.borrow().as_ref() {
                        if song.get_uri() == uri.as_str() {
                            this.update_lyrics(this.imp().cache.get().unwrap().load_cached_lyrics(song.get_info()).unwrap());
                        }
                    }
                }
            ),
        );

        let _ = self.imp().cache.set(cache);
        let _ = self.imp().app.set(application);
        // Connect to ClientState signals
        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |state, _| {
                    match state.get_connection_state() {
                        ConnectionState::Connected => {
                            // Newly-connected? Get initial status.
                            this.populate();
                        }
                        ConnectionState::Connecting => {
                            this.clear();
                        }
                        _ => {}
                    }
                }
            ),
        );

        client_state
            .bind_property("supports-playlists", self, "supports-playlists")
            .sync_create()
            .build();

        client_state.connect_closure(
            "idle",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, subsys: glib::BoxedAnyObject| {
                    match subsys.borrow::<Subsystem>().deref() {
                        Subsystem::Player | Subsystem::Options => {
                            if let Some(status) = this.client().get_status(false) {
                                this.update_status(&status);
                            }
                        }
                        Subsystem::Queue => {
                            if let Some(status) = this.client().get_status(true) {
                                this.update_status(&status);
                            }
                        }
                        Subsystem::Output => {
                            if let Some(outs) = this.client().get_outputs() {
                                this.update_outputs(outs);
                            }
                        }
                        Subsystem::Mixer => {
                            if let Some(vol) = this.client().get_volume() {
                                this.emit_by_name::<()>("volume-changed", &[&vol]);
                            }
                        }
                        _ => {}
                    }
                }
            ),
        );

        // Only used at init time
        client_state.connect_closure(
            "queue-songs-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, songs: glib::BoxedAnyObject| {
                    let songs = songs.borrow::<Vec<Song>>();
                    this.imp().queue.extend_from_slice(&songs);
                    let mut song_cache = this.imp().song_cache.borrow_mut();
                    for song in songs.iter() {
                        song_cache.push(song.get_queue_id(), song.clone());
                    }
                }
            )
        );

        client_state.connect_closure(
            "queue-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, changes: BoxedAnyObject| {
                    this.update_queue(changes.borrow::<Vec<PosIdChange>>().as_ref());
                }
            ),
        );

        let settings = settings_manager().child("player");
        let _ = self
            .imp()
            .mpris_enabled
            .replace(settings.boolean("enable-mpris"));
        settings.connect_changed(
            Some("enable-mpris"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |settings, _| {
                    let new_state = settings.boolean("enable-mpris");
                    let _ = this.imp().mpris_enabled.replace(new_state);
                    if !new_state {
                        // Ping once to clear existing controls
                        this.update_mpris_properties(vec![Property::Metadata(
                            MprisMetadata::default(),
                        )]);
                    }
                }
            ),
        );
    }

    fn update_mpris_properties(&self, properties: Vec<Property>) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                match this.get_mpris().await {
                    Ok(mpris) => {
                        if let Err(err) = mpris.properties_changed(properties).await {
                            println!("{:?}", err);
                        }
                    }
                    Err(err) => {
                        println!("No MPRIS server: {:?}", err);
                    }
                }
            }
        ));
    }

    fn seek_mpris(&self, position: f64) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                match this.get_mpris().await {
                    Ok(mpris) => {
                        let pos_time = Time::from_millis((position * 1000.0).round() as i64);
                        if let Err(err) =
                            mpris.emit(MprisSignal::Seeked { position: pos_time }).await
                        {
                            println!("{:?}", err);
                        }
                    }
                    Err(err) => {
                        println!("No MPRIS server: {:?}", err);
                    }
                }
            }
        ));
    }

    // Update functions
    // These all have side-effects of notifying listeners of changes to the
    // GObject properties, which in turn are read from this struct's fields.
    // Signals will be sent for properties whose values have changed, even though
    // we will be receiving updates for many properties at once.

    /// Main update function. MPD's protocol has a single "status" commands
    /// that returns everything at once. This update function will take what's
    /// relevant and update the GObject properties accordingly.
    pub fn update_status(&self, status: &Status) {
        let mut mpris_changes: Vec<Property> = Vec::new();
        match status.state {
            State::Play => {
                let new_state = PlaybackState::Playing;
                let old_state = self.imp().state.replace(new_state);
                self.maybe_start_polling();
                if old_state != new_state {
                    self.notify("playback-state");
                    if self.imp().mpris_enabled.get() {
                        mpris_changes.push(Property::PlaybackStatus(MprisPlaybackStatus::Playing));
                    }
                }
            }
            State::Pause => {
                let new_state = PlaybackState::Paused;
                let old_state = self.imp().state.replace(new_state);
                self.stop_polling();
                if old_state != new_state {
                    self.notify("playback-state");
                    if self.imp().mpris_enabled.get() {
                        mpris_changes.push(Property::PlaybackStatus(MprisPlaybackStatus::Paused));
                    }
                }
            }
            State::Stop => {
                let new_state = PlaybackState::Stopped;
                let old_state = self.imp().state.replace(new_state);
                self.stop_polling();
                if old_state != new_state {
                    self.notify("playback-state");
                    if self.imp().mpris_enabled.get() {
                        mpris_changes.push(Property::PlaybackStatus(MprisPlaybackStatus::Stopped));
                    }
                }
            }
        };

        let new_rg = status.replaygain.unwrap_or(ReplayGain::Off);
        let old_rg = self.imp().replaygain.replace(new_rg);
        if old_rg != new_rg {
            // These properties are affected by the "state" field.
            self.notify("replaygain");
        }

        let new_flow = PlaybackFlow::from_status(status);
        let old_flow = self.imp().flow.replace(new_flow);
        if old_flow != new_flow {
            self.notify("playback-flow");
            if self.imp().mpris_enabled.get() {
                mpris_changes.push(Property::LoopStatus(new_flow.into()));
            }
        }

        let old_rand = self.imp().random.replace(status.random);
        if old_rand != status.random {
            self.notify("random");
            if self.imp().mpris_enabled.get() {
                mpris_changes.push(Property::Shuffle(status.random));
            }
        }

        let old_consume = self.imp().consume.replace(status.consume);
        if old_consume != status.consume {
            self.notify("consume");
        }

        let old_format = self.imp().format.replace(status.audio);
        if old_format != status.audio {
            self.notify("format-desc");
        }

        let new_bitrate = status.bitrate.unwrap_or(0);
        let old_bitrate = self.imp().bitrate.replace(new_bitrate);
        if new_bitrate != old_bitrate {
            self.notify("bitrate");
        }

        let old_mixramp_db = self.imp().mixramp_db.replace(status.mixrampdb);
        if old_mixramp_db != status.mixrampdb {
            self.notify("mixramp-db");
        }

        let new_mixramp_delay: f64;
        if let Some(dur) = status.mixrampdelay {
            new_mixramp_delay = dur.as_secs_f64();
        } else {
            new_mixramp_delay = 0.0;
        }
        let old_mixramp_delay = self.imp().mixramp_delay.replace(new_mixramp_delay);
        if old_mixramp_delay != new_mixramp_delay {
            self.notify("mixramp-delay");
        }

        let new_crossfade: f64;
        if let Some(dur) = status.crossfade {
            new_crossfade = dur.as_secs_f64();
        } else {
            new_crossfade = 0.0;
        }
        let old_crossfade = self.imp().crossfade.replace(new_crossfade);
        if old_crossfade != new_crossfade {
            self.notify("crossfade");
        }

        // Handle volume changes (might be external)
        // TODO: Find a way to somewhat responsively update volume to external
        // changes at all times rather than relying on the seekbar poller.
        let new_vol = status.volume;
        let old_vol = self.imp().volume.replace(new_vol);
        if old_vol != new_vol {
            self.emit_by_name::<()>("volume-changed", &[&new_vol]);
            if self.imp().mpris_enabled.get() {
                mpris_changes.push(Property::Volume(new_vol as f64 / 100.0));
            }
        }

        // Update playing status of songs in the queue
        if let Some(new_queue_place) = status.song {
            let mut needs_refresh: bool = false;
            {
                // There is now a playing song. Fetch if we haven't already.
                let mut local_curr_song = self
                    .imp()
                    .current_song
                    .borrow_mut();

                if local_curr_song.as_ref().is_none_or(|song| song.get_queue_id() != new_queue_place.id.0) {
                    needs_refresh = true;
                    if let Some(new_song) = self.client().get_song_at_queue_id(new_queue_place.id.0) {
                        // Always fetch as the queue might not have been populated yet
                        local_curr_song.replace(new_song.clone());
                        // If using PipeWire visualiser, might need to restart it
                        if self.imp().pipewire_restart_between_songs.get()
                            && self.imp().fft_backend.borrow().as_ref().is_some_and(
                                |backend| backend.name() == "pipewire"
                            )
                        {
                            println!("Starting PipeWire backend again after song change...");
                            self.maybe_start_fft_thread();
                        }
                    }
                } else if let Some(curr_song) = local_curr_song.as_ref() {
                    // Same old song. Might want to record into playback history.
                    if !settings_manager().child("library").boolean("pause-recent") {
                        let dur = curr_song.get_duration() as f32;
                        if dur >= 10.0 {
                            if let Some(new_position_dur) = status.elapsed {
                                if !self.imp().saved_to_history.get() && new_position_dur.as_secs_f32() / dur >= 0.5 {
                                    if let Ok(()) = sqlite::add_to_history(curr_song.get_info()) {
                                        self.emit_by_name::<()>("history-changed", &[]);
                                    }
                                    self.imp().saved_to_history.set(true);
                                }
                            }
                        }
                    }
                }
            }
            if needs_refresh {
                if let Some(new_song) = self.imp().current_song.borrow().as_ref() {
                    self.imp().saved_to_history.set(false);
                    self.notify("title");
                    self.notify("artist");
                    self.notify("duration");
                    self.notify("quality-grade");
                    self.notify("format-desc");
                    self.notify("album");
                    self.notify("queue-id");
                    // Get album art. Start with CoverSource::Unknown.
                    // We might also get an asynchronous reply later via a cache state signal.
                    if let Some((tex, is_fallback)) = self
                        .imp()
                        .cache
                        .get()
                        .unwrap()
                        .clone()
                        .load_cached_embedded_cover(new_song.get_info(), false, true)
                    {
                        self.imp().cover_source.set(if is_fallback {CoverSource::Folder} else {CoverSource::Embedded});
                        self.emit_by_name::<()>("cover-changed", &[&Some(tex)]);
                    }
                    else {
                        self.imp().cover_source.set(CoverSource::Unknown);
                        self.emit_by_name::<()>("cover-changed", &[&Option::<gdk::Texture>::None]);
                    }
                    // Get new lyrics
                    // First remove all current lines
                    self.imp().lyric_lines.splice(0, self.imp().lyric_lines.n_items(), &[]);
                    let _ = self.imp().lyrics.take();
                    // Fetch new lyrics
                    if let Some(lyrics) = self.imp().cache.get().unwrap().load_cached_lyrics(new_song.get_info()) {
                        self.update_lyrics(lyrics);
                    }
                    else {
                        // Schedule downloading
                        self.imp().cache.get().unwrap().ensure_cached_lyrics(new_song.get_info());
                    }
                    // Update MPRIS side
                    if self.imp().mpris_enabled.get() {
                        mpris_changes.push(Property::Metadata(
                            new_song.get_mpris_metadata(),
                        ));
                    }
                }
            }
        }
        // status responses after a "stop" command will still come with the ID of the last-played
        // song, which is not what we want.
        if status.song.is_none() || status.state == State::Stop {
            println!("No song playing right now");
            // No song is playing. Update state accordingly.
            if let Some(_) = self.imp().current_song.take() {
                self.imp().saved_to_history.set(false);
                self.notify("title");
                self.notify("artist");
                self.notify("album");
                self.notify("duration");
                self.notify("queue-id");
                self.imp().cover_source.set(CoverSource::Unknown);
                self.emit_by_name::<()>("cover-changed", &[&Option::<gdk::Texture>::None]);
                // Update MPRIS side
                if self.imp().mpris_enabled.get() {
                    mpris_changes.push(Property::Metadata(
                        MprisMetadata::builder().trackid(TrackId::NO_TRACK).build(),
                    ));
                }
            }
        }

        if let Some(new_position_dur) = status.elapsed {
            let new = new_position_dur.as_secs_f64();
            let old = self.set_position(new);
            if new != old && self.imp().mpris_enabled.get() {
                self.seek_mpris(new);
            }
            // If using PipeWire visualiser and auto-restart is enabled, stop the thread
            // just before song ends. As we poll once every second, we can't use a threshold
            // shorter than 1s.
            let secs_to_end = self.duration() as f64 - new;
            if self.imp().pipewire_restart_between_songs.get()
                && self.imp().fft_backend.borrow().as_ref().is_some_and(
                    |backend| backend.name() == "pipewire" && backend.status() != FftStatus::ValidNotReading
                )
                && secs_to_end >= 0.0 && secs_to_end < 1.5
            {
                println!("Stopping PipeWire backend to allow samplerate change...");
                self.maybe_stop_fft_thread(false); // FIXME: we can't block while runnin in an async loop
            }
        } else {
            self.set_position(0.0);
        }
        if let Some(lyrics) = self.imp().lyrics.borrow().as_ref() {
            let new_idx = lyrics.get_line_at_timestamp(self.imp().position.get() as f32) as u32;
            let old_idx = self.imp().current_lyric_line.replace(new_idx);
            if new_idx != old_idx {
                self.notify("current-lyric-line");
            }
        }

        // We need to separately keep track of queue length here as the queue list model might
        // not have been initialised yet.
        let new_len = status.queue_len;
        let old_len = self.imp().queue_len.replace(new_len);
        if old_len != new_len {
            self.notify("queue-len");
        }
        // If new queue is shorter, truncate current queue.
        // This is because update_queue would be called before update_status, which means
        // the new length was not available to update_queue.
        let old_len = self.imp().queue.n_items();
        if old_len > new_len {
            self.imp()
                .queue
                .splice(new_len, old_len - new_len, &[] as &[Song; 0]);
        }
        if self.imp().mpris_enabled.get() {
            self.update_mpris_properties(mpris_changes);
        }
    }

    pub fn update_lyrics(&self, lyrics: Lyrics) {
        self.imp().current_lyric_line.set(0);
        self.imp().lyric_lines.splice(0, 0, &lyrics.to_plain_lines());
        self.imp().lyrics.replace(Some(lyrics));
        self.notify("current-lyric-line");
    }

    /// Returns true if we have lyrics for the current song and it is synced; false otherwise.
    pub fn lyrics_are_synced(&self) -> bool {
        if let Some(lyrics) = self.imp().lyrics.borrow().as_ref() {
            lyrics.synced
        }
        else {
            false
        }
    }

    pub fn current_lyric_line(&self) -> u32 {
        self.imp().current_lyric_line.get()
    }

    pub fn n_lyric_lines(&self) -> u32 {
        self.imp().lyric_lines.n_items()
    }

    /// Update the queue, optionally with diffs or an entirely new queue.
    ///
    /// If replace=True, simply yeet the whole old queue. Only replace when you are giving this
    /// function ALL the songs in the new queue version. If you only have a diff, use replace = false
    /// for correct diff resolution.
    ///
    /// This function cannot detect song removals at the end of the queue since it is always called
    /// before update_status() (by MPD's idle change notifier) and as such has no way to know the
    /// new queue length. The update_status() function will instead truncate the queue to the new
    /// length for us once called.
    /// If an MPRIS server is running, it will also emit property change signals.
    pub fn update_queue(&self, changes: &[PosIdChange]) {
        let queue = &self.imp().queue;
        if changes.len() > 0 {
            // Find queue range covered by the changes vec
            let mut max_pos: u32 = 0;
            let mut min_pos: u32 = u32::MAX;
            for change in changes.iter() {
                if change.pos < min_pos {
                    min_pos = change.pos;
                }
                if change.pos > max_pos {
                    max_pos = change.pos;
                }
            }

            // Reconstruct the queue within that range.
            let mut new_segment: Vec<glib::Object> = Vec::with_capacity((max_pos - min_pos + 1) as usize);
            let mut song_cache = self.imp().song_cache.borrow_mut();
            let mut change_idx: usize = 0;
            for pos in min_pos..=max_pos {
                // If this position did not change, then simply use the current GObject.
                // This only happens within the length of the current queue. Entries past its
                // length will be included in the changes vec.
                if changes[change_idx].pos != pos {
                    if let Some(old_song) = queue.item(pos as u32) {
                        new_segment.push(old_song);
                    } else {
                        // Exceeded current queue (new queue is longer)
                        panic!("New queue is longer than current queue, but no corresponding diff info was received");
                    }
                } else {
                    // This position changed. Check if it's a song we already have locally.
                    let id = changes[change_idx].id.0;
                    if let Some(existing) = song_cache.get(&id) {
                        new_segment.push(existing.clone().into());
                    } else {
                        println!("update_queue(): Song cache miss");
                        // New song. Fetch info.
                        // On very slow connections this might be called after the queue has changed
                        // once more, potentially removing the song with this ID from the server-side
                        // queue. In that case, push a default Song GObject as padding. More update
                        // calls are guaranteed to be triggered and will fix this.
                        let song = self.client()
                                .get_song_at_queue_id(id)
                                .unwrap_or_default();
                        song_cache.push(id, song.clone());
                        new_segment.push(song.into());
                    }
                    change_idx += 1;
                }
            }
            if queue.n_items() > 0 && min_pos < queue.n_items() {
                // Overwrite current queue with the above updated segment
                queue.splice(
                    min_pos,
                    max_pos.min(queue.n_items() - 1) - min_pos + 1,
                    &new_segment
                );
            } else {
                queue.extend_from_slice(&new_segment);
            }
        }
    }

    pub fn client(&self) -> &Rc<MpdWrapper> {
        self.imp().client.get().unwrap()
    }

    fn update_outputs(&self, outputs: Vec<mpd::Output>) {
        self.imp().outputs.remove_all();
        self.imp().outputs.extend_from_slice(
            &outputs.into_iter().map(glib::BoxedAnyObject::new).collect::<Vec<glib::BoxedAnyObject>>()
        );
        self.emit_by_name::<()>("outputs-changed", &[]);
    }

    pub fn set_output(&self, id: u32, state: bool) {
        self.client().set_output(id, state);
    }

    // Here we try to define getters and setters in terms of the GObject
    // properties as defined above in mod imp {} instead of the actual
    // internal fields.
    pub fn cycle_playback_flow(&self) {
        let next_flow = self.imp().flow.get().next_in_cycle();
        self.client().set_playback_flow(next_flow);
    }

    pub fn cycle_replaygain(&self) {
        let next_rg = cycle_replaygain(self.imp().replaygain.get());
        self.client().set_replaygain(next_rg);
    }

    pub fn set_random(&self, new: bool) {
        self.client().set_random(new);
    }

    pub fn set_consume(&self, new: bool) {
        self.client().set_consume(new);
    }

    pub fn set_crossfade(&self, new: f64) {
        self.client().set_crossfade(new);
    }

    pub fn set_mixramp_db(&self, new: f32) {
        self.client().set_mixramp_db(new);
    }

    pub fn set_mixramp_delay(&self, new: f64) {
        self.client().set_mixramp_delay(new);
    }

    pub fn title(&self) -> Option<String> {
        if let Some(song) = &*self.imp().current_song.borrow() {
            return Some(song.get_name().to_owned());
        }
        None
    }

    pub fn artist(&self) -> Option<String> {
        if let Some(song) = &*self.imp().current_song.borrow() {
            return song.get_artist_str();
        }
        None
    }

    pub fn album(&self) -> Option<String> {
        if let Some(song) = &*self.imp().current_song.borrow() {
            if let Some(album) = song.get_album() {
                return Some(album.title.clone());
            }
            return None;
        }
        None
    }

    pub fn current_song_cover(&self) -> Option<Texture> {
        if let Some(cache) = self.imp().cache.get() {
            if let Some(song) = self.imp().current_song.borrow().as_ref() {
                // Do not schedule again (already done once in update_status)
                return cache.clone().load_cached_embedded_cover(song.get_info(), false, false).map(|pair| pair.0);
            }
            return None;
        }
        None
    }

    pub fn current_song_cover_path(&self, thumbnail: bool) -> Option<PathBuf> {
        if let Some(song) = self.imp().current_song.borrow().as_ref() {
            let mut path = get_image_cache_path();
            if let Some(filename) = sqlite::find_cover_by_uri(&song.get_uri(), thumbnail)
                .expect("Sqlite DB error")
                .map_or(None, |name| if name.len() > 0 {Some(name)} else {None})
            {
                // Will fall back to folder level cover if there is no embedded art
                path.push(filename);
                Some(path)
            }
            else {
                None
            }
        } else {
            None
        }
    }

    pub fn quality_grade(&self) -> QualityGrade {
        if let Some(song) = &*self.imp().current_song.borrow() {
            return song.get_quality_grade();
        }
        QualityGrade::Unknown
    }

    pub fn fft_status(&self) -> FftStatus {
        self.imp().fft_status.get()
    }

    pub fn set_fft_status(&self, new: FftStatus) {
        let old = self.imp().fft_status.replace(new);
        if old != new {
            self.notify("fft-status");
        }
    }

    pub fn format_desc(&self) -> Option<String> {
        if let Some(format) = &*self.imp().format.borrow() {
            return Some(prettify_audio_format(format));
        }
        None
    }

    pub fn duration(&self) -> u64 {
        if let Some(song) = &*self.imp().current_song.borrow() {
            return song.get_duration(); // Can still be 0
        }
        0
    }

    pub fn mpd_volume(&self) -> i8 {
        self.imp().volume.get()
    }

    pub fn queue_id(&self) -> Option<u32> {
        self.imp().current_song.borrow().as_ref().map(|s| s.get_queue_id())
    }

    pub fn queue_pos(&self) -> Option<u32> {
        self.imp().current_song.borrow().as_ref().map(|s| s.get_queue_pos())
    }

    pub fn position(&self) -> f64 {
        self.imp().position.get()
    }

    /// Set new position. Only sets the property (does not send a seek command to MPD yet).
    /// Returns the old position.
    /// To apply this new position, call seek().
    pub fn set_position(&self, new: f64) -> f64 {
        let old = self.imp().position.replace(new);
        if new != old {
            self.notify("position");
        }
        old
    }

    /// Seek to current position. Called when the seekbar is released.
    pub fn send_seek(&self, new_pos: f64) {
        self.client().seek_current_song(new_pos);
    }

    /// Seek to the timestamp of a lyric line
    pub fn seek_to_lyric_line(&self, line: i32) {
        if let Some(lyrics) = self.imp().lyrics.borrow().as_ref() {
            if lyrics.synced && line >= 0 && line < lyrics.lines.len() as i32 {
                self.client().seek_current_song(lyrics.lines[line as usize].0 as f64);
            }
        }
    }

    pub fn queue(&self) -> &gio::ListStore {
        &self.imp().queue
    }

    pub fn lyrics(&self) -> gtk::StringList {
        self.imp().lyric_lines.clone()
    }

    fn send_play(&self) {
        self.client().pause(false);
    }

    fn send_pause(&self) {
        self.client().pause(true);
    }

    pub fn toggle_playback(&self) {
        // If state is stopped, there won't be a "current song".
        // To start playing, instead of using the "pause" command to toggle,
        // we need to explicitly tell MPD to start playing the first song in
        // the queue.

        match self.imp().state.get() {
            PlaybackState::Stopped => {
                // Check if queue is not empty
                if self.queue().n_items() > 0 {
                    // Start playing first song in queue.
                    self.client().play_at(0, false);
                } else {
                    println!("Queue is empty; nothing to play");
                }
            }
            PlaybackState::Playing => {
                self.send_pause();
            }
            PlaybackState::Paused => {
                self.send_play();
            }
        }
    }

    pub fn prev_song(&self, block: bool) {
        if self.imp().pipewire_restart_between_songs.get()
            && self.imp().fft_backend.borrow().as_ref().is_some_and(
                |backend| backend.name() == "pipewire" && backend.status() != FftStatus::ValidNotReading
            )
        {
            println!("Stopping PipeWire backend to allow samplerate change...");
            self.maybe_stop_fft_thread(block);
        }
        self.client().prev();
    }

    pub fn next_song(&self, block: bool) {
        if self.imp().pipewire_restart_between_songs.get()
            && self.imp().fft_backend.borrow().as_ref().is_some_and(
                |backend| backend.name() == "pipewire" && backend.status() != FftStatus::ValidNotReading
            )
        {
            println!("Stopping PipeWire backend to allow samplerate change...");
            self.maybe_stop_fft_thread(block);
        }
        self.client().next();
    }

    pub fn clear_queue(&self) {
        self.client().clear_queue();
    }

    pub fn send_set_volume(&self, val: i8) {
        let old_vol = self.imp().volume.replace(val);
        if old_vol != val {
            self.client().volume(val);
        }
    }

    pub fn on_song_clicked(&self, song: Song) {
        if self.imp().pipewire_restart_between_songs.get()
            && self.imp().fft_backend.borrow().as_ref().is_some_and(
                |backend| backend.name() == "pipewire" && backend.status() != FftStatus::ValidNotReading
            )
        {
            println!("Stopping PipeWire backend to allow samplerate change...");
            self.maybe_stop_fft_thread(true);
        }
        self.client().play_at(song.get_queue_id(), true);
    }

    /// Remove given song from queue.
    pub fn remove_pos(&self, pos: u32) {
        self.client().register_local_queue_changes(1);
        self.queue().remove(pos);
        self.client().delete_at(pos, false);
    }

    pub fn swap_dir(&self, pos: u32, direction: SwapDirection) {
        self.client().register_local_queue_changes(1);
        let target = self.imp().queue.item(pos).and_downcast::<Song>().unwrap();
        match direction {
            SwapDirection::Up => {
                if pos > 0 {
                    let upper = self.imp().queue.item(pos - 1).and_downcast::<Song>().unwrap();
                    self.imp().queue.splice(pos - 1, 2, &[
                        target.clone().upcast::<glib::Object>(),
                        upper.upcast::<glib::Object>()
                    ]);
                    self.client().swap(pos, pos - 1, false);
                }
            }
            SwapDirection::Down => {
                if pos < self.imp().queue.n_items() - 1 {
                    let lower = self.imp().queue.item(pos + 1).and_downcast::<Song>().unwrap();
                    self.imp().queue.splice(pos, 2, &[
                        lower.upcast::<glib::Object>(),
                        target.clone().upcast::<glib::Object>()
                    ]);
                    self.client().swap(pos, pos + 1, false);
                }
            }
        }
    }

    pub fn save_queue(&self, name: &str, save_mode: SaveMode) -> Result<(), Option<MpdError>> {
        return self.client().save_queue_as_playlist(name, save_mode);
    }

    /// Periodically poll for player progress to update seekbar.
    /// Won't start a new loop if there is already one or when polling is blocked by a seekbar.
    pub fn maybe_start_polling(&self) {
        let this = self.clone();
        let client = self.client().clone();
        if self.imp().poller_handle.borrow().is_none() {
            let poller_handle = glib::MainContext::default().spawn_local(async move {
                loop {
                    // Don't poll if not playing
                    if this.imp().state.get() == PlaybackState::Playing {
                        if let Some(status) = client.clone().get_status(false) {
                            this.update_status(&status);
                        }
                    }
                    glib::timeout_future_seconds(1).await;
                }
            });
            self.imp().poller_handle.replace(Some(poller_handle));
        }
    }

    /// Stop poller loop. Seekbar should call this when being interacted with.
    pub fn stop_polling(&self) {
        if let Some(handle) = self.imp().poller_handle.take() {
            handle.abort();
        }
    }

    pub fn export_lyrics(&self) -> Option<String> {
        self.imp().lyrics.borrow().as_ref().map(|lyrics| lyrics.to_string())
    }

    pub fn import_lyrics(&self, text: &str) {
        if let Some(curr_song) = self.imp().current_song.borrow().as_ref() {
            if let Ok(lyrics) = Lyrics::try_from_synced_lrclib_str(&text)
                // .map_err(|res| {
                //     println!("Synced lyrics parse error: {:?}", &res);
                //     return res;
                // })
                .or_else(|_| Lyrics::try_from_plain_lrclib_str(&text))
            {
                sqlite::write_lyrics(curr_song.get_info(), Some(&lyrics))
                    .expect("Unable to import lyrics into SQLite DB");
                self.update_lyrics(lyrics);
            }
        }
    }

    pub fn clear_lyrics(&self) {
        if let Some(curr_song) = self.imp().current_song.borrow().as_ref() {
            sqlite::write_lyrics(&curr_song.get_info(), None).expect("Unable to clear lyrics from DB");
            self.imp().lyric_lines.splice(0, self.imp().lyric_lines.n_items(), &[]);
            let _ = self.imp().lyrics.take();
        }
    }
}

impl LocalRootInterface for Player {
    async fn raise(&self) -> fdo::Result<()> {
        self.imp().app.get().unwrap().raise_window();
        Ok(())
    }

    async fn quit(&self) -> fdo::Result<()> {
        self.imp().app.get().unwrap().quit();
        Ok(())
    }

    async fn can_quit(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn fullscreen(&self) -> fdo::Result<bool> {
        Ok(self.imp().app.get().unwrap().is_fullscreen())
    }

    async fn set_fullscreen(&self, fullscreen: bool) -> zbus::Result<()> {
        // Very funny, why is this returning a zbus result instead of fdo?
        self.imp().app.get().unwrap().set_fullscreen(fullscreen);
        Ok(())
    }

    async fn can_set_fullscreen(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn can_raise(&self) -> fdo::Result<bool> {
        Ok(true)
    }

    async fn has_track_list(&self) -> fdo::Result<bool> {
        // TODO
        Ok(false)
    }

    async fn identity(&self) -> fdo::Result<String> {
        Ok(APPLICATION_ID.to_string())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok(APPLICATION_ID.to_string())
    }

    async fn supported_uri_schemes(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }

    async fn supported_mime_types(&self) -> fdo::Result<Vec<String>> {
        Ok(vec![])
    }
}

impl LocalPlayerInterface for Player {
    async fn next(&self) -> fdo::Result<()> {
        self.next_song(false);
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.prev_song(false);
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.toggle_playback();
        Ok(())
    }

    async fn pause(&self) -> fdo::Result<()> {
        self.send_pause();
        Ok(())
    }

    async fn play_pause(&self) -> fdo::Result<()> {
        self.toggle_playback();
        Ok(())
    }

    async fn stop(&self) -> fdo::Result<()> {
        let _ = self.client().stop();
        Ok(())
    }

    async fn seek(&self, offset: Time) -> fdo::Result<()> {
        let curr_pos = self.imp().position.get();
        let new_pos = curr_pos + (offset.as_millis() as f64 / 1000.0);
        self.send_seek(new_pos);
        Ok(())
    }

    /// Use MPD's queue ID to construct track_id in this format:
    /// io/github/htkhiem/Euphonica/<queue_id>
    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        if let Some(song) = self.imp().current_song.borrow().as_ref() {
            if track_id.as_str().split("/").last().unwrap() == &song.get_queue_id().to_string() {
                self.send_seek(position.as_millis() as f64 / 1000.0);
                return Ok(());
            }
            return Err(fdo::Error::Failed("Song has already changed".to_owned()));
        }
        return Err(fdo::Error::Failed("No song is being played".to_owned()));
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported(
            "Euphonica currently does not support playing local files via MPD".to_owned(),
        ))
    }

    async fn playback_status(&self) -> fdo::Result<MprisPlaybackStatus> {
        Ok(self.imp().state.get().into())
    }

    async fn loop_status(&self) -> fdo::Result<LoopStatus> {
        Ok(self.imp().flow.get().into())
    }

    async fn set_loop_status(&self, loop_status: LoopStatus) -> zbus::Result<()> {
        let flow: PlaybackFlow = loop_status.into();
        let _ = self.client().set_playback_flow(flow);
        Ok(())
    }

    async fn rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(PlaybackRate::from(1.0))
    }

    async fn set_rate(&self, _rate: PlaybackRate) -> zbus::Result<()> {
        Err(zbus::Error::from(fdo::Error::NotSupported(
            "Euphonica currently does not support changing playback rate".to_owned(),
        )))
    }

    async fn shuffle(&self) -> fdo::Result<bool> {
        Ok(self.imp().random.get())
    }

    async fn set_shuffle(&self, shuffle: bool) -> zbus::Result<()> {
        self.set_random(shuffle);
        Ok(())
    }

    async fn metadata(&self) -> fdo::Result<MprisMetadata> {
        if let Some(song) = self.imp().current_song.borrow().as_ref() {
            Ok(song.get_mpris_metadata())
        } else {
            Ok(MprisMetadata::builder().trackid(TrackId::NO_TRACK).build())
        }
    }

    async fn volume(&self) -> fdo::Result<Volume> {
        Ok(self.imp().volume.get() as f64 / 100.0)
    }

    async fn set_volume(&self, volume: Volume) -> zbus::Result<()> {
        self.send_set_volume((volume * 100.0).round() as i8);
        Ok(())
    }

    async fn position(&self) -> fdo::Result<Time> {
        Ok(Time::from_millis(
            (self.imp().position.get() * 1000.0).round() as i64,
        ))
    }

    async fn minimum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(PlaybackRate::from(1.0))
    }

    async fn maximum_rate(&self) -> fdo::Result<PlaybackRate> {
        Ok(PlaybackRate::from(1.0))
    }

    async fn can_go_next(&self) -> fdo::Result<bool> {
        Ok(self.imp().mpris_enabled.get())
    }

    async fn can_go_previous(&self) -> fdo::Result<bool> {
        Ok(self.imp().mpris_enabled.get())
    }

    async fn can_play(&self) -> fdo::Result<bool> {
        Ok(self.imp().mpris_enabled.get())
    }

    async fn can_pause(&self) -> fdo::Result<bool> {
        Ok(self.imp().mpris_enabled.get())
    }

    async fn can_seek(&self) -> fdo::Result<bool> {
        Ok(self.imp().mpris_enabled.get())
    }

    async fn can_control(&self) -> fdo::Result<bool> {
        Ok(self.imp().mpris_enabled.get())
    }
}
