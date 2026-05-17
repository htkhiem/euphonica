use glib::{SignalHandlerId, WeakRef, clone, closure_local};
use gtk::{
    CompositeTemplate,
    glib::{self, Properties, subclass::Signal},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::OnceLock;

use crate::{
    cache::{
        Cache,
        placeholders::{EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING},
    },
    common::{ImageStack, Marquee, Song},
    player::{ratio_center_box::RatioCenterBox, seekbar2::Seekbar},
    utils::settings_manager,
};

use super::{MpdOutput, OutputControls, PlaybackControls, PlaybackState, Player, VolumeKnob};

mod imp {
    use super::*;

    #[derive(Default, Properties, CompositeTemplate)]
    #[properties(wrapper_type = super::PlayerBar)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/bar.ui")]
    pub struct PlayerBar {
        #[template_child]
        pub multi_layout_view: TemplateChild<adw::MultiLayoutView>,
        #[template_child]
        pub full_center_box: TemplateChild<RatioCenterBox>,
        // Left side: current song info
        #[template_child]
        pub albumart: TemplateChild<ImageStack>,
        #[template_child]
        pub info_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
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
        pub output_controls: TemplateChild<OutputControls>,
        #[template_child]
        pub goto_pane: TemplateChild<gtk::Button>,
        #[template_child]
        pub vol_knob: TemplateChild<VolumeKnob>,

        pub player: WeakRef<Player>,
        pub cover_changed_id: RefCell<Option<SignalHandlerId>>,
        #[property(get, set)]
        pub layout: Cell<u32>, // 0: micro, 1: mini, 2: full. TODO: turn into enum.
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

            obj.bind_property("layout", &self.multi_layout_view.get(), "layout-name")
                .transform_to(|_, layout: u32| match layout {
                    0 => Some("micro".to_value()),
                    1 => Some("mini".to_value()),
                    2 => Some("full".to_value()),
                    _ => unimplemented!(),
                })
                .sync_create()
                .build();

            obj.bind_property("layout", &self.seekbar.get(), "visible")
                .transform_to(|_, layout: u32| Some((layout > 0).to_value()))
                .sync_create()
                .build();

            // Hide certain widgets when in compact mode
            obj.bind_property("layout", &self.album.get(), "visible")
                .transform_to(|_, layout: u32| Some((layout > 1).to_value()))
                .sync_create()
                .build();

            obj.bind_property("layout", &self.output_section.get(), "visible")
                .transform_to(|_, layout: u32| Some((layout > 1).to_value()))
                .sync_create()
                .build();

            obj.bind_property("layout", &self.vol_knob.get(), "visible")
                .transform_to(|_, layout: u32| Some((layout > 1).to_value()))
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

        fn dispose(&self) {
            if let Some(player) = self.player.upgrade() {
                if let Some(id) = self.cover_changed_id.take() {
                    player.disconnect(id);
                }
            }
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

    pub fn setup(&self, player: &Player, cache: Rc<Cache>) {
        self.imp().player.set(Some(player));
        self.bind_state(player, cache);
        self.imp().playback_controls.setup(player);
        self.imp().output_controls.setup(player);
        self.imp().seekbar.setup(player);
    }

    fn bind_state(&self, player: &Player, cache: Rc<Cache>) {
        let imp = self.imp();

        self.imp().vol_knob.setup(player);

        let infobox_revealer = imp.infobox_revealer.get();
        let seekbar_revealer = imp.seekbar_revealer.get();
        // Also controls seekbar revealer, see binding in bar.ui
        player
            .bind_property("playback-state", &infobox_revealer, "reveal_child")
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

        self.update_album_art(player.current_song(), cache.clone());
        self.imp()
            .cover_changed_id
            .replace(Some(player.connect_closure(
                "cover-changed",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    #[weak]
                    cache,
                    move |p: Player| {
                        this.update_album_art(p.current_song(), cache.clone());
                    }
                ),
            )));
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
}
