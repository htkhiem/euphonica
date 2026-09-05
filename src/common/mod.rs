pub mod album;
pub mod artist;
pub mod blend_mode;
pub mod content_stack;
pub mod content_view;
pub mod dynamic_playlist;
pub mod genre;
pub mod image_stack;
pub mod inode;
pub mod marquee;
pub mod paintables;
pub mod picture_stack;
pub mod cover_fan;
pub mod rating;
pub mod row_add_buttons;
pub mod row_edit_buttons;
pub mod song;
pub mod song_row;
pub mod sticker;
pub mod tags;
pub mod theme_selector;
pub mod fading_scrolled_window;

pub use album::{Album, AlbumInfo};
pub use artist::{Artist, ArtistInfo, artists_to_string, parse_mb_artist_tag};
pub use genre::split_genre_tag;
pub use content_stack::ContentStack;
pub use content_view::ContentView;
pub use dynamic_playlist::DynamicPlaylist;
use gtk::glib;
pub use image_stack::ImageStack;
pub use inode::{INode, INodeType};
pub use marquee::Marquee;
pub use picture_stack::PictureStack;
pub use cover_fan::CoverFan;
pub use rating::Rating;
pub use row_add_buttons::RowAddButtons;
pub use row_edit_buttons::RowEditButtons;
pub use song::{QualityGrade, Song, SongInfo};
pub use song_row::SongRow;
pub use sticker::Stickers;
pub use theme_selector::ThemeSelector;
pub use fading_scrolled_window::FadingScrolledWindow;

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

#[derive(Clone, Copy, Eq, PartialEq, Debug, Default)]
pub enum ImageState {
    #[default]
    Empty,
    Spinner,
    Image,
}

/// Maps output plugin name to icon name
pub fn map_output_plugin_icon(plugin_name: &str) -> &'static str {
    match plugin_name {
        "alsa" => "alsa-symbolic",
        "pulse" => "pulseaudio-symbolic",
        "pipewire" => "pipewire-symbolic",
        _ => "soundcard-symbolic",
    }
}

// For use with GridViews.
pub static TEXTURE_LOAD_DELAY_MS: core::time::Duration = core::time::Duration::from_millis(50);

#[derive(Default, Clone, Copy, Debug, glib::Enum, glib::Variant, Eq, PartialEq)]
#[enum_type(name = "EuphonicaView")]
pub enum View {
    #[default]
    Recents,
    Albums,
    Artists,
    Folders,
    Playlists,
    DynamicPlaylists,
    Queue,
    Last  // special value, not a view
}

impl TryFrom<&str> for View {
    type Error = ();
    /// For mapping from GSettings
    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "recent" => Ok(Self::Recents),
            "albums" => Ok(Self::Albums),
            "artists" => Ok(Self::Artists),
            "folders" => Ok(Self::Folders),
            "playlists" => Ok(Self::Playlists),
            "dyn-playlists" => Ok(Self::DynamicPlaylists),
            "queue" => Ok(Self::Queue),
            "last" => Ok(Self::Last),
            _ => Err(()),
        }
    }
}

impl TryFrom<u32> for View {
    type Error = ();
    /// For mapping from UI selection
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Recents),
            2 => Ok(Self::Albums),
            3 => Ok(Self::Artists),
            4 => Ok(Self::Folders),
            5 => Ok(Self::Playlists),
            6 => Ok(Self::DynamicPlaylists),
            7 => Ok(Self::Queue),
            0 => Ok(Self::Last),
            _ => Err(()),
        }
    }
}

impl View {
    /// For setting into GSettings
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Recents => "recent",
            Self::Albums => "albums",
            Self::Artists => "artists",
            Self::Folders => "folders",
            Self::Playlists => "playlists",
            Self::DynamicPlaylists => "dyn-playlists",
            Self::Queue => "queue",
            Self::Last => "last"
        }
    }

    /// For mapping to UI menu selection
    pub fn as_idx(&self) -> u32 {
        match self {
            Self::Recents => 1,
            Self::Albums => 2,
            Self::Artists => 3,
            Self::Folders => 4,
            Self::Playlists => 5,
            Self::DynamicPlaylists => 6,
            Self::Queue => 7,
            Self::Last => 0
        }
    }
}