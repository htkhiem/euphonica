use ashpd::desktop::file_chooser::{FileFilter, SelectedFiles};
use glib::{clone, closure_local};
use gtk::{
    CompositeTemplate, gdk,
    glib::{self, Variant},
    prelude::*,
    subclass::prelude::*,
};
use std::{
    cell::{Cell, RefCell},
    fs::{self, File},
    io::Write, rc::Rc,
};

use crate::{
    cache::{Cache, placeholders::{ALBUMART_PLACEHOLDER, EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING}},
    client::{ClientState, state::StickersSupportLevel},
    common::{ImageStack, Rating, Song, paintables::FadePaintable},
    player::seekbar::Seekbar,
    utils::{self, settings_manager},
};

use super::{MpdOutput, PlaybackControls, PlaybackState, Player, VolumeKnob};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/pane.ui")]
    pub struct PlayerPane {
        // Song info
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub albumart: TemplateChild<ImageStack>,
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
        pub import_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub export_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub clear_lyrics: TemplateChild<gtk::Button>,
        #[template_child]
        pub output_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub output_section: TemplateChild<gtk::Box>,
        #[template_child]
        pub output_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub prev_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub vol_knob: TemplateChild<VolumeKnob>,

        // Kept here so we can access it in snapshot()
        pub bg_paintable: FadePaintable,
        pub output_widgets: RefCell<Vec<MpdOutput>>,

        // Index of visible child in output_widgets
        pub current_output: Cell<usize>,
        pub output_count: Cell<usize>,
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

            self.vol_knob
                .bind_property("value", &self.output_btn.get(), "icon-name")
                .transform_to(|_, level: f64| {
                    if level > 75.0 {
                        Some("speaker-4-symbolic")
                    } else if level > 50.0 {
                        Some("speaker-3-symbolic")
                    } else if level > 25.0 {
                        Some("speaker-2-symbolic")
                    } else if level > 0.0 {
                        Some("speaker-1-symbolic")
                    } else {
                        Some("speaker-0-symbolic")
                    }
                })
                .sync_create()
                .build();
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlayerPane {}

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
    }

    pub fn update_lyrics_state(&self, player: &Player) {
        let lyrics_box = self.imp().lyrics_box.get();
        let n_lyric_lines = player.n_lyric_lines();
        if player.lyrics_are_synced() && self.imp().use_synced_lyrics.is_active() {
            let curr_line_idx = player.current_lyric_line();
            for i in 0..n_lyric_lines {
                if let Some(row) = lyrics_box.row_at_index(i as i32) {
                    if let Some(label) = row.child() {
                        label.set_opacity(if i == curr_line_idx { 1.0 } else { 0.2 });
                    }
                }
            }
            // Actually focus on several (currently 1) lines after the
            // current one, such that the next lines are visible too.
            // TODO: Figure out exactly how many lines ahead to focus
            // on, based on lyrics box height, such that the current line
            // is vertically centered.
            let focus_line = if curr_line_idx == 0 {
                0
            } else {
                (curr_line_idx + 1).min(n_lyric_lines - 1)
            };
            if let Some(row) = lyrics_box.row_at_index(focus_line as i32) {
                row.grab_focus();
            }
        } else {
            for i in 0..n_lyric_lines {
                if let Some(row) = lyrics_box.row_at_index(i as i32) {
                    if let Some(label) = row.child() {
                        label.set_opacity(1.0);
                    }
                }
            }
        }
    }

    pub fn setup(&self, player: &Player, cache: Rc<Cache>, client_state: &ClientState) {
        self.setup_volume_knob(player);
        self.bind_state(player, cache, client_state);
        self.imp().playback_controls.setup(player);
        self.imp().seekbar.setup(player);
    }

    fn setup_volume_knob(&self, player: &Player) {
        let knob = self.imp().vol_knob.get();
        knob.set_value(player.mpd_volume() as f64);

        knob.connect_notify_local(
            Some("value"),
            clone!(
                #[weak]
                player,
                move |knob: &VolumeKnob, _| {
                    player.send_set_volume(knob.value().round() as i8);
                }
            ),
        );

        knob.connect_notify_local(
            Some("is-muted"),
            clone!(
                #[weak]
                player,
                move |knob: &VolumeKnob, _| {
                    if knob.is_muted() {
                        player.send_set_volume(0);
                    } else {
                        // Restore previous volume
                        player.send_set_volume(knob.value().round() as i8);
                    }
                }
            ),
        );

        // Only fired for EXTERNAL changes.
        player.connect_closure(
            "volume-changed",
            false,
            closure_local!(|_: Player, val: i8| {
                if !knob.is_muted() {
                    knob.sync_value(val);
                }
            }),
        );

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
                player.cycle_replaygain();
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
    }

    fn bind_state(&self, player: &Player, cache: Rc<Cache>, client_state: &ClientState) {
        let imp = self.imp();
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
                    player.rate_current_song(rating_opt);
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
                player.seek_to_lyric_line(row.index());
            }
        ));

        // - After having repopulated lyric_lines, player controller will then
        // trigger a current-lyric-line notification (with current_lyric_line
        // at zero), which in turn runs this callback to highlight the initial
        // lyric line.
        player.connect_notify_local(
            Some("current-lyric-line"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |player, _| {
                    this.update_lyrics_state(player);
                }
            ),
        );

        self.update_outputs(player);
        player.connect_closure(
            "outputs-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |player: Player| {
                    this.update_outputs(&player);
                }
            ),
        );

        self.update_album_art(player.current_song(), cache.clone());
        player.connect_closure(
            "cover-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)] self,
                #[strong] cache,
                move |p: Player| {
                    this.update_album_art(p.current_song(), cache.clone());
                }
            ),
        );

        imp.prev_output.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.prev_output();
            }
        ));

        imp.next_output.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.next_output();
            }
        ));

        imp.show_lyrics.connect_notify_local(
            Some("active"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                player,
                move |_, _| {
                    println!("show-lyrics changed");
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
                    println!("use-synced-lyrics changed");
                    this.update_lyrics_state(&player);
                }
            ),
        );

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
            #[weak(rename_to = this)] self,
            #[weak] cache,
            async move {
                if let Some(song) = song {
                    this.imp().albumart.show_spinner();
                    match cache.get_song_cover(song.get_info(), true, true).await {
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

    fn update_outputs(&self, player: &Player) {
        let outputs = player.outputs();
        let outputs: Vec<glib::BoxedAnyObject> = (0..outputs.n_items())
            .map(|i| {
                outputs
                    .item(i)
                    .unwrap()
                    .downcast::<glib::BoxedAnyObject>()
                    .unwrap()
            })
            .collect();
        let section = self.imp().output_section.get();
        let stack = self.imp().output_stack.get();
        let new_len = outputs.len();
        if new_len == 0 {
            section.set_visible(false);
        } else {
            section.set_visible(true);
            if new_len > 1 {
                self.imp().prev_output.set_visible(true);
                self.imp().next_output.set_visible(true);
            } else {
                self.imp().prev_output.set_visible(false);
                self.imp().next_output.set_visible(false);
            }
        }
        // Handle new/removed outputs
        // Pretty rare though...
        {
            let mut output_widgets = self.imp().output_widgets.borrow_mut();
            let curr_len = output_widgets.len();
            if curr_len >= new_len {
                // Trim down
                for w in &output_widgets[new_len..] {
                    stack.remove(w);
                }
                output_widgets.truncate(new_len);
                // Overwrite state of the remaining widgets
                // Note that this does not re-populate the stack, so the visible
                // child won't be changed.
                for (w, o) in output_widgets.iter().zip(outputs) {
                    w.update_state(&o.borrow());
                }
            } else {
                // Need to add more widgets
                // Override state of all current widgets. Personal reminder:
                // zip() is auto-truncated to the shorter of the two iters.
                for (w, o) in output_widgets.iter().zip(&outputs) {
                    w.update_state(&o.borrow());
                }
                output_widgets.reserve_exact(new_len - curr_len);
                for o in &outputs[curr_len..] {
                    let w = MpdOutput::from_output(&o.borrow(), player);
                    stack.add_child(&w);
                    output_widgets.push(w);
                }
            }
        }
        let _ = self.imp().output_count.replace(new_len);
        self.set_visible_output(self.imp().current_output.get() as i32);
    }

    fn set_visible_output(&self, new_idx: i32) {
        if self.imp().output_count.get() > 0 {
            let max = self.imp().output_count.get() - 1;
            if new_idx as usize >= max {
                let _ = self.imp().current_output.replace(max);
                self.imp().next_output.set_sensitive(false);
                self.imp().prev_output.set_sensitive(true);
            } else if new_idx <= 0 {
                let _ = self.imp().current_output.replace(0);
                self.imp().next_output.set_sensitive(true);
                self.imp().prev_output.set_sensitive(false);
            } else {
                let _ = self.imp().current_output.replace(new_idx as usize);
                self.imp().next_output.set_sensitive(true);
                self.imp().prev_output.set_sensitive(true);
            }

            // Update stack
            self.imp().output_stack.set_visible_child(
                &self.imp().output_widgets.borrow()[self.imp().current_output.get()],
            );
        }
    }

    fn next_output(&self) {
        self.set_visible_output(self.imp().current_output.get() as i32 + 1);
    }

    fn prev_output(&self) {
        self.set_visible_output(self.imp().current_output.get() as i32 - 1);
    }
}
