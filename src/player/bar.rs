use glib::{clone, closure_local, BoxedAnyObject};
use gtk::{
    gdk,
    glib::{self, Variant},
    prelude::*,
    subclass::prelude::*,
    CompositeTemplate,
};
use mpd::output::Output;
use std::cell::{Cell, RefCell};

use crate::{
    cache::placeholders::ALBUMART_PLACEHOLDER,
    common::{Marquee, QualityGrade},
    utils::settings_manager,
};

use super::{
    MpdOutput,
    PlaybackControls,
    PlaybackState,
    Player,
    VolumeKnob
};

mod imp {
    use std::sync::OnceLock;

    use glib::{subclass::Signal, Properties};

    use crate::player::{ratio_center_box::RatioCenterBox, seekbar::Seekbar};

    use super::*;

    #[derive(Default, Properties, CompositeTemplate)]
    #[properties(wrapper_type = super::PlayerBar)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/bar.ui")]
    pub struct PlayerBar {
        #[template_child]
        pub multi_layout_view: TemplateChild<adw::MultiLayoutView>,
        #[template_child]
        pub full_layout_box: TemplateChild<RatioCenterBox>,
        // Left side: current song info
        #[template_child]
        pub albumart: TemplateChild<gtk::Image>,
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub mini_infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub song_name: TemplateChild<Marquee>,
        #[template_child]
        pub artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub album: TemplateChild<gtk::Label>,

        // Centre: playback controls
        #[template_child]
        pub playback_controls: TemplateChild<PlaybackControls>,
        #[template_child]
        pub seekbar_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub seekbar: TemplateChild<Seekbar>,

        // Right side: output info & volume control
        #[template_child]
        pub output_section: TemplateChild<gtk::Box>,
        #[template_child]
        pub output_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub prev_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_output: TemplateChild<gtk::Button>,
        #[template_child]
        pub goto_pane: TemplateChild<gtk::Button>,
        #[template_child]
        pub vol_knob: TemplateChild<VolumeKnob>,

        pub output_widgets: RefCell<Vec<MpdOutput>>,
        // Index of visible child in output_widgets
        pub current_output: Cell<usize>,
        pub output_count: Cell<usize>,
        #[property(get, set)]
        pub collapsed: Cell<bool>, // If true, will turn into a minimal bar that can fit narrow displays (e.g., phones)
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PlayerBar {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPlayerBar";
        type Type = super::PlayerBar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    #[glib::derived_properties]
    impl ObjectImpl for PlayerBar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            obj.bind_property("collapsed", &self.multi_layout_view.get(), "layout-name")
                .transform_to(
                    |_, collapsed: bool| {
                        if collapsed {
                            Some("mini")
                        } else {
                            Some("full")
                        }
                    },
                )
                .sync_create()
                .build();

            obj.bind_property("collapsed", &self.albumart.get(), "pixel-size")
                .transform_to(
                    |_, collapsed: bool| {
                        if collapsed {
                            Some(48)
                        } else {
                            Some(115)
                        }
                    },
                )
                .sync_create()
                .build();

            obj.bind_property("collapsed", &self.seekbar.get(), "visible")
                .invert_boolean()
                .sync_create()
                .build();

            // Hide certain widgets when in compact mode
            obj.bind_property("collapsed", &self.album.get(), "visible")
                .invert_boolean()
                .sync_create()
                .build();

            obj.bind_property("collapsed", &self.output_section.get(), "visible")
                .invert_boolean()
                .sync_create()
                .build();

            obj.bind_property("collapsed", &self.vol_knob.get(), "visible")
                .invert_boolean()
                .sync_create()
                .build();

            obj.bind_property("collapsed", &self.goto_pane.get(), "visible")
                .sync_create()
                .build();

            self.goto_pane.connect_clicked(clone!(
                #[weak(rename_to = this)]
                obj,
                move |_| {
                    this.emit_by_name::<()>("goto-pane-clicked", &[]);
                }
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("goto-pane-clicked").build()])
        }
    }

    impl WidgetImpl for PlayerBar {}

    impl BoxImpl for PlayerBar {}

    impl PlayerBar {}
}

glib::wrapper! {
    pub struct PlayerBar(ObjectSubclass<imp::PlayerBar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for PlayerBar {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlayerBar {
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
        let settings = settings_manager().child("ui");
        let knob = self.imp().vol_knob.get();
        knob.setup();

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
    }

    fn bind_state(&self, player: &Player) {
        let imp = self.imp();

        let infobox_revealer = imp.infobox_revealer.get();
        let mini_infobox_revealer = imp.mini_infobox_revealer.get();
        let seekbar_revealer = imp.seekbar_revealer.get();
        // Also controls seekbar revealer, see binding in bar.ui
        player
            .bind_property("playback-state", &infobox_revealer, "reveal_child")
            .transform_to(|_, state: PlaybackState| Some(state != PlaybackState::Stopped))
            .sync_create()
            .build();

        player
            .bind_property("playback-state", &mini_infobox_revealer, "reveal_child")
            .transform_to(|_, state: PlaybackState| Some(state != PlaybackState::Stopped))
            .sync_create()
            .build();

        player
            .bind_property("playback-state", &seekbar_revealer, "reveal_child")
            .transform_to(|_, state: PlaybackState| Some(state != PlaybackState::Stopped))
            .sync_create()
            .build();

        let song_name = imp.song_name.get().label();
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

        self.update_album_art(player.current_song_album_art(true));
        player.connect_notify_local(
            Some("album-art"),
            clone!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                player,
                move |_, _| {
                    this.update_album_art(player.current_song_album_art(true));
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
