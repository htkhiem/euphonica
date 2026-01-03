pub mod image_stack;
pub mod picture_stack;
pub mod album;
pub mod artist;
pub mod blend_mode;
pub mod content_view;
pub mod dynamic_playlist;
pub mod inode;
pub mod marquee;
pub mod paintables;
pub mod rating;
pub mod row_add_buttons;
pub mod row_edit_buttons;
pub mod song;
pub mod song_row;
pub mod sticker;
pub mod theme_selector;

pub use image_stack::ImageStack;
pub use picture_stack::PictureStack;
pub use album::{Album, AlbumInfo};
pub use artist::{Artist, ArtistInfo, artists_to_string, parse_mb_artist_tag};
pub use content_view::ContentView;
pub use dynamic_playlist::DynamicPlaylist;
pub use inode::{INode, INodeType};
pub use marquee::Marquee;
pub use rating::Rating;
pub use row_add_buttons::RowAddButtons;
pub use row_edit_buttons::RowEditButtons;
pub use song::{QualityGrade, Song, SongInfo};
pub use song_row::SongRow;
pub use sticker::Stickers;
pub use theme_selector::ThemeSelector;

#[derive(Clone, Copy, Eq, PartialEq, Debug, Default)]
pub enum ImageState {
    #[default]
    Empty,
    Spinner,
    Image
}
