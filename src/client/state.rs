use glib::{
    BoxedAnyObject,
    prelude::*,
    subclass::{Signal, prelude::*},
};
use gtk::glib;
use std::{cell::Cell, sync::OnceLock};

use crate::common::{Album, Artist};

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "EuphonicaConnectionState")]
pub enum ConnectionState {
    #[default]
    NotConnected,
    ConnectionRefused,
    SocketNotFound,
    Connecting,
    Unauthenticated, // No password, or provided password is incorrect or insufficiently privileged
    CredentialStoreError, // Internal error
    WrongPassword,   // The provided password does not match any of the configured passwords
    Connected,
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, glib::Enum, PartialOrd, Ord)]
#[enum_type(name = "EuphonicaStickersSupportLevel")]
pub enum StickersSupportLevel {
    #[default]
    Disabled, // Sticker DB has not been set up
    SongsOnly, // MPD <0.23.15 only supports attaching stickers directly to songs
    All,       // MPD 0.24+ also supports attaching stickers to tags
}

mod imp {
    use gio::glib::derived_properties;
    use ::glib::Properties;
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecEnum, ParamSpecUInt64};

    use super::*;
    use once_cell::sync::Lazy;

    #[derive(Debug, Default, Properties)]
    #[properties(wrapper_type = super::ClientState)]
    pub struct ClientState {
        #[property(get, set, default)]
        pub connection_state: Cell<ConnectionState>,
        // Used to indicate that the background client is busy.
        #[property(get)]
        pub n_fg_tasks: Cell<u64>,
        #[property(get)]
        pub n_bg_tasks: Cell<u64>,
        #[property(get, set)]
        pub supports_playlists: Cell<bool>,
        #[property(get, set, default)]
        pub stickers_support_level: Cell<StickersSupportLevel>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClientState {
        const NAME: &'static str = "EuphonicaClientState";
        type Type = super::ClientState;

        fn new() -> Self {
            Self {
                connection_state: Cell::default(),
                n_fg_tasks: Cell::new(0),
                n_bg_tasks: Cell::new(0),
                stickers_support_level: Cell::default(),
                supports_playlists: Cell::new(true)
            }
        }
    }

    #[derived_properties]
    impl ObjectImpl for ClientState {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("idle")
                        .param_types([
                            BoxedAnyObject::static_type(), // mpd::Subsystem::to_str
                        ])
                        .build()
                ]
            })
        }
    }
}

glib::wrapper! {
    pub struct ClientState(ObjectSubclass<imp::ClientState>);
}

impl Default for ClientState {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ClientState {
    // Convenience emit wrappers
    pub fn emit_boxed_result<T: 'static>(&self, signal_name: &str, to_box: T) {
        // T must be owned or static
        self.emit_by_name::<()>(signal_name, &[&BoxedAnyObject::new(to_box)]);
    }

    pub fn inc_bg(&self) {
        self.imp().n_bg_tasks.set(self.imp().n_bg_tasks.get() + 1);
        self.notify("n-background-tasks");
    }

    pub fn dec_bg(&self) {
        self.imp().n_bg_tasks.set(self.imp().n_bg_tasks.get() - 1);
        self.notify("n-background-tasks");
    }

    pub fn inc_fg(&self) {
        self.imp().n_fg_tasks.set(self.imp().n_fg_tasks.get() + 1);
        self.notify("n-background-tasks");
    }

    pub fn dec_fg(&self) {
        self.imp().n_fg_tasks.set(self.imp().n_fg_tasks.get() - 1);
        self.notify("n-background-tasks");
    }
}
