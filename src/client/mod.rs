mod stream;
mod background;
pub mod state;
pub mod wrapper;
pub mod password;

use mpd::{lsinfo::LsInfoEntry, Query, Subsystem, error::Error as MpdError};
pub use state::{ClientState, ConnectionState, ClientError};
pub use wrapper::MpdWrapper;

use crate::common::{AlbumInfo, ArtistInfo, SongInfo};

// Messages to be sent from child thread or synchronous methods
enum AsyncClientMessage {
    Connect, // Host and port are always read from gsettings
    Disconnect,
    Status(usize), // Number of pending background tasks
    Idle(Vec<Subsystem>), // Will only be sent from the child thread
    QueueSongsDownloaded(Vec<SongInfo>),
    QueueChangesReceived(Vec<SongInfo>),
    Queuing(bool),  // Set queuing state
    AlbumBasicInfoDownloaded(AlbumInfo), // Return new album to be added to the list model (as SongInfo of a random song in it).
    RecentAlbumDownloaded(AlbumInfo),
    AlbumSongInfoDownloaded(String, Vec<SongInfo>), // Return songs in the album with the given tag (batched)
    ArtistBasicInfoDownloaded(ArtistInfo), // Return new artist to be added to the list model.
    RecentArtistDownloaded(ArtistInfo),
    ArtistSongInfoDownloaded(String, Vec<SongInfo>), // Return songs of an artist (or had their participation)
    ArtistAlbumBasicInfoDownloaded(String, AlbumInfo), // Return albums that had this artist in their AlbumArtist tag.
    FolderContentsDownloaded(String, Vec<LsInfoEntry>),
    PlaylistSongInfoDownloaded(String, Vec<SongInfo>),
    RecentSongInfoDownloaded(Vec<SongInfo>),
    DBUpdated,
    // Generic background error, with an optional Euphonica-specific hint
    BackgroundError(MpdError, Option<ClientError>)
}

// Work requests for sending to the child thread.
// Completed results will be reported back via AsyncClientMessage.
pub enum BackgroundTask {
    Update,
    // Optional recursive, optional position to start playing from,
    // optional position in queue to insert at
    QueueUris(Vec<String>, bool, Option<u32>, Option<u32>),
    QueueQuery(Query<'static>, Option<u32>),  // Optional position to start playing from
    QueuePlaylist(String, Option<u32>),
    DownloadFolderCover(AlbumInfo),
    DownloadEmbeddedCover(SongInfo),
    FetchQueue,  // Full fetch
    FetchQueueChanges(u32, u32),  // Current version and expected length of updated queue
    FetchFolderContents(String), // Gradually get all inodes in folder at path
    FetchAlbums, // Gradually get all albums
    FetchRecentAlbums,
    FetchAlbumSongs(String),  // Get songs of album with given tag
    FetchArtists(bool), // Gradually get all artists. If bool flag is true, will parse AlbumArtist tag
    FetchRecentArtists,
    FetchArtistSongs(String), // Get all songs of an artist with given name
    FetchArtistAlbums(String), // Get all albums of an artist with given name
    FetchPlaylistSongs(String), // Get songs of playlist with given name
    FetchRecentSongs(u32), // Get last n songs
}
