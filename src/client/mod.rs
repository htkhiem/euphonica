mod connection;
mod stream;

pub mod password;
pub mod state;
pub mod wrapper;

use mpd::{Query, Subsystem, error::Error as MpdError, lsinfo::LsInfoEntry};
pub use state::{ClientState, ConnectionState};
pub use wrapper::MpdWrapper;

use crate::common::{AlbumInfo, ArtistInfo, DynamicPlaylist, SongInfo, Stickers};

pub use connection::Error;
pub use connection::Result;

#[derive(Debug, Clone, Copy)]
pub enum StickerSetMode {
    Inc,
    Set,
    Dec,
}
