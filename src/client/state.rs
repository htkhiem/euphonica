use glib::{
    BoxedAnyObject,
    prelude::*,
    subclass::{Signal, prelude::*}, Properties, derived_properties
};
use std::{cell::Cell, sync::OnceLock};


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
    use super::*;

    #[derive(Debug, Default, Properties)]
    #[properties(wrapper_type = super::ClientState)]
    pub struct ClientState {
        #[property(get, set, builder(ConnectionState::default()))]
        pub connection_state: Cell<ConnectionState>,
        // Used to indicate that the background client is busy.
        #[property(get)]
        pub n_fg_tasks: Cell<u64>,
        #[property(get)]
        pub n_done_fg_tasks: Cell<u64>,
        #[property(get)]
        pub pct_done_fg_tasks: Cell<f64>,
        #[property(get)]
        pub n_bg_tasks: Cell<u64>,
        #[property(get)]
        pub n_done_bg_tasks: Cell<u64>,
        #[property(get)]
        pub pct_done_bg_tasks: Cell<f64>,
        #[property(get, set)]
        pub supports_playlists: Cell<bool>,
        #[property(get)]
        pub has_pending: Cell<bool>,
        #[property(get, set, builder(StickersSupportLevel::default()))]
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
                n_done_fg_tasks: Cell::new(0),
                pct_done_fg_tasks: Cell::new(0.0),
                n_bg_tasks: Cell::new(0),
                n_done_bg_tasks: Cell::new(0),
                pct_done_bg_tasks: Cell::new(0.0),
                has_pending: Cell::new(false),
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

    #[inline]
    fn update_has_pending(&self) {
        let new = self.imp().n_bg_tasks.get() + self.imp().n_fg_tasks.get() > 0;
        let old = self.imp().has_pending.replace(new);
        if old != new {
            self.notify("has-pending");
        }
    }

    #[inline]
    fn update_pct_done_bg(&self) {
        let n_bg_tasks = self.imp().n_bg_tasks.get();
        let new_pct = if n_bg_tasks == 0 {
            0.0
        } else {
            self.imp().n_done_bg_tasks.get() as f64 / n_bg_tasks as f64
        };
        let old_pct = self.imp().pct_done_bg_tasks.replace(new_pct);
        if old_pct != new_pct {
            self.notify("pct-done-bg-tasks");
        }
        self.update_has_pending();
    }

    pub fn inc_bg(&self) {
        self.imp().n_bg_tasks.set(self.imp().n_bg_tasks.get() + 1);
        self.notify("n-bg-tasks");
        self.update_pct_done_bg();
    }

    pub fn dec_bg(&self) {
        let curr_done = self.imp().n_done_bg_tasks.get();
        let all = self.imp().n_bg_tasks.get();
        if curr_done + 1 == all {
            // Completed everything, reset to zero
            self.imp().n_done_bg_tasks.set(0);
            self.imp().n_bg_tasks.set(0);
            self.notify("n-done-bg-tasks");
            self.notify("n-bg-tasks");
        } else {
            self.imp().n_done_bg_tasks.set(curr_done + 1);
            self.notify("n-done-bg-tasks");
        }
        self.update_pct_done_bg();
    }

    #[inline]
    fn update_pct_done_fg(&self) {
        let n_fg_tasks = self.imp().n_fg_tasks.get();
        let new_pct = if n_fg_tasks == 0 {
            0.0
        } else {
            self.imp().n_done_fg_tasks.get() as f64 / n_fg_tasks as f64
        };
        let old_pct = self.imp().pct_done_fg_tasks.replace(new_pct);
        if old_pct != new_pct {
            self.notify("pct-done-fg-tasks");
        }
        self.update_has_pending();
    }

    pub fn inc_fg(&self) {
        self.imp().n_fg_tasks.set(self.imp().n_fg_tasks.get() + 1);
        self.notify("n-fg-tasks");
        self.update_pct_done_fg();
    }

    pub fn dec_fg(&self) {
        let curr_done = self.imp().n_done_fg_tasks.get();
        let all = self.imp().n_fg_tasks.get();
        if curr_done + 1 == all {
            // Completed everything, reset to zero
            self.imp().n_done_fg_tasks.set(0);
            self.imp().n_fg_tasks.set(0);
            self.notify("n-done-fg-tasks");
            self.notify("n-fg-tasks");
        } else {
            self.imp().n_done_fg_tasks.set(curr_done + 1);
            self.notify("n-done-fg-tasks");
        }
        self.update_pct_done_fg();
    }
}
