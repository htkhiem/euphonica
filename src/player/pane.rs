use glib::{clone, closure_local};
use gtk::{
    gdk,
    glib::{self, Variant},
    prelude::*,
    subclass::prelude::*,
    CompositeTemplate,
};
use std::cell::{Cell, RefCell};

use crate::{
    cache::placeholders::ALBUMART_PLACEHOLDER,
    common::paintables::FadePaintable,
    utils::settings_manager,
};

use super::{MpdOutput, PlaybackControls, PlaybackState, Player, VolumeKnob};

mod imp {

    use crate::player::seekbar::Seekbar;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/pane.ui")]
    pub struct PlayerPane {
        // Song info
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub albumart: TemplateChild<gtk::Picture>,
        #[template_child]
        pub song_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub album: TemplateChild<gtk::Label>,

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

        // Bottom: output info, volume control & quality
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
            let settings = settings_manager().child("ui");
            let knob = self.vol_knob.get();
            settings
                .bind("vol-knob-unit", &knob, "use-dbfs")
                .get_only()
                .mapping(|v: &Variant, _| {
                    Some((v.get::<String>().unwrap().as_str() == "decibels").to_value())
                })
                .build();

            settings
                .bind("vol-knob-sensitivity", &knob, "sensitivity")
                .mapping(|v: &Variant, _| Some(v.get::<f64>().unwrap().to_value()))
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

    pub fn setup(&self, player: &Player) {
        self.setup_volume_knob(player);
        self.bind_state(player);
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
                knob.sync_value(val);
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

    fn bind_state(&self, player: &Player) {
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
            .sync_create()
            .build();

        let artist = imp.artist.get();
        player
            .bind_property("artist", &artist, "label")
            .sync_create()
            .build();

        let lyric_lines = player.lyrics();
        let lyrics_window = imp.lyrics_window.get();
        lyric_lines
            .bind_property("n-items", &lyrics_window, "visible")
            .transform_to(|_, n_items: u32| Some(n_items > 0))
            .sync_create()
            .build();

        let lyrics_box = imp.lyrics_box.get();
        lyrics_box.bind_model(Some(&lyric_lines), |line| {
            let widget = gtk::Label::new(Some(&line.downcast_ref::<gtk::StringObject>().unwrap().string()));
            widget.set_halign(gtk::Align::Center);
            widget.set_hexpand(true);
            widget.set_wrap(true);
            widget.into()
        });
        player.connect_notify_local(Some("current-lyric-line"), clone!(
            #[weak]
            lyrics_box,
            move |player, _| {
                if let Some(row) = lyrics_box.row_at_index(player.current_lyric_line() as i32) {
                    row.grab_focus();
                }
            }
        ));

        self.update_outputs(&player);
        player.connect_closure(
            "outputs-changed",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |player: Player| {
                    this.update_outputs(&player);
                }
            ),
        );

        self.update_album_art(player.current_song_album_art(false));
        player.connect_notify_local(
            Some("album-art"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                player,
                move |_, _| {
                    this.update_album_art(player.current_song_album_art(false));
                }
            ),
        );

        self.imp().prev_output.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.prev_output();
            }
        ));

        self.imp().next_output.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.next_output();
            }
        ));
    }

    fn update_album_art(&self, tex: Option<gdk::Texture>) {
        // Use high-resolution version here
        // Update cover paintable
        if tex.is_some() {
            self.imp().albumart.set_paintable(tex.as_ref());
        } else {
            self.imp()
                .albumart
                .set_paintable(Some(&*ALBUMART_PLACEHOLDER));
        }
    }

    fn update_outputs(&self, player: &Player) {
        let outputs = player.outputs();
        let outputs: Vec<glib::BoxedAnyObject> = (0..outputs.n_items())
            .map(|i| outputs.item(i).unwrap().downcast::<glib::BoxedAnyObject>().unwrap()).collect();
        let section = self.imp().output_section.get();
        let stack = self.imp().output_stack.get();
        let new_len = outputs.len() as usize;
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
                    let w = MpdOutput::from_output(&o.borrow(), &player);
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
