
use gtk::{
    gio, glib::{self, closure_local}, prelude::*, subclass::prelude::*, CompositeTemplate
};
use glib::{
    clone,
    Object,
};

use super::{
    PlaybackFlow, PlaybackState, Player, Seekbar
};

// All playback controls are grouped in this custom widget since we'll need to draw
// them in two different places: the bottom bar and the Now Playing pane. Only one
// should be visible at any time though.
mod imp {
    use std::cell::Cell;

    use glib::Properties;

    use super::*;

    #[derive(Default, Properties, CompositeTemplate)]
    #[properties(wrapper_type = super::PlaybackControls)]
    #[template(resource = "/org/euphonica/Euphonica/gtk/player/playback-controls.ui")]
    pub struct PlaybackControls {
        #[template_child]
        pub flow_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_pause_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub play_pause_symbol: TemplateChild<gtk::Stack>,  // inside the play/pause button
        #[template_child]
        pub prev_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub next_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub random_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub seekbar: TemplateChild<Seekbar>,
        #[property(get, set)]
        pub playing: Cell<bool>,
        #[property(get, set)]
        pub collapsed: Cell<bool>  // If true, will only show prev track, play/pause and next track
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
    impl ObjectImpl for PlaybackControls {
        fn constructed(&self) {
            self.parent_constructed();

            let obj = self.obj();

            // Hide certain widgets when not in expanded mode
            obj
                .bind_property(
                    "collapsed",
                    &self.seekbar.get(),
                    "visible"
                )
                .transform_to(|_, val: bool| {Some(!val)})
                .sync_create()
                .build();
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlaybackControls {}

    // Trait shared by all boxes
    impl BoxImpl for PlaybackControls {}
}

glib::wrapper! {
    pub struct PlaybackControls(ObjectSubclass<imp::PlaybackControls>)
    @extends gtk::Widget,
    @implements gio::ActionGroup, gio::ActionMap;
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
            .bind_property(
                "playback-state",
                &play_pause_symbol,
                "visible-child-name"
            )
            .transform_to(
                |_, state: PlaybackState| {
                    match state {
	                    PlaybackState::Playing => {
	                        Some("play")
                        },
	                    PlaybackState::Paused | PlaybackState::Stopped => {
	                        Some("pause")
	                    },
	                }
                }
            )
            .sync_create()
            .build();

        let flow_btn = imp.flow_btn.get();
        player
            .bind_property(
                "playback-flow",
                &flow_btn,
                "icon-name"
            )
            .transform_to(|_, flow: PlaybackFlow| { Some(flow.icon_name()) })
            .sync_create()
            .build();
        player
            .bind_property(
                "playback-flow",
                &flow_btn,
                "tooltip-text"
            )
            // TODO: translatable
            .transform_to(|_, flow: PlaybackFlow| { Some(format!("Playback Mode: {}", flow.description())) })
            .sync_create()
            .build();
        flow_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                player.cycle_playback_flow();
            }
        ));
        self.imp().prev_btn.connect_clicked(
            clone!(
                #[strong]
                player,
                move |_| {
                    player.prev_song();
                }
            )
        );
        self.imp().play_pause_btn.connect_clicked(
            clone!(
                #[weak]
                player,
                move |_| {
                    player.toggle_playback()
                }
            )
        );
        self.imp().next_btn.connect_clicked(
            clone!(
                #[strong]
                player,
                move |_| {
                    player.next_song();
                }
            )
        );
        let shuffle_btn = imp.random_btn.get();
        shuffle_btn
            .bind_property(
                "active",
                player,
                "random"
            )
            .bidirectional()
            .sync_create()
            .build();

        self.setup_seekbar(player);
    }

    pub fn seekbar(&self) -> Seekbar {
        self.imp().seekbar.get()
    }

    fn setup_seekbar(&self, player: &Player) {
        let seekbar = self.imp().seekbar.get();
        seekbar.connect_closure(
            "pressed",
            false,
            closure_local!(
                #[weak]
                player,
                move |_: Seekbar| {
                    player.block_polling();
                    player.stop_polling();
                }
            )
        );

        seekbar.connect_closure(
            "released",
            false,
            closure_local!(
                #[weak]
                player,
                move |_: Seekbar| {
                    player.unblock_polling();
                    player.send_seek();
                    // Player will start polling again on next status update,
                    // which should be triggered by us seeking.
                }
            )
        );

        seekbar.set_duration(player.duration() as f64);
        player.connect_notify_local(
            Some("duration"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |player, _| {
                    this.imp().seekbar.set_duration(player.duration() as f64);
                }
            ),
        );
        player
            .bind_property(
                "position",
                &seekbar,
                "position"
            )
            .sync_create()
            .bidirectional()
            .build();

        player
            .bind_property(
                "duration",
                &seekbar,
                "duration"
            )
            .sync_create()
            .build();
    }
}
