use std::cell::{Cell, RefCell};
use gtk::{
    gdk, glib::{self, Variant}, prelude::*, subclass::prelude::*, CompositeTemplate
};
use glib::{
    clone,
    closure_local,
    BoxedAnyObject
};
use mpd::output::Output;

use crate::{
    cache::placeholders::ALBUMART_PLACEHOLDER, common::{Marquee, QualityGrade}, utils::settings_manager
};

use super::{
    Player, PlaybackControls,
    PlaybackState, VolumeKnob, MpdOutput
};

mod imp {
    use std::sync::OnceLock;

    use glib::{subclass::Signal, Properties};

    use super::*;

    #[derive(Default, Properties, CompositeTemplate)]
    #[properties(wrapper_type = super::PlayerBar)]
    #[template(resource = "/org/euphonica/Euphonica/gtk/player/bar.ui")]
    pub struct PlayerBar {
        // Left side: current song info
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub albumart: TemplateChild<gtk::Image>,
        #[template_child]
        pub song_name: TemplateChild<Marquee>,
        #[template_child]
        pub artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub album: TemplateChild<gtk::Label>,
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub format_desc: TemplateChild<gtk::Label>,

        // Centre: playback controls
        #[template_child]
        pub playback_controls: TemplateChild<PlaybackControls>,

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
        pub collapsed: Cell<bool>  // If true, will turn into a minimal bar that can fit narrow displays (e.g., phones)
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

            obj
                .bind_property(
                    "collapsed",
                    &self.albumart.get(),
                    "pixel-size"
                )
                .transform_to(|_, collapsed: bool| {
                    if collapsed {
                        Some(48)
                    } else {
                        Some(96)
                    }
                })
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.playback_controls.get(),
                    "collapsed"
                )
                .sync_create()
                .build();

            // Hide certain widgets when in compact mode
            obj
                .bind_property(
                    "collapsed",
                    &self.album.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.quality_grade.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.format_desc.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.output_section.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.vol_knob.get(),
                    "visible"
                )
                .invert_boolean()
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.goto_pane.get(),
                    "visible"
                )
                .sync_create()
                .build();

            obj
                .bind_property(
                    "collapsed",
                    &self.playback_controls.get(),
                    "width-request"
                )
                .transform_to(|_, collapsed: bool| {
                    if collapsed {
                        None
                    }
                    else {
                        // When the seekbar is visible, prevent the controls from getting too narrow.
                        Some(320)
                    }
                })
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
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("goto-pane-clicked")
                        .build()
                ]
            })
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlayerBar {}

    // Trait shared by all boxes
    impl BoxImpl for PlayerBar {}
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

    pub fn setup(&self, player: Player) {
        self.setup_volume_knob(player.clone());
        self.bind_state(player.clone());
        self.imp().playback_controls.setup(player);
    }

    fn setup_volume_knob(&self, player: Player) {
        let settings = settings_manager().child("player");
        let knob = self.imp().vol_knob.get();
        knob.setup();

        settings
            .bind(
                "vol-knob-unit",
                &knob,
                "use-dbfs"
            )
            .get_only()
            .mapping(|v: &Variant, _| {
                Some((v.get::<String>().unwrap().as_str() == "decibels").to_value())
            })
            .build();

        settings
            .bind(
                "vol-knob-sensitivity",
                &knob,
                "sensitivity"
            )
            .mapping(|v: &Variant, _| { Some(v.get::<f64>().unwrap().to_value())})
            .build();

        knob.connect_notify_local(
            Some("value"),
            clone!(
                #[weak]
                player,
                move |knob: &VolumeKnob, _| {
                    player.send_set_volume(knob.value().round() as i8);
                }
            )
        );

        knob.connect_notify_local(
            Some("is-muted"),
            clone!(
                #[weak]
                player,
                move |knob: &VolumeKnob, _| {
                    if knob.is_muted() {
                        player.send_set_volume(0);
                    }
                    else {
                        // Restore previous volume
                        player.send_set_volume(knob.value().round() as i8);
                    }
                }
            )
        );

        // Only fired for EXTERNAL changes.
        player.connect_closure(
            "volume-changed",
            false,
            closure_local!(
                |_: Player, val: i8| {
                    knob.sync_value(val);
                }
            )
        );
    }

    fn bind_state(&self, player: Player) {
        let imp = self.imp();
        let info_box = imp.info_box.get();
        player
            .bind_property(
                "playback-state",
                &info_box,
                "visible"
            )
            .transform_to(|_, state: PlaybackState| {
                Some(state != PlaybackState::Stopped)
            })
            .sync_create()
            .build();

        let song_name = imp.song_name.get().label();
        player
            .bind_property(
                "title",
                &song_name,
                "label"
            )
            .sync_create()
            .build();

        let album = imp.album.get();
        player
            .bind_property(
                "album",
                &album,
                "label"
            )
            .sync_create()
            .build();

        let artist = imp.artist.get();
        player
            .bind_property(
                "artist",
                &artist,
                "label"
            )
            .sync_create()
            .build();

        let quality_grade = imp.quality_grade.get();
        player
            .bind_property(
                "quality-grade",
                &quality_grade,
                "icon-name"
            )
            .transform_to(|_, grade: QualityGrade| {
                Some(grade.to_icon_name())}
            )
            .sync_create()
            .build();

        player
            .bind_property(
                "quality-grade",
                &quality_grade,
                "visible"
            )
            .transform_to(|_, grade: QualityGrade| {
                Some(grade != QualityGrade::Lossy)
            })
            .sync_create()
            .build();

        let format_desc = imp.format_desc.get();
        player
            .bind_property(
                "format-desc",
                &format_desc,
                "label"
            )
            .sync_create()
            .build();

        player.connect_closure(
            "outputs-changed",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |player: Player, outputs: BoxedAnyObject| {
                    this.update_outputs(player, outputs.borrow::<Vec<Output>>().as_ref());
                }
            )
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
            )
        );

        self.imp().prev_output.connect_clicked(
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.prev_output();
                }
            )
        );

        self.imp().next_output.connect_clicked(
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.next_output();
                }
            )
        );
    }

    fn update_album_art(&self, tex: Option<gdk::Texture>) {
        // Use high-resolution version here
        // Update cover paintable
        if tex.is_some() {
            self.imp().albumart.set_paintable(tex.as_ref());
        }
        else {
            self.imp().albumart.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
        }
    }

    fn update_outputs(&self, player: Player, outputs: &[Output]) {
        let section = self.imp().output_section.get();
        let stack = self.imp().output_stack.get();
        let new_len = outputs.len();
        if new_len == 0 {
            section.set_visible(false);
        }
        else {
            section.set_visible(true);
            if new_len > 1 {
                self.imp().prev_output.set_visible(true);
                self.imp().next_output.set_visible(true);
            }
            else {
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
                    w.update_state(o);
                }
            }
            else {
                // Need to add more widgets
                // Override state of all current widgets. Personal reminder:
                // zip() is auto-truncated to the shorter of the two iters.
                for (w, o) in output_widgets.iter().zip(outputs) {
                    w.update_state(o);
                }
                output_widgets.reserve_exact(new_len - curr_len);
                for o in &outputs[curr_len..] {
                    let w = MpdOutput::from_output(o, &player);
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
            }
            else if new_idx <= 0 {
                let _ = self.imp().current_output.replace(0);
                self.imp().next_output.set_sensitive(true);
                self.imp().prev_output.set_sensitive(false);
            }
            else {
                let _ = self.imp().current_output.replace(new_idx as usize);
                self.imp().next_output.set_sensitive(true);
                self.imp().prev_output.set_sensitive(true);
            }

            // Update stack
            self.imp().output_stack.set_visible_child(
                &self.imp().output_widgets.borrow()[
                    self.imp().current_output.get()
                ]
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
