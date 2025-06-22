use glib::{
    prelude::*,
    subclass::{prelude::*, Signal},
    BoxedAnyObject,
};
use gtk::glib;
use std::{cell::Cell, sync::OnceLock};

use crate::common::{Album, Artist};

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, glib::Enum)]
#[enum_type(name = "EuphonicaConnectionState")]
pub enum ConnectionState {
    #[default]
    NotConnected,
    Connecting,
    Unauthenticated, // Either no password is provided or the one provided is insufficiently privileged
    CredentialStoreError, // Cannot access underlying credential store to fetch or save password
    WrongPassword,   // The provided password does not match any of the configured passwords
    Connected,
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, glib::Enum, PartialOrd, Ord)]
#[enum_type(name = "EuphonicaStickersSupportLevel")]
pub enum StickersSupportLevel {
    #[default]
    Disabled,  // Sticker DB has not been set up
    SongsOnly, // MPD <0.23.15 only supports attaching stickers directly to songs
    All // MPD 0.24+ also supports attaching stickers to tags
}

mod imp {
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecEnum};

    use super::*;
    use once_cell::sync::Lazy;

    #[derive(Debug, Default)]
    pub struct ClientState {
        pub connection_state: Cell<ConnectionState>,
        // Used to indicate that the background client is busy.
        pub busy: Cell<bool>,
        pub supports_playlists: Cell<bool>,
        pub stickers_support_level: Cell<StickersSupportLevel>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ClientState {
        const NAME: &'static str = "EuphonicaClientState";
        type Type = super::ClientState;

        fn new() -> Self {
            Self {
                connection_state: Cell::default(),
                busy: Cell::new(false),
                stickers_support_level: Cell::default(),
                supports_playlists: Cell::new(true),
            }
        }
    }

    impl ObjectImpl for ClientState {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecBoolean::builder("busy").read_only().build(),
                    ParamSpecEnum::builder::<StickersSupportLevel>("stickers-support-level")
                        .read_only()
                        .build(),
                    ParamSpecBoolean::builder("supports-playlists")
                        .read_only()
                        .build(),
                    ParamSpecEnum::builder::<ConnectionState>("connection-state")
                        .read_only()
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "connection-state" => obj.get_connection_state().to_value(),
                "busy" => obj.is_busy().to_value(),
                "stickers-support-level" => obj.get_stickers_support_level().to_value(),
                "supports-playlists" => obj.supports_playlists().to_value(),
                _ => unimplemented!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("idle")
                        .param_types([
                            BoxedAnyObject::static_type(), // mpd::Subsystem::to_str
                        ])
                        .build(),
                    Signal::builder("album-art-downloaded")
                        .param_types([
                            String::static_type(),         // folder URI
                            BoxedAnyObject::static_type(), // hires
                            BoxedAnyObject::static_type(), // thumbnail
                        ])
                        .build(),
                    // Enough information about this album has been downloaded to display it
                    // as a thumbnail in the album view
                    Signal::builder("album-basic-info-downloaded")
                        .param_types([Album::static_type()])
                        .build(),
                    // A chunk of an album's songs have been retrieved. Emit this
                    // to make AlbumContentView append this chunk.
                    Signal::builder("album-songs-downloaded")
                        .param_types([
                            String::static_type(),
                            BoxedAnyObject::static_type(), // Vec<Song>
                        ])
                        .build(),
                    // ArtistInfo downloaded. Should probably queue metadata retrieval.
                    Signal::builder("artist-basic-info-downloaded")
                        .param_types([Artist::static_type()])
                        .build(),
                    // A chunk of an artist's songs have been retrieved. Emit this
                    // to make ArtistContentView append this chunk.
                    Signal::builder("artist-songs-downloaded")
                        .param_types([
                            String::static_type(),
                            BoxedAnyObject::static_type(), // Vec<Song>
                        ])
                        .build(),
                    Signal::builder("artist-album-basic-info-downloaded")
                        .param_types([String::static_type(), Album::static_type()])
                        .build(),
                    Signal::builder("folder-contents-downloaded")
                        .param_types([
                            str::static_type(),            // corresponding path
                            BoxedAnyObject::static_type(), // Vec<INode>
                        ])
                        .build(),
                    // A chunk of a playlist's songs have been retrieved. Emit this
                    // to make PlaylistContentView append this chunk.
                    Signal::builder("playlist-songs-downloaded")
                        .param_types([
                            String::static_type(),
                            BoxedAnyObject::static_type(), // Vec<Song>
                        ])
                        .build(),
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
    pub fn get_connection_state(&self) -> ConnectionState {
        self.imp().connection_state.get()
    }

    pub fn is_busy(&self) -> bool {
        self.imp().busy.get()
    }

    pub fn set_connection_state(&self, new_state: ConnectionState) {
        let old_state = self.imp().connection_state.replace(new_state);
        if old_state != new_state {
            self.notify("connection-state");
        }
    }

    pub fn set_busy(&self, new_busy: bool) {
        let old_busy = self.imp().busy.replace(new_busy);
        if old_busy != new_busy {
            self.notify("busy");
        }
    }

    // Convenience emit wrappers
    pub fn emit_result<T: ToValue>(&self, signal_name: &str, val: T) {
        self.emit_by_name::<()>(signal_name, &[&val])
    }

    pub fn emit_boxed_result<T: 'static>(&self, signal_name: &str, to_box: T) {
        // T must be owned or static
        self.emit_by_name::<()>(signal_name, &[&BoxedAnyObject::new(to_box)]);
    }

    pub fn get_stickers_support_level(&self) -> StickersSupportLevel {
        self.imp().stickers_support_level.get()
    }

    pub fn set_stickers_support_level(&self, new: StickersSupportLevel) {
        let old = self.imp().stickers_support_level.replace(new);
        if old != new {
            self.notify("stickers-support-level");
        }
    }

    pub fn supports_playlists(&self) -> bool {
        self.imp().supports_playlists.get()
    }

    pub fn set_supports_playlists(&self, state: bool) {
        let old = self.imp().supports_playlists.replace(state);
        if old != state {
            self.notify("supports-playlists");
        }
    }
}
