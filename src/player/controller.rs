extern crate mpd;
use crate::{
    application::EuphonicaApplication,
    cache::{Cache, CacheState},
    client::{ClientState, ConnectionState, MpdWrapper},
    common::{AlbumInfo, QualityGrade, Song},
    utils::{prettify_audio_format, settings_manager}
};
use async_lock::OnceCell as AsyncOnceCell;
use futures::future::FusedFuture;
use mpris_server::{
    zbus::{self, fdo},
    LocalPlayerInterface, LocalRootInterface, LocalServer,
    LoopStatus, Metadata as MprisMetadata, PlaybackRate,
    PlaybackStatus as MprisPlaybackStatus,
    Property, Signal as MprisSignal, Time, TrackId, Volume,
};

use adw::subclass::prelude::*;
use glib::{closure_local, subclass::Signal, BoxedAnyObject, clone};
use gtk::gdk::Texture;
use gtk::{gio, glib, prelude::*};
use mpd::{
    status::{AudioFormat, State, Status},
    ReplayGain,
    Subsystem,
    SaveMode,
    error::Error as MpdError
};
use std::{
    cell::{Cell, OnceCell, RefCell}, ops::Deref, path::PathBuf, rc::Rc, str::FromStr, thread, time::Duration, vec::Vec,
    sync::{
        atomic::{
            AtomicBool,
            Ordering
        },
        Arc,
        Mutex,
        OnceLock
    }
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
            PlaybackState::Paused => Self::Paused
        }
    }
}

#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaPlaybackFlow")]
pub enum PlaybackFlow {
    #[default]
    Sequential,  // Plays through the queue once
    Repeat,      // Loops through the queue
    Single,      // Plays one song then stops & waits for click to play next song
    RepeatSingle // Loops current song
}

pub enum SwapDirection {
    Up,
    Down
}

impl PlaybackFlow {
    pub fn from_status(st: &mpd::status::Status) -> Self {
        if st.repeat {
            if st.single {
                PlaybackFlow::RepeatSingle
            }
            else {
                PlaybackFlow::Repeat
            }
        }
        else {
            if st.single {
                PlaybackFlow::Single
            }
            else {
                PlaybackFlow::Sequential
            }
        }
    }

    pub fn icon_name(&self) -> &'static str {
        match self {
            &PlaybackFlow::Sequential => "playlist-consecutive-symbolic",
            &PlaybackFlow::Repeat => "playlist-repeat-symbolic",
            &PlaybackFlow::Single => "stop-sign-outline-symbolic",
            &PlaybackFlow::RepeatSingle => "playlist-repeat-song-symbolic"
        }
    }

    pub fn next_in_cycle(&self) -> Self {
        match self {
            &PlaybackFlow::Sequential => PlaybackFlow::Repeat,
            &PlaybackFlow::Repeat => PlaybackFlow::Single,
            &PlaybackFlow::Single => PlaybackFlow::RepeatSingle,
            &PlaybackFlow::RepeatSingle => PlaybackFlow::Sequential
        }
    }

    // TODO: translatable
    pub fn description(&self) -> &'static str {
        match self {
            &PlaybackFlow::Sequential => "Sequential",
            &PlaybackFlow::Repeat => "Repeat Queue",
            &PlaybackFlow::Single => "Single Song",
            &PlaybackFlow::RepeatSingle => "Repeat Current Song"
        }
    }
}

impl From<PlaybackFlow> for LoopStatus {
    fn from(pf: PlaybackFlow) -> Self {
        match pf {
            PlaybackFlow::RepeatSingle => Self::Track,
            PlaybackFlow::Repeat => Self::Playlist,
            PlaybackFlow::Sequential | PlaybackFlow::Single => Self::None
        }
    }
}

impl From<LoopStatus> for PlaybackFlow {
    fn from(ls: LoopStatus) -> Self {
        match ls {
            LoopStatus::Track => PlaybackFlow::RepeatSingle,
            LoopStatus::Playlist => PlaybackFlow::Repeat,
            LoopStatus::None => PlaybackFlow::Sequential
        }
    }
}

fn cycle_replaygain(curr: ReplayGain) -> ReplayGain {
    match curr {
        ReplayGain::Off => ReplayGain::Auto,
        ReplayGain::Auto => ReplayGain::Track,
        ReplayGain::Track => ReplayGain::Album,
        ReplayGain::Album => ReplayGain::Off
    }
}

fn get_replaygain_icon_name(mode: ReplayGain) -> &'static str {
    match mode {
        ReplayGain::Off => "rg-off-symbolic",
        ReplayGain::Auto => "rg-auto-symbolic",
        ReplayGain::Track => "rg-track-symbolic",
        ReplayGain::Album => "rg-album-symbolic"
    }
}

mod imp {
    use crate::application::EuphonicaApplication;
    use super::*;
    use glib::{
        ParamSpec, ParamSpecBoolean, ParamSpecDouble, ParamSpecEnum, ParamSpecFloat, ParamSpecObject, ParamSpecString, ParamSpecUInt, ParamSpecUInt64
    };
    use once_cell::sync::Lazy;

    pub struct Player {
        pub state: Cell<PlaybackState>,
        pub position: Cell<f64>,
        pub queue: gio::ListStore,
        pub current_song: RefCell<Option<Song>>,
        pub format: RefCell<Option<AudioFormat>>,
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
        // Set this to true to pause polling even if PlaybackState is Playing.
        // Used by seekbar widgets.
        pub poll_blocked: Cell<bool>,
        pub mpris_server: AsyncOnceCell<LocalServer<super::Player>>,
        pub mpris_enabled: Cell<bool>,
        pub app: OnceCell<EuphonicaApplication>,
        pub supports_playlists: Cell<bool>,
        // For receiving frequency levels from FFT thread
        pub fft_stop: Arc<AtomicBool>,
        pub fft_data: Arc<Mutex<(Vec<f32>, Vec<f32>)>>,  // Binned magnitudes, in stereo
        pub fft_handle: RefCell<Option<gio::JoinHandle<()>>>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Player {
        const NAME: &'static str = "EuphonicaPlayer";
        type Type = super::Player;

        fn new() -> Self {
            Self {
                state: Cell::new(PlaybackState::Stopped),
                position: Cell::new(0.0),
                random: Cell::new(false),
                consume: Cell::new(false),
                supports_playlists: Cell::new(false),
                replaygain: Cell::new(ReplayGain::Off),
                crossfade: Cell::new(0.0),
                mixramp_db: Cell::new(0.0),
                mixramp_delay: Cell::new(0.0),
                queue: gio::ListStore::new::<Song>(),
                current_song: RefCell::new(None),
                format: RefCell::new(None),
                flow: Cell::default(),
                client: OnceCell::new(),
                cache: OnceCell::new(),
                volume: Cell::new(0),
                poller_handle: RefCell::new(None),
                poll_blocked: Cell::new(false),
                mpris_server: AsyncOnceCell::new(),
                mpris_enabled: Cell::new(false),
                app: OnceCell::new(),
                fft_stop: Arc::new(AtomicBool::new(false)),
                fft_data: Arc::new(Mutex::new((
                    vec![0.0; settings_manager().child("player").uint("visualizer-spectrum-bands") as usize],
                    vec![0.0; settings_manager().child("player").uint("visualizer-spectrum-bands") as usize]
                ))),
                fft_handle: RefCell::new(None)
            }
        }
    }

    impl ObjectImpl for Player {
        fn constructed(&self) {
            self.parent_constructed();

            let settings = settings_manager();
            if settings.child("ui").boolean("use-visualizer") {
                self.obj().maybe_start_fft_thread();
            }
            settings.child("ui").connect_changed(
                Some("use-visualizer"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |settings, key| {
                        if settings.boolean(key) {
                            // Visualiser turned on. Start FFT thread.
                            this.obj().maybe_start_fft_thread();
                        }
                        else {
                            // Visualiser turned off. FFT thread should
                            // have stopped by itself. Join & yeet handle.
                            this.obj().maybe_stop_fft_thread();
                        }
                    }
                )
            );

            // TODO: restart thread upon changing visualiser config.
            // In UI there should be a "save" button to trigger saving
            // config and restarting thread since this is costly.
        }

        fn dispose(&self) {
            self.obj().maybe_stop_fft_thread();
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
                    ParamSpecDouble::builder("crossfade").build(), // seconds
                    ParamSpecFloat::builder("mixramp-db").build(),
                    ParamSpecDouble::builder("mixramp-delay").build(), // seconds
                    ParamSpecBoolean::builder("random").build(),
                    ParamSpecBoolean::builder("consume").build(),
                    ParamSpecBoolean::builder("supports-playlists").build(),
                    ParamSpecDouble::builder("position").build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecString::builder("album").read_only().build(),
                    ParamSpecObject::builder::<Texture>("album-art")
                        .read_only()
                        .build(), // Will use high-resolution version
                    ParamSpecUInt64::builder("duration").read_only().build(),
                    ParamSpecUInt::builder("queue-id").read_only().build(),
                    ParamSpecEnum::builder::<QualityGrade>("quality-grade")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("format-desc").read_only().build(),
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
                "crossfade" => self.crossfade.get().to_value(),
                "mixramp-db" => self.mixramp_db.get().to_value(),
                "mixramp-delay" => self.mixramp_delay.get().to_value(),
                "replaygain" => get_replaygain_icon_name(self.replaygain.get()).to_value(),
                "position" => obj.position().to_value(),
                // These are proxies for Song properties
                "title" => obj.title().to_value(),
                "artist" => obj.artist().to_value(),
                "album" => obj.album().to_value(),
                "album-art" => obj.current_song_album_art(false).to_value(), // High-res version
                "duration" => obj.duration().to_value(),
                "queue-id" => obj.queue_id().to_value(),
                "quality-grade" => obj.quality_grade().to_value(),
                "format-desc" => obj.format_desc().to_value(),
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
                },
                "mixramp-db" => {
                    if let Ok(v) = value.get::<f32>() {
                        obj.set_mixramp_db(v);
                    }
                },
                "mixramp-delay" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_mixramp_delay(v);
                    }
                },
                "position" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_position(v);
                    }
                },
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
                _ => unimplemented!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("outputs-changed")
                        .param_types([BoxedAnyObject::static_type()])
                        .build(),
                    // Reserved for EXTERNAL changes (i.e. changes made by this client won't
                    // emit this).
                    Signal::builder("volume-changed")
                        .param_types([i8::static_type()])
                        .build(),
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
    /// Lazily get an MPRIS server. This will always be invoked near the start anyway
    /// by the initial call to update_status().
    async fn get_mpris(&self) -> zbus::Result<&LocalServer<Self>> {
        self.imp().mpris_server.get_or_try_init(|| async {
            let server = LocalServer::new("org.euphonica.Euphonica", self.clone())
                .await?;
            glib::spawn_future_local(server.run());
            Ok(server)
        }).await
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
        let stop_flag = self.imp().fft_stop.clone();
        stop_flag.store(false, Ordering::Relaxed);
        let output = self.imp().fft_data.clone();
        let should_start: bool;
        {
            should_start = self.imp().fft_handle.borrow().as_ref().is_none();
        }
        if should_start {
            let fft_handle = gio::spawn_blocking(move || {
                println!("Starting FFT loop...");
                let settings = settings_manager();
                let player_settings = settings.child("player");
                // Will require starting a new thread to account for path and format changes
                if let Ok(format) = AudioFormat::from_str(settings.child("client").string("mpd-fifo-format").as_str()) {
                    // These settings require a restart
                    let n_samples = player_settings.uint("visualizer-fft-samples") as usize;
                    let n_bins = player_settings.uint("visualizer-spectrum-bins") as usize;
                    if let Ok(mut reader) = super::fft::try_open_pipe(
                        settings.child("client").string("mpd-fifo-path").as_str(),
                        &format,
                        n_samples
                    ) {
                        // Allocate the following once only
                        let mut fft_buf_left: Vec<f32> = vec![0.0; n_samples];
                        let mut fft_buf_right: Vec<f32> = vec![0.0; n_samples];
                        let mut curr_step_left: Vec<f32> = vec![0.0; n_bins];
                        let mut curr_step_right: Vec<f32> = vec![0.0; n_bins];
                        'outer: loop {
                            // These should be applied on-the-fly
                            let bin_mode = if player_settings.boolean("visualizer-spectrum-use-log-bins") {
                                super::fft::BinMode::Logarithmic
                            } else {
                                super::fft::BinMode::Linear
                            };
                            let fps = player_settings.uint("visualizer-fps") as f32;
                            let min_freq = player_settings.uint("visualizer-spectrum-min-hz") as f32;
                            let max_freq = player_settings.uint("visualizer-spectrum-max-hz") as f32;
                            let curr_step_weight = player_settings.double("visualizer-spectrum-curr-step-weight") as f32;
                            if stop_flag.load(Ordering::Relaxed) || !settings.child("ui").boolean("use-visualizer") {
                                break 'outer;
                            }
                            // TEST
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
                                        max_freq
                                    );
                                    super::fft::get_magnitudes(
                                        &format,
                                        &mut fft_buf_right,
                                        &mut curr_step_right,
                                        n_bins as u32,
                                        bin_mode,
                                        min_freq,
                                        max_freq
                                    );
                                    // Replace last frame
                                    if let Ok(mut output_lock) = output.lock() {
                                        if output_lock.0.len() != n_bins || output_lock.1.len() != n_bins {
                                            output_lock.0.clear();
                                            output_lock.1.clear();
                                            for _ in 0..n_bins {
                                                output_lock.0.push(0.0);
                                                output_lock.1.push(0.0);
                                            }
                                        }
                                        for i in 0..n_bins {
                                            output_lock.0[i] = curr_step_left[i] * curr_step_weight + output_lock.0[i] * (1.0 - curr_step_weight);
                                            output_lock.1[i] = curr_step_right[i] * curr_step_weight + output_lock.1[i] * (1.0 - curr_step_weight);
                                        }
                                        // println!("FFT L: {:?}\tR: {:?}", &output_lock.0, &output_lock.1);
                                    }
                                    else {
                                        panic!("Unable to lock FFT data mutex");
                                    }
                                }
                                Err(e) => {
                                    match e.kind() {
                                        std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::WouldBlock => {}
                                        _ => {
                                            println!("FFT ERR: {:?}", &e);
                                            break 'outer;
                                        }
                                    }
                                }
                            }
                            thread::sleep(Duration::from_millis(
                                (1000.0 / fps).floor() as u64
                            ));
                        }
                    }
                }
            });
            self.imp().fft_handle.replace(Some(fft_handle));
        }
    }

    fn maybe_stop_fft_thread(&self) {
        if let Some(handle) = self.imp().fft_handle.take() {
            self.imp().fft_stop.store(true, Ordering::Relaxed);
            // Digusting sync hack - will freeze UI
            while !handle.is_terminated() { thread::sleep(Duration::from_millis(10)); }
        }
    }

    pub fn fft_data(&self) -> Arc<Mutex<(Vec<f32>, Vec<f32>)>> {
        self.imp().fft_data.clone()
    }

    pub fn setup(
        &self,
        application: EuphonicaApplication,
        client: Rc<MpdWrapper>,
        cache: Rc<Cache>
    ) {
        let client_state = client.clone().get_client_state();
        let _ = self.imp().client.set(client);

        // Signal once for current songs in case the UI is initialised too quickly,
        // causing the player pane & bar to be stuck without album art for the
        // song playing at startup.
        cache.get_cache_state().connect_closure(
            "album-art-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, folder_uri: String| {
                    if let Some(song) = this.imp().current_song.borrow().as_ref() {
                        if let Some(album) = song.get_album() {
                            if album.uri.as_str() == folder_uri.as_str() {
                                this.notify("album-art");
                            }
                        }
                    }
                }
            )
        );

        let _ = self.imp().cache.set(cache);
        let _ = self.imp().app.set(application);
        // Connect to ClientState signals
        client_state.connect_notify_local(Some("connection-state"), clone!(
            #[weak(rename_to = this)]
            self,
            move |state, _| {
                if state.get_connection_state() == ConnectionState::Connected {
                    // Newly-connected? Get initial status
                    // Remember to get queue before status so status parsing has something to read off.
                    if let Some(songs) = this.client().get_current_queue() {
                        this.update_queue(&songs, true);
                    }
                    if let Some(status) = this.client().get_status() {
                        this.update_status(&status);
                    }
                    if let Some(outs) = this.client().get_outputs() {
                        this.update_outputs(glib::BoxedAnyObject::new(outs));
                    }
                }
            }
        ));
        client_state
            .bind_property(
                "supports-playlists",
                self,
                "supports-playlists"
            )
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
                            if let Some(status) = this.client().get_status() {
                                this.update_status(&status);
                            }
                        }
                        Subsystem::Queue => {
                            if let Some(songs) = this.client().get_queue_changes() {
                                this.update_queue(&songs, false);
                            }
                            // Need to also update queue length
                            if let Some(status) = this.client().get_status() {
                                this.update_status(&status);
                            }
                        }
                        Subsystem::Output => {
                            if let Some(outs) = this.client().get_outputs() {
                                this.update_outputs(glib::BoxedAnyObject::new(outs));
                            }
                        }
                        _ => {}
                    }
                }
            )
        );

        let settings = settings_manager().child("player");
        let _ = self.imp().mpris_enabled.replace(settings.boolean("enable-mpris"));
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
                        this.update_mpris_properties(vec![
                            Property::Metadata(MprisMetadata::default()),
                        ]);
                    }
                }
            )
        );
    }

    fn update_mpris_properties(&self, properties: Vec<Property>) {
        glib::spawn_future_local(
            clone!(
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
            ),
        );
    }

    fn seek_mpris(&self, position: f64) {
        glib::spawn_future_local(
            clone!(
                #[weak(rename_to = this)]
                self,
                async move {
                    match this.get_mpris().await {
                        Ok(mpris) => {
                            let pos_time = Time::from_millis((position * 1000.0).round() as i64);
                            if let Err(err) = mpris.emit(MprisSignal::Seeked { position: pos_time }).await {
                                println!("{:?}", err);
                            }
                        }
                        Err(err) => {
                            println!("No MPRIS server: {:?}", err);
                        }
                    }
                }
            ),
        );
    }

    // Update functions
    // These all have side-effects of notifying listeners of changes to the
    // GObject properties, which in turn are read from this struct's fields.
    // Signals will be sent for properties whose values have changed, even though
    // we will be receiving updates for many properties at once.

    /// Main update function. MPD's protocol has a single "status" commands
    /// that returns everything at once. This update function will take what's
    /// relevant and update the GObject properties accordingly.
    ///
    /// This function must only be called AFTER updating the queue. MPD's idle
    /// change notifier already follows this convention (by sending the queue change
    /// notification before the player one).
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
            },
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
            },
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
            },
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

        let old_mixramp_db = self.imp().mixramp_db.replace(status.mixrampdb);
        if old_mixramp_db != status.mixrampdb {
            self.notify("mixramp-db");
        }

        let new_mixramp_delay: f64;
        if let Some(dur) = status.mixrampdelay {
            new_mixramp_delay = dur.as_secs_f64();
        }
        else {
            new_mixramp_delay = 0.0;
        }
        let old_mixramp_delay = self.imp().mixramp_delay.replace(new_mixramp_delay);
        if old_mixramp_delay != new_mixramp_delay {
            self.notify("mixramp-delay");
        }

        let new_crossfade: f64;
        if let Some(dur) = status.crossfade {
            new_crossfade = dur.as_secs_f64();
        }
        else {
            new_crossfade = 0.0;
        }
        let old_crossfade = self.imp().crossfade.replace(new_crossfade);
        if old_crossfade != new_crossfade {
            self.notify("crossfade");
        }

        if let Some(new_position_dur) = status.elapsed {
            let new = new_position_dur.as_secs_f64();
            let old = self.set_position(new);
            if new != old && self.imp().mpris_enabled.get() {
                self.seek_mpris(new);
            }
        } else {
            self.set_position(0.0);
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
            // There is now a playing song.
            // Check if there was one and whether it is different from the one playing now.
            // let new_id: u32 = new_queue_place.id.0;
            let new_pos: u32 = new_queue_place.pos;
            let new_song = self
                .imp()
                .queue
                .item(new_pos)
                .expect("Expected queue to have a song at new_pos")
                .downcast::<Song>()
                .expect("Queue has to contain common::Song objects");
            // Set playing status
            new_song.set_is_playing(true); // this whole thing would only run if playback state is not Stopped
            // Always replace current song to ensure we're pointing at something on the queue.
            // This is because a partial queue update may replace the current song with another instance of the
            // same song (for example, when deleting a song before it from the queue).
            let maybe_old_song = self.imp().current_song.replace(Some(new_song.clone()));
            if let Some(old_song) = maybe_old_song {
                if old_song.get_queue_id() != new_song.get_queue_id() {
                    old_song.set_is_playing(false);
                    // There was nothing playing previously. Update the following now:
                    self.notify("title");
                    self.notify("artist");
                    self.notify("duration");
                    self.notify("quality-grade");
                    self.notify("format-desc");
                    // Avoid needlessly changing album art as background blur updates are expensive.
                    if new_song.get_album_title() != old_song.get_album_title() {
                        self.notify("album");
                        self.notify("album-art");
                    }
                    // Update MPRIS side
                    if self.imp().mpris_enabled.get() {
                        mpris_changes.push(Property::Metadata(new_song.get_mpris_metadata(
                            self.imp().cache.get().unwrap().clone())
                        ));
                    }
                }
            }
            else {
                // There was nothing playing previously. Update the following now:
                self.notify("title");
                self.notify("artist");
                self.notify("duration");
                self.notify("quality-grade");
                self.notify("format-desc");
                self.notify("album");
                self.notify("album-art");
            }
            // else if let Some(song) = self.imp().current_song.borrow().as_ref() {
            //     // Just update pos. It's cheap so no need to check.
            //     song.set_queue_pos(new_pos);
            // }
        } else {
            // No song is playing. Update state accordingly.
            let was_playing = self.imp().current_song.borrow().as_ref().is_some(); // end borrow
            if  was_playing {
                let _ = self.imp().current_song.take();
                self.notify("title");
                self.notify("artist");
                self.notify("album");
                self.notify("album-art");
                self.notify("duration");
                // Update MPRIS side
                if self.imp().mpris_enabled.get() {
                    mpris_changes.push(Property::Metadata(
                        MprisMetadata::builder()
                            .trackid(TrackId::NO_TRACK)
                            .build()
                    ));
                }
            }
        }

        // If new queue is shorter, truncate current queue.
        // This is because update_queue would be called before update_status, which means
        // the new length was not available to update_queue.
        let old_len = self.imp().queue.n_items();
        let new_len = status.queue_len;
        if old_len > new_len {
            self.imp().queue.splice(new_len, old_len - new_len, &[] as &[Song; 0]);
        }

        self.update_mpris_properties(mpris_changes);
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
    ///
    /// If an MPRIS server is running, it will also emit property change signals.
    pub fn update_queue(&self, songs: &[Song], replace: bool) {
        let queue = &self.imp().queue;
        if replace {
            if songs.len() == 0 {
                queue.remove_all();
            }
            else {
                // Replace overlapping portion.
                // Only emit one changed signal for both removal and insertion. This avoids the brief visual
                // blanking between queue versions.
                // New songs should all have is_playing == false.
                queue.splice(0, queue.n_items(), &songs[..(songs.len().min(queue.n_items() as usize))]);
                if songs.len() > queue.n_items() as usize {
                    queue.extend_from_slice(&songs[(queue.n_items() as usize)..]);
                }
            }
        }
        else {
            if songs.len() > 0 {
                // Update overlapping portion
                let curr_len = self.imp().queue.n_items() as usize;
                let mut overlap: Vec<Song> = Vec::with_capacity(curr_len);
                let mut new_pos: usize = 0;
                for (i, maybe_old_song) in queue.iter::<Song>().enumerate() {
                    if i >= curr_len {
                        // Out of overlapping portion
                        break;
                    }
                    // See if this position is changed
                    if songs[new_pos].get_queue_pos() as usize == i {
                        overlap.push(songs[new_pos].clone());
                        if new_pos < songs.len() - 1 {
                            new_pos += 1;
                        }
                    }
                    else {
                        let old_song = maybe_old_song.expect("Cannot read from old queue");
                        overlap.push(old_song);
                    }
                }
                // Only emit one changed signal for both removal and insertion
                queue.splice(0, overlap.len() as u32, &overlap);

                // Add songs beyond the length of the old queue
                if new_pos < songs.len() {
                    queue.extend_from_slice(&songs[new_pos..]);
                }
            }
        }

        if let Some(cache) = self.imp().cache.get() {
            let infos: Vec<&AlbumInfo> = songs
                .into_iter()
                .map(|song| song.get_album())
                .filter(|ao| ao.is_some())
                .map(|info| info.unwrap())
                .collect();
            // Might queue downloads, depending on user settings
            cache.ensure_cached_album_arts(&infos);
        }
        // Downstream widgets should now receive an item-changed signal.
    }

    fn client(&self) -> &Rc<MpdWrapper> {
        self.imp().client.get().unwrap()
    }

    fn update_outputs(&self, outputs: BoxedAnyObject) {
        self.emit_by_name::<()>("outputs-changed", &[&outputs]);
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

    pub fn current_song_album_art(&self, thumbnail: bool) -> Option<Texture> {
        if let Some(song) = self.imp().current_song.borrow().as_ref() {
            if let Some(cache) = self.imp().cache.get() {
                if let Some(album) = song.get_album() {
                    // Should have been scheduled by queue updates.
                    return cache.load_cached_album_art(album, thumbnail, false);
                }
            }
            return None;
        }
        None
    }

    pub fn current_song_album_art_path(&self, thumbnail: bool) -> Option<PathBuf> {
        if let (
            Some(song), Some(cache)
        ) = (
            self.imp().current_song.borrow().as_ref(),
            self.imp().cache.get()
        )
            {
            if let Some(album) = song.get_album() {
                // Always read from disk
                Some(
                    cache.get_path_for(
                        &crate::meta_providers::Metadata::AlbumArt(album.uri.to_owned(), thumbnail)
                    )
                )
            }
            else {
                None
            }
        }
        else {
            None
        }
    }

    pub fn quality_grade(&self) -> QualityGrade {
        if let Some(song) = &*self.imp().current_song.borrow() {
            return song.get_quality_grade();
        }
        QualityGrade::Unknown
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

    pub fn queue_id(&self) -> u32 {
        if let Some(song) = &*self.imp().current_song.borrow() {
            return song.get_queue_id();
        }
        // Should never match a real song.
        u32::MAX
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
    pub fn send_seek(&self) {
        self.client().seek_current_song(self.position());
    }

    pub fn queue(&self) -> gio::ListStore {
        self.imp().queue.clone()
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

    pub fn prev_song(&self) {
        self.client().prev();
    }

    pub fn next_song(&self) {
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
        self.client().play_at(song.get_queue_id(), true);
    }

    pub fn remove_song_id(&self, id: u32) {
        self.client().delete_at(id, true);
    }

    pub fn swap_dir(&self, pos: u32, direction: SwapDirection) {
        match direction {
            SwapDirection::Up => {
                if pos > 0 {
                    self.client().swap(pos, pos - 1, false);
                }
            }
            SwapDirection::Down => {
                if pos < self.imp().queue.n_items() - 1 {
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
        if self.imp().poller_handle.borrow().is_none() && !self.imp().poll_blocked.get() {
            let poller_handle = glib::MainContext::default().spawn_local(async move {
                loop {
                    // Don't poll if not playing
                    if this.imp().state.get() == PlaybackState::Playing {
                        if let Some(status) = client.clone().get_status() {
                            this.update_status(&status);
                        }
                    }
                    glib::timeout_future_seconds(1).await;
                }
            });
            self.imp().poller_handle.replace(Some(poller_handle));
        }
        else if self.imp().poll_blocked.get() {
            println!("Polling blocked");
        }
    }

    /// Stop poller loop. Seekbar should call this when being interacted with.
    pub fn stop_polling(&self) {
        if let Some(handle) = self.imp().poller_handle.take() {
            handle.abort();
        }
    }

    pub fn block_polling(&self) {
        let _ = self.imp().poll_blocked.replace(true);
    }

    pub fn unblock_polling(&self) {
        let _ = self.imp().poll_blocked.replace(false);
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
        Ok("Euphonica".to_string())
    }

    async fn desktop_entry(&self) -> fdo::Result<String> {
        Ok("org.euphonica.Euphonica".to_string())
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
        self.next_song();
        Ok(())
    }

    async fn previous(&self) -> fdo::Result<()> {
        self.prev_song();
        Ok(())
    }

    async fn play(&self) -> fdo::Result<()> {
        self.send_play();
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
        let _ = self.imp().position.replace(new_pos);
        self.send_seek();
        Ok(())
    }

    /// Use MPD's queue ID to construct track_id in this format:
    /// /org/euphonica/Euphonica/<queue_id>
    async fn set_position(&self, track_id: TrackId, position: Time) -> fdo::Result<()> {
        if let Some(song) = self.imp().current_song.borrow().as_ref() {
            if track_id.as_str().split("/").last().unwrap() == &song.get_queue_id().to_string() {
                let _ = self.imp().position.replace(position.as_millis() as f64 / 1000.0);
                self.send_seek();
                return Ok(());
            }
            return Err(fdo::Error::Failed("Song has already changed".to_owned()));
        }
        return Err(fdo::Error::Failed("No song is being played".to_owned()));
    }

    async fn open_uri(&self, _uri: String) -> fdo::Result<()> {
        Err(fdo::Error::NotSupported("Euphonica currently does not support playing local files via MPD".to_owned()))
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
            "Euphonica currently does not support changing playback rate".to_owned())
        ))
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
            Ok(song.get_mpris_metadata(self.imp().cache.get().unwrap().clone()))
        }
        else {
            Ok(
                MprisMetadata::builder()
                    .trackid(TrackId::NO_TRACK)
                    .build()
            )
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
        Ok(Time::from_millis((self.imp().position.get() * 1000.0).round() as i64))
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
