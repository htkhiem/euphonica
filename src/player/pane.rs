use adw::prelude::*;
use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use glib::{SignalHandlerId, WeakRef, clone, closure_local};
use gtk::{
    CompositeTemplate,
    glib::{self, Variant},
    subclass::prelude::*,
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    fs::{self, File},
    io::Write,
    rc::Rc,
};

use crate::{
    cache::{
        Cache, placeholders::{EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING}
    },
    client::{ClientState, state::StickersSupportLevel},
    common::{PictureStack, Rating, Song, paintables::RotatingPaintable},
    player::seekbar2::Seekbar,
    utils::{self, settings_manager, sync_animation}, window::EuphonicaWindow,
};

use super::{MpdOutput, OutputControls, PlaybackControls, PlaybackState, Player, VolumeKnob};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/pane.ui")]
    pub struct PlayerPane {
        // Song info
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub albumart: TemplateChild<PictureStack>,
        #[template_child]
        pub song_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub album: TemplateChild<gtk::Label>,
        #[template_child]
        pub rating: TemplateChild<Rating>,

        // Lyrics box
        #[template_child]
        pub lyrics_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub lyrics_box: TemplateChild<gtk::ListBox>,

        // Playback controls
        #[template_child]
        pub playback_controls: TemplateChild<PlaybackControls>,
        #[template_child]
        pub seekbar: TemplateChild<Seekbar>,
        #[template_child]
        pub seekbar_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub rg_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub crossfade_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub crossfade: TemplateChild<gtk::SpinButton>,
        #[template_child]
        pub mixramp_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub mixramp_db: TemplateChild<gtk::SpinButton>,
        #[template_child]
        pub mixramp_delay: TemplateChild<gtk::SpinButton>,
        #[template_child]
        pub lyrics_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub show_lyrics: TemplateChild<gtk::Switch>,
        #[template_child]
        pub use_synced_lyrics: TemplateChild<gtk::Switch>,
        #[template_child]
        pub refetch_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub import_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub export_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub clear_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub output_controls: TemplateChild<OutputControls>,
        #[template_child]
        pub vol_knob: TemplateChild<VolumeKnob>,

        // Kept here so we can access it in snapshot()
        pub output_widgets: RefCell<Vec<MpdOutput>>,

        pub player: WeakRef<Player>,
        pub albumart_paintable: RotatingPaintable,
        pub albumart_animation: OnceCell<adw::TimedAnimation>,
        pub albumart_rotation_offset: Cell<f64>,
        pub albumart_rotation_speed: Cell<f64>,
        pub current_lyric_line_id: RefCell<Option<SignalHandlerId>>,
        pub cover_changed_id: RefCell<Option<SignalHandlerId>>,
        pub song_changed_id: RefCell<Option<SignalHandlerId>>,
        pub playback_state_id: RefCell<Option<SignalHandlerId>>,
        pub seeked_id: RefCell<Option<SignalHandlerId>>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PlayerPane {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPlayerPane";
        type Type = super::PlayerPane;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for PlayerPane {
        fn constructed(&self) {
            self.parent_constructed();
            let ui_settings = settings_manager().child("ui");

            self.albumart.install_wrapper(&self.albumart_paintable);
            let target = adw::CallbackAnimationTarget::new(clone!(
                #[weak(rename_to = this)]
                self,
                move |rotation| this.albumart_paintable.set_rotation(rotation)
            ));
            self.albumart_animation
                .set(
                    adw::TimedAnimation::builder()
                        .widget(self.obj().as_ref())
                        .target(&target)
                        .value_from(0.0)
                        .value_to(360.0)
                        .duration(20_000)
                        .repeat_count(0)
                        .easing(adw::Easing::Linear)
                        .build(),
                )
                .unwrap();
            self.albumart_paintable.connect_circular_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.obj().resync_album_art_rotation()
            ));
            for property in ["return-to-starting-angle", "degrees-per-second"] {
                self.albumart_paintable.connect_notify_local(
                    Some(property),
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move |_, _| {
                            if let Some(player) = this.player.upgrade() {
                                this.obj().rebase_album_art_rotation(player.position());
                            }
                        }
                    ),
                );
            }
            ui_settings
                .bind(
                    "album-art-rotation-speed",
                    &self.albumart_paintable,
                    "degrees-per-second",
                )
                .get_only()
                .mapping(|v: &Variant, _| {
                    Some(super::rotation_degrees_per_second(v.get::<f64>().unwrap()).to_value())
                })
                .build();
            ui_settings
                .bind(
                    "return-to-starting-angle",
                    &self.albumart_paintable,
                    "return-to-starting-angle",
                )
                .get_only()
                .build();
            ui_settings
                .bind("rotate-album-art", &self.albumart_paintable, "circular")
                .get_only()
                .build();
            // The settings bindings above must run before reading degrees-per-second.
            self.albumart_rotation_speed
                .set(self.albumart_paintable.rotation_speed());
            let knob = self.vol_knob.get();
            ui_settings
                .bind("vol-knob-unit", &knob, "use-dbfs")
                .get_only()
                .mapping(|v: &Variant, _| {
                    Some((v.get::<String>().unwrap().as_str() == "decibels").to_value())
                })
                .build();

            ui_settings
                .bind("vol-knob-sensitivity", &knob, "sensitivity")
                .mapping(|v: &Variant, _| Some(v.get::<f64>().unwrap().to_value()))
                .build();

            let pane_settings = settings_manager().child("state").child("queueview");
            pane_settings
                .bind("show-lyrics", &self.show_lyrics.get(), "active")
                .build();

            pane_settings
                .bind("use-synced-lyrics", &self.use_synced_lyrics.get(), "active")
                .build();

            self.show_lyrics
                .bind_property("active", &self.lyrics_btn.get(), "icon-name")
                .transform_to(|_, show_lyrics: bool| {
                    if show_lyrics {
                        Some("lyrics-on-symbolic")
                    } else {
                        Some("lyrics-off-symbolic")
                    }
                })
                .sync_create()
                .build();
        }

        fn dispose(&self) {
            // Disconnect player signal handlers to prevent callbacks
            // from running on disposed widgets
            if let Some(player) = self.player.upgrade() {
                if let Some(id) = self.current_lyric_line_id.take() {
                    player.disconnect(id);
                }
                if let Some(id) = self.cover_changed_id.take() {
                    player.disconnect(id);
                }
                if let Some(id) = self.song_changed_id.take() {
                    player.disconnect(id);
                }
                if let Some(id) = self.playback_state_id.take() {
                    player.disconnect(id);
                }
                if let Some(id) = self.seeked_id.take() {
                    player.disconnect(id);
                }
            }
            self.albumart_animation.get().unwrap().reset();
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlayerPane {
        fn map(&self) {
            self.parent_map();
            self.obj().resync_album_art_rotation();
        }
    }

    impl BoxImpl for PlayerPane {}
}

glib::wrapper! {
    pub struct PlayerPane(ObjectSubclass<imp::PlayerPane>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for PlayerPane {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlayerPane {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn update_lyrics_availability(&self, player: &Player) {
        let has_lyrics = player.n_lyric_lines() > 0;
        self.imp()
            .lyrics_window
            .set_visible(has_lyrics && self.imp().show_lyrics.is_active());
        self.imp().export_lyrics.set_sensitive(has_lyrics);
        self.imp().clear_lyrics.set_sensitive(has_lyrics);
        self.imp().refetch_lyrics.set_visible(!has_lyrics);
    }

    pub fn update_lyrics_state(&self, player: &Player) {
        let lyrics_box = self.imp().lyrics_box.get();
        let lyrics_window = self.imp().lyrics_window.get();
        let n_lyric_lines = player.n_lyric_lines();
        if player.lyrics_are_synced() && self.imp().use_synced_lyrics.is_active() {
            let curr_line_idx = player.current_lyric_line();
            for i in 0..n_lyric_lines {
                if let Some(row) = lyrics_box.row_at_index(i as i32)
                    && let Some(label) = row.child()
                {
                    label.set_opacity(if i == curr_line_idx { 1.0 } else { 0.2 });
                }
            }
            let v_adjust = lyrics_window.vadjustment();
            if let Some(row) = lyrics_box.row_at_index(curr_line_idx as i32) {
                let bounds = row.compute_bounds(&lyrics_box).unwrap();

                // Calculate the target scroll position to center the row.
                // Specifically, we centre the imaginary "page" within the adjustment at the row.
                // Even more specifically cuz my future self is always dumber than right now: align
                // the vertical midpoints of the page and the row.
                let page_size = v_adjust.page_size() as f32;
                let row_height = bounds.height();
                let row_top_left = bounds.top_left();
                let row_midpoint = row_top_left.y() + row_height / 2.0;
                let page_top = row_midpoint - page_size / 2.0; // < 0 or > bottom is fine, GtkAdjustments will just clamp to top/bottom

                v_adjust.set_value(page_top as f64);
            };
        } else {
            for i in 0..n_lyric_lines {
                if let Some(row) = lyrics_box.row_at_index(i as i32)
                    && let Some(label) = row.child()
                {
                    label.set_opacity(1.0);
                }
            }
        }
    }

    pub fn setup(&self, player: &Player, cache: Rc<Cache>, client_state: &ClientState, win: &EuphonicaWindow) {
        self.imp().player.set(Some(player));
        self.imp()
            .song_changed_id
            .replace(Some(player.connect_closure(
                "song-changed",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |player: Player, cover_changed: bool, automatic_transition: bool| {
                        this.sync_album_art_animation_for_song(
                            player.current_song(),
                            player.position(),
                            cover_changed,
                            automatic_transition,
                        );
                    }
                ),
            )));
        self.sync_album_art_animation_for_song(
            player.current_song(),
            player.position(),
            true,
            true,
        );
        self.imp()
            .playback_state_id
            .replace(Some(player.connect_notify_local(
                Some("playback-state"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| {
                        this.sync_album_art_animation();
                    }
                ),
            )));
        self.imp().seeked_id.replace(Some(player.connect_closure(
            "seeked",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: Player, position: f64| {
                    if this.imp().albumart_paintable.circular() {
                        this.resync_album_art_rotation_to(position);
                    }
                }
            ),
        )));
        self.bind_state(player, cache, client_state, win);
        self.imp().playback_controls.setup(player);
        self.imp().output_controls.setup(player);
        self.imp().seekbar.setup(player);
    }

    fn bind_state(&self, player: &Player, cache: Rc<Cache>, client_state: &ClientState, win: &EuphonicaWindow) {
        let imp = self.imp();
        self.imp().vol_knob.setup(player);
        let rg_btn = self.imp().rg_btn.get();
        player
            .bind_property("replaygain", &rg_btn, "icon-name")
            .sync_create()
            .build();
        player
            .bind_property("replaygain", &rg_btn, "tooltip-text")
            // TODO: translatable
            .transform_to(|_, icon: String| match icon.as_ref() {
                "rg-off-symbolic" => Some("ReplayGain: off"),
                "rg-auto-symbolic" => Some("ReplayGain: auto-select between track & album"),
                "rg-track-symbolic" => Some("ReplayGain: track"),
                "rg-album-symbolic" => Some("ReplayGain: album"),
                _ => None,
            })
            .sync_create()
            .build();
        rg_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                glib::spawn_future_local(async move {
                    player.cycle_replaygain().await;
                });
            }
        ));

        let crossfade_btn = self.imp().crossfade_btn.get();
        player
            .bind_property("crossfade", &crossfade_btn, "icon-name")
            .transform_to(|_, secs: f64| {
                if secs > 0.0 {
                    Some("crossfade-symbolic")
                } else {
                    Some("crossfade-off-symbolic")
                }
            })
            .sync_create()
            .build();

        let crossfade = self.imp().crossfade.get();
        player
            .bind_property("crossfade", &crossfade, "value")
            .bidirectional()
            .sync_create()
            .build();

        let mixramp_btn = self.imp().mixramp_btn.get();
        player
            .bind_property("mixramp-delay", &mixramp_btn, "icon-name")
            .transform_to(|_, secs: f64| {
                if secs > 0.0 {
                    Some("mixramp-symbolic")
                } else {
                    Some("mixramp-off-symbolic")
                }
            })
            .sync_create()
            .build();
        let mixramp_db = self.imp().mixramp_db.get();
        player
            .bind_property("mixramp-db", &mixramp_db, "value")
            .bidirectional()
            .sync_create()
            .build();
        let mixramp_delay = self.imp().mixramp_delay.get();
        player
            .bind_property("mixramp-delay", &mixramp_delay, "value")
            .bidirectional()
            .sync_create()
            .build();

        let info_box = imp.info_box.get();
        player
            .bind_property("playback-state", &info_box, "visible")
            .transform_to(|_, state: PlaybackState| Some(state != PlaybackState::Stopped))
            .sync_create()
            .build();

        let seekbar_revealer = imp.seekbar_revealer.get();
        player
            .bind_property("playback-state", &seekbar_revealer, "reveal_child")
            .transform_to(|_, state: PlaybackState| Some(state != PlaybackState::Stopped))
            .sync_create()
            .build();

        let song_name = imp.song_name.get();
        player
            .bind_property("title", &song_name, "label")
            .sync_create()
            .build();

        let album = imp.album.get();
        player
            .bind_property("album", &album, "label")
            .transform_to(|_, s: Option<&str>| {
                Some(if s.is_none_or(|s| s.is_empty()) {
                    (*EMPTY_ALBUM_STRING).to_value()
                } else {
                    s.to_value()
                })
            })
            .sync_create()
            .build();

        let artist = imp.artist.get();
        player
            .bind_property("artist", &artist, "label")
            .transform_to(|_, s: Option<&str>| {
                Some(if s.is_none_or(|s| s.is_empty()) {
                    (*EMPTY_ARTIST_STRING).to_value()
                } else {
                    s.to_value()
                })
            })
            .sync_create()
            .build();

        let rating = imp.rating.get();
        player
            .bind_property("rating", &rating, "value")
            .sync_create()
            .build();

        client_state
            .bind_property("stickers-support-level", &rating, "visible")
            .transform_to(|_, lvl: StickersSupportLevel| {
                Some((lvl >= StickersSupportLevel::SongsOnly).to_value())
            })
            .sync_create()
            .build();

        rating.connect_closure(
            "changed",
            false,
            closure_local!(
                #[weak]
                player,
                move |rating: Rating| {
                    let rating_val = rating.value();
                    let rating_opt = if rating_val > 0 {
                        Some(rating_val)
                    } else {
                        None
                    };
                    glib::spawn_future_local(async move {
                        player.rate_current_song(rating_opt).await;
                    });
                }
            ),
        );

        let lyric_lines = player.lyrics();
        lyric_lines.connect_notify_local(
            Some("n-items"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                player,
                move |_, _| {
                    this.update_lyrics_availability(&player);
                }
            ),
        );
        // Synced lyrics handling:
        // - Upon loading new lyrics, player controller sets new lyrics object,
        // clears out lyric_lines and repopulates it with new lyrics.
        // - With new lyrics object already in place, this callback will always
        // fetch that new object's synced property, rendering all newly created
        // Labels at 20% opacity.
        let lyrics_box = imp.lyrics_box.get();
        lyrics_box.bind_model(
            Some(player.lyrics()),
            clone!(
                #[weak]
                player,
                #[upgrade_or]
                gtk::Label::default().into(),
                move |line| {
                    let widget = gtk::Label::new(Some(
                        &line.downcast_ref::<gtk::StringObject>().unwrap().string(),
                    ));
                    widget.set_halign(gtk::Align::Center);
                    widget.set_hexpand(true);
                    widget.set_wrap(true);
                    if player.lyrics_are_synced() {
                        widget.set_opacity(0.2);
                    }
                    widget.into()
                }
            ),
        );

        lyrics_box.connect_row_activated(clone!(
            #[weak]
            player,
            move |_, row: &gtk::ListBoxRow| {
                let idx = row.index();
                glib::spawn_future_local(async move {
                    player.seek_to_lyric_line(idx).await;
                });
            }
        ));

        // - After having repopulated lyric_lines, player controller will then
        // trigger a current-lyric-line notification (with current_lyric_line
        // at zero), which in turn runs this callback to highlight the initial
        // lyric line.
        self.imp()
            .current_lyric_line_id
            .replace(Some(player.connect_notify_local(
                Some("current-lyric-line"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |player, _| {
                        this.update_lyrics_state(player);
                    }
                ),
            )));

        self.update_album_art(player.current_song(), cache.clone());
        self.imp()
            .cover_changed_id
            .replace(Some(player.connect_closure(
                "cover-changed",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    cache,
                    move |p: Player| {
                        this.update_album_art(p.current_song(), cache.clone());
                    }
                ),
            )));

        imp.show_lyrics.connect_notify_local(
            Some("active"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                player,
                move |_, _| {
                    this.update_lyrics_availability(&player);
                }
            ),
        );

        imp.use_synced_lyrics.connect_notify_local(
            Some("active"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                player,
                move |_, _| {
                    this.update_lyrics_state(&player);
                }
            ),
        );

        imp.refetch_lyrics.connect_clicked(clone!(
            #[weak]
            player,
            #[weak]
            cache,
            #[weak]
            win,
            move |btn| {
                let btn = btn.clone();
                glib::spawn_future_local(async move {
                    if let Some(song) = player.current_song() {
                        btn.set_visible(false);
                        let res = cache.get_lyrics(song.get_info(), true, true, Some(&win)).await;
                        btn.set_visible(true);
                        match res {
                            Ok(Some(lyrics)) => {
                                // Write as if user imported it
                                player.import_lyrics_obj(lyrics);
                            }
                            Ok(None) => {
                                win.send_simple_toast("No lyrics found", 3);
                            }
                            Err(e) => {
                                dbg!(e);
                                win.send_simple_toast("Could not fetch lyrics (internal error)", 3);
                            }
                        }
                    }
                });
            }
        ));

        imp.import_lyrics.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                let (sender, receiver) = async_channel::bounded(1);
                utils::tokio_runtime().spawn(async move {
                    sender
                        .send(
                            SelectedFiles::open_file()
                                .title("Import a .lrc file")
                                .modal(true)
                                .multiple(false)
                                .filter(FileFilter::new("LRC files").glob("*.lrc"))
                                .send()
                                .await
                                .expect("ashpd file open await failure")
                                .response(),
                        )
                        .await
                        .expect("Unable to send response from ashpd back to main thread");
                });
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Some(uri) = receiver
                            .recv()
                            .await
                            .unwrap()
                            .ok() // Once for receiver result and once for ashpd's
                            .and_then(|sel_files| {
                                let uris = sel_files.uris();
                                if uris.is_empty() {
                                    None
                                } else {
                                    Some(uris[0].to_string())
                                }
                            })
                        {
                            let uri = urlencoding::decode(if uri.starts_with("file://") {
                                &uri[7..]
                            } else {
                                &uri
                            })
                            .expect("UTF-8")
                            .into_owned();
                            let text =
                                fs::read_to_string(uri).expect("Unable to read given .lrc file");
                            player.import_lyrics(&text);
                        }
                    }
                ));
            }
        ));

        imp.export_lyrics.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                let (sender, receiver) = async_channel::bounded(1);
                if let Some(text) = player.export_lyrics() {
                    utils::tokio_runtime().spawn(async move {
                        sender
                            .send(
                                SelectedFiles::save_file()
                                    .title("Save lyrics to .lrc file")
                                    .accept_label("Save")
                                    .current_name("lyrics.lrc")
                                    .modal(true)
                                    .filter(FileFilter::new("LRC files").glob("*.lrc"))
                                    .send()
                                    .await
                                    .expect("ashpd file open await failure")
                                    .response(),
                            )
                            .await
                            .expect("Unable to send response from ashpd back to main thread");
                    });
                    glib::spawn_future_local(async move {
                        if let Some(uri) = receiver
                            .recv()
                            .await
                            .unwrap()
                            .ok() // Once for receiver result and once for ashpd's
                            .and_then(|sel_files| {
                                let uris = sel_files.uris();
                                if uris.is_empty() {
                                    None
                                } else {
                                    Some(uris[0].to_string())
                                }
                            })
                        {
                            let uri = urlencoding::decode(if uri.starts_with("file://") {
                                &uri[7..]
                            } else {
                                &uri
                            })
                            .expect("UTF-8")
                            .into_owned();
                            let mut output = File::create(uri)
                                .expect("Unable to open a file for exporting lyrics");
                            output
                                .write_all(text.as_bytes())
                                .expect("Unable to write to opened file");
                        }
                    });
                }
            }
        ));

        imp.clear_lyrics.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                player.clear_lyrics();
            }
        ));

        self.update_lyrics_availability(player);
        self.update_lyrics_state(player);
    }

    fn update_album_art(&self, song: Option<Song>, cache: Rc<Cache>) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cache,
            async move {
                if let Some(song) = song {
                    this.imp().albumart.show_spinner();
                    match cache.get_song_cover(song.get_info(), false).await {
                        Ok(Some(tex)) => this.imp().albumart.show(&tex),
                        Ok(None) => this.imp().albumart.clear(),
                        Err(e) => {
                            this.imp().albumart.clear();
                            dbg!(e);
                        }
                    }
                } else {
                    this.imp().albumart.clear();
                }
            }
        ));
    }

    fn sync_album_art_animation_for_song(
        &self,
        song: Option<Song>,
        position: f64,
        cover_changed: bool,
        automatic_transition: bool,
    ) {
        self.imp().albumart_paintable.set_duration(
            song.and_then(|song| song.get_info().duration)
                .map_or(0.0, |duration| duration.as_secs_f64()),
        );
        if !cover_changed && automatic_transition {
            // MPD may change tracks before GTK draws the disc's final frame. Keep the current
            // angle to avoid a visible jump when the cover stays the same.
            self.rebase_album_art_rotation(position);
        } else {
            // Ignore subsecond playback progress so new artwork starts upright.
            self.reset_album_art_rotation(position.floor());
        }
    }

    fn album_art_rotation_at(&self, position: f64) -> f64 {
        let imp = self.imp();
        (imp.albumart_rotation_offset.get() + position * imp.albumart_rotation_speed.get())
            .rem_euclid(360.0)
    }

    /// Sets the angle at a playback position and records the offset later updates work from.
    fn anchor_album_art_rotation(&self, rotation: f64, position: f64, speed: f64) {
        let imp = self.imp();
        imp.albumart_rotation_speed.set(speed);
        imp.albumart_rotation_offset
            .set(rotation - position * speed);
        self.restart_album_art_animation(rotation, speed);
    }

    /// Starts the track upright, turning a whole number of times over its length.
    fn reset_album_art_rotation(&self, position: f64) {
        let speed = self.imp().albumart_paintable.rotation_speed();
        self.anchor_album_art_rotation((position * speed).rem_euclid(360.0), position, speed);
    }

    /// Keeps the visible angle and adjusts speed so the track still ends upright.
    fn rebase_album_art_rotation(&self, position: f64) {
        let paintable = &self.imp().albumart_paintable;
        let rotation = paintable.rotation().rem_euclid(360.0);
        let speed = paintable.rotation_speed_from(rotation, position);
        self.anchor_album_art_rotation(rotation, position, speed);
    }

    fn resync_album_art_rotation_to(&self, position: f64) {
        self.restart_album_art_animation(
            self.album_art_rotation_at(position),
            self.imp().albumart_rotation_speed.get(),
        );
    }

    fn resync_album_art_rotation(&self) {
        if self.imp().albumart_paintable.circular()
            && let Some(player) = self.imp().player.upgrade()
        {
            self.resync_album_art_rotation_to(player.position());
        } else {
            self.sync_album_art_animation();
        }
    }

    fn restart_album_art_animation(&self, rotation: f64, speed: f64) {
        let imp = self.imp();
        let animation = imp.albumart_animation.get().unwrap();
        animation.set_value_from(rotation);
        animation.set_value_to(rotation + 360.0);
        animation.set_duration((360_000.0 / speed).round() as u32);
        animation.reset();
        self.sync_album_art_animation();
    }

    fn sync_album_art_animation(&self) {
        let should_rotate = self.imp().albumart_paintable.circular()
            && self
                .imp()
                .player
                .upgrade()
                .is_some_and(|player| player.state() == PlaybackState::Playing);
        sync_animation(self.imp().albumart_animation.get().unwrap(), should_rotate);
    }
}

// Slider values 0, 1, and 2 mean 30, 20, and 10 seconds per rotation respectively.
fn rotation_degrees_per_second(setting: f64) -> f64 {
    360.0 / (30.0 - setting * 10.0)
}
