mod recent_view;

mod album_cell;
mod album_content_view;
mod album_sort;
mod album_view;
mod artist_tag_button;
mod tag;
mod tag_button;
mod tags_section;

mod artist_cell;
mod artist_content_view;
mod artist_view;
mod discography_album;
mod discography_year;
mod folder_view;

mod song_sort;
mod song_view;

mod playlist_content_view;
mod playlist_row;
mod playlist_view;

mod dynamic_playlist_content_view;
mod dynamic_playlist_editor_view;
mod dynamic_playlist_view;
mod ordering_button;
mod rule_button;

// Common stuff shared between views
mod add_to_playlist;
mod generic_row;
mod tags_filter;

// Recommendation algorithms. Sounds grand but right now they're just SQLite queries :)
pub mod recommendations;

// The Library controller itself
mod controller;

pub use recent_view::RecentView;

use album_cell::AlbumCell;
pub use album_content_view::AlbumContentView;
pub use album_view::AlbumView;
use strum::{EnumMessage, VariantArray};
pub use tag::Tag;
pub use tags_filter::TagsFilter;
pub use tags_section::TagsSection;

use artist_cell::ArtistCell;
pub use artist_content_view::ArtistContentView;
pub use artist_view::ArtistView;

pub use folder_view::FolderView;

pub use song_view::SongView;

pub use dynamic_playlist_content_view::DynamicPlaylistContentView;
pub use dynamic_playlist_editor_view::DynamicPlaylistEditorView;
pub use dynamic_playlist_view::DynamicPlaylistView;

pub use playlist_content_view::PlaylistContentView;
pub use playlist_view::PlaylistView;

pub use controller::Library;


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

pub trait SortCriterion: VariantArray + EnumMessage {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SortTier<T: SortCriterion> {
    pub criterion: T,
    pub direction: SortDirection,
}