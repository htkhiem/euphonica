mod recent_view;
mod recent_song_row;

mod album_cell;
mod album_content_view;
mod album_song_row;
mod album_view;

mod artist_cell;
mod artist_content_view;
mod artist_song_row;
mod artist_view;
mod playlist_song_row;

mod folder_view;

mod playlist_content_view;
mod playlist_view;

// Common stuff shared between views
mod add_to_playlist;
mod generic_row;

// The Library controller itself
mod controller;

pub use recent_view::RecentView;

use album_cell::AlbumCell;
pub use album_content_view::AlbumContentView;
use album_song_row::AlbumSongRow;
pub use album_view::AlbumView;

use artist_cell::ArtistCell;
pub use artist_content_view::ArtistContentView;
use artist_song_row::ArtistSongRow;
pub use artist_view::ArtistView;

pub use folder_view::FolderView;

pub use playlist_content_view::PlaylistContentView;
pub use playlist_song_row::PlaylistSongRow;
pub use playlist_view::PlaylistView;

pub use controller::Library;
