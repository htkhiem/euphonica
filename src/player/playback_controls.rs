use glib::{clone, Object};
use gtk::{
    glib::{self},
    prelude::*,
    subclass::prelude::*,
    CompositeTemplate,
};

use super::{PlaybackFlow, PlaybackState, Player};

// All playback controls are grouped in this custom widget since we'll need to draw
// them in two different places: the bottom bar and the Now Playing pane. Only one
// should be visible at any time though.
mod imp {
    use std::cell::Cell;

    use glib::Properties;

    use super::*;

    #[derive(Default, Properties, CompositeTemplate)]
    #[properties(wrapper_type = super::PlaybackControls)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/playback-controls.ui")]
    pub struct PlaybackControls {
        #[template_child]
        pub flow_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_pause_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_pause_symbol: TemplateChild<gtk::Stack>, // inside the play/pause button
        #[template_child]
        pub prev_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub random_btn: TemplateChild<gtk::ToggleButton>,
        #[property(get, set)]
        pub playing: Cell<bool>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PlaybackControls {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPlaybackControls";
        type Type = super::PlaybackControls;
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
    impl ObjectImpl for PlaybackControls {}

    // Trait shared by all widgets
    impl WidgetImpl for PlaybackControls {}

    // Trait shared by all boxes
    impl BoxImpl for PlaybackControls {}
}

glib::wrapper! {
    pub struct PlaybackControls(ObjectSubclass<imp::PlaybackControls>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for PlaybackControls {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaybackControls {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn setup(&self, player: &Player) {
        let imp = self.imp();
        // Set up buttons
        let play_pause_symbol = imp.play_pause_symbol.get();
        player
            .bind_property("playback-state", &play_pause_symbol, "visible-child-name")
            .transform_to(|_, state: PlaybackState| match state {
                PlaybackState::Playing => Some("play"),
                PlaybackState::Paused | PlaybackState::Stopped => Some("pause"),
            })
            .sync_create()
            .build();

        let flow_btn = imp.flow_btn.get();
        player
            .bind_property("playback-flow", &flow_btn, "icon-name")
            .transform_to(|_, flow: PlaybackFlow| Some(flow.icon_name()))
            .sync_create()
            .build();
        player
            .bind_property("playback-flow", &flow_btn, "tooltip-text")
            // TODO: translatable
            .transform_to(|_, flow: PlaybackFlow| {
                Some(format!("Playback Mode: {}", flow.description()))
            })
            .sync_create()
            .build();
        flow_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |_| player.cycle_playback_flow()
        ));
        self.imp().prev_btn.connect_clicked(clone!(
            #[strong]
            player,
            move |_| player.prev_song(true)
        ));
        self.imp().play_pause_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |_| player.toggle_playback()
        ));
        self.imp().next_btn.connect_clicked(clone!(
            #[strong]
            player,
            move |_| player.next_song(true)
        ));
        let shuffle_btn = imp.random_btn.get();
        shuffle_btn
            .bind_property("active", player, "random")
            .bidirectional()
            .sync_create()
            .build();
    }
}
