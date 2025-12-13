mod stream;
mod background;
mod connection;

pub mod state;
pub mod wrapper;
pub mod password;

use mpd::{lsinfo::LsInfoEntry, Query, Subsystem, error::Error as MpdError};
pub use state::{ClientState, ConnectionState, ClientError};
pub use wrapper::MpdWrapper;

use crate::common::{AlbumInfo, ArtistInfo, DynamicPlaylist, SongInfo, Stickers};

/// Messages to be sent from child thread or asynchronous methods.
pub enum AsyncClientMessage {
    /// Notifies the main thread to initiate a connection & reconnect the background one too.
    /// The host and port are read from GSettings.
    Connect,

    /// Notifies the main thread to disconnect both clients.
    Disconnect,

    /// Reports the current status of the background task queue.
    Status(
        /// The number of tasks currently pending in the background.
        usize,
    ),

    /// Notifies of MPD-side changes picked up while idling.
    Idle(
        /// A list of MPD subsystems that have changed.
        Vec<Subsystem>,
    ),

    /// Returns the queue's contents batch-by-batch.
    QueueSongsDownloaded(
        /// One batch of SongInfos.
        Vec<SongInfo>,
    ),

    /// Returns songs at changed queue positions batch-by-batch.
    QueueChangesReceived(
        /// A list of songs that have changed or been added.
        Vec<SongInfo>,
    ),

    /// Instructs the UI to update its "queuing" state (e.g., show/hide spinners, enable/disable queue buttons, etc).
    Queuing(
        /// `true` if a queuing operation is in progress, `false` otherwise.
        bool,
    ),

    /// Returns a new AlbumInfo to be shown in a grid view (Album View or Artist Discography subview).
    AlbumBasicInfoDownloaded(
        /// Basic metadata only.
        AlbumInfo,
    ),

    /// Returns a new AlbumInfo to be shown in Recent View.
    RecentAlbumDownloaded(
        /// Basic metadata only.
        AlbumInfo,
    ),

    /// Returns a batch of songs belonging to a specific album.
    AlbumSongInfoDownloaded(
        /// Album tag.
        String,
        /// One batch of songs from this album.
        Vec<SongInfo>,
    ),

    /// Returns a new ArtistInfo to be shown in Artist View or artist tag buttons.
    ArtistBasicInfoDownloaded(
        /// Basic metadata only.
        ArtistInfo,
    ),

    /// Returns metadata for a recently played artist.
    RecentArtistDownloaded(
        /// Basic metadata only.
        ArtistInfo,
    ),

    /// Returns a batch of songs belonging to a specific artist.
    ArtistSongInfoDownloaded(
        /// Artist name (the one originally used to query).
        String,
        /// One batch of songs by this artist.
        Vec<SongInfo>,
    ),

    /// Provides an album associated with a specific artist. Used for the Discography subview.
    ArtistAlbumBasicInfoDownloaded(
        /// Artist name (the one originally used to query).
        String,
        /// Album metadata.
        AlbumInfo,
    ),

    /// Provides the contents (tracks and subfolders) of a directory.
    FolderContentsDownloaded(
        /// Folder URI.
        String,
        /// A list of inodes (files/directories/M3U playlists) found in the folder.
        Vec<LsInfoEntry>,
    ),

    /// Returns the content of a saved (MPD-side) playlist.
    PlaylistSongInfoDownloaded(
        /// The name of the playlist.
        String,
        /// Content as SongInfos.
        Vec<SongInfo>,
    ),

    /// Returns resolved songs for a dynamic playlist.
    DynamicPlaylistSongInfoDownloaded(
        /// The name of the dynamic playlist.
        String,
        /// Resolved songs.
        Vec<SongInfo>,
    ),

    /// Provides a list of recently played songs. For use by Recent View.
    RecentSongInfoDownloaded(
        /// The list of recent songs.
        Vec<SongInfo>,
    ),

    /// Notifies that the MPD database has finished updating.
    DBUpdated,

    /// Reports an error that occurred in a background task.
    BackgroundError(
        /// The underlying error from rust-mpd.
        MpdError,
        /// An optional, Euphonica-specific error hint (e.g., `ClientError`)
        /// to raise in case we need to notify the user. Some errors, like
        /// connection problems, should be handled transparently instead.
        Option<ClientError>,
    ),
}

/// Work requests for sending to the child thread.
/// Completed results will be reported back via AsyncClientMessage.
pub enum BackgroundTask {
    /// Triggers an MPD database update.
    Update,

    /// Queues a list of song URIs for playback.
    QueueUris(
        /// A list of URIs to add to the queue.
        Vec<String>,
        /// Whether to recursively scan directories (if any URIs are directories).
        bool,
        /// Optional queue pos to start playing from.
        Option<u32>,
        /// Optional queue pos to insert at. The first song to be inserted will be at this pos.
        Option<u32>,
    ),

    /// Finds songs matching a specific query and adds them to the queue.
    QueueQuery(
        /// The search query to execute.
        Query<'static>,
        /// Optional queue pos to start playing from.
        Option<u32>,
    ),

    /// Queues all songs from a saved playlist (MPD-side).
    QueuePlaylist(
        /// The name of the playlist to queue.
        String,
        /// Optional queue pos to start playing from.
        Option<u32>,
    ),

    /// Downloads the cover art found in an album's folder (e.g., `cover.jpg`).
    DownloadFolderCover(AlbumInfo),

    /// Extracts and saves embedded cover art from a song's metadata.
    DownloadEmbeddedCover(SongInfo),

    /// Requests a complete refresh of the current playback queue.
    FetchQueue,

    /// Fetches only the changes to the queue since a known version.
    FetchQueueChanges(
        /// The queue version the client currently has.
        u32,
        /// The expected length of the queue after changes (known beforehand from the originating status API call)
        u32,
    ),

    /// Gradually fetches all tracks and subfolders within a specific directory path.
    FetchFolderContents(
        /// Folder URI (path starting from library root).
        String,
    ),

    /// Gradually fetches all albums library (basic information only, for the grid view).
    FetchAlbums,

    /// Fetches a list of recently played albums.
    FetchRecentAlbums,

    /// Fetches all songs associated with a specific album tag.
    FetchAlbumSongs(
        /// Album tag
        String,
    ),

    /// Gradually fetches all artists from the library.
    FetchArtists(
        /// If `true`, fetches based on the "AlbumArtist" tag.
        /// If `false`, fetches based on the "Artist" tag.
        bool,
    ),

    /// Fetches a list of artists whose songs were recently played.
    FetchRecentArtists,

    /// Fetches all songs by a specific artist.
    FetchArtistSongs(
        /// Artist name (will perform a substring search in artist tags too).
        String,
    ),

    /// Fetches all albums by a specific artist.
    FetchArtistAlbums(
        /// Artist name (will perform a substring search in albumartist tags too).
        String,
    ),

    /// Fetches all songs within a specific saved playlist.
    FetchPlaylistSongs(
        /// MPD playlist name.
        String,
    ),

    /// Fetches the last 'n' recently played songs.
    FetchRecentSongs(
        /// n
        u32,
    ),

    /// Fetches songs for a dynamic playlist based on its rules,
    /// optionally updating the cache.
    FetchDynamicPlaylistSongs(
        /// The DP object.
        DynamicPlaylist,
        /// If `true`, cache the results; if `false`, fetch fresh results.
        bool,
    ),

    /// Fetches the last cached state (songs) of a Dynamic Playlist (DP).
    FetchCachedDynamicPlaylistSongs(
        /// DP name.
        String,
    ),

    /// Queues songs from a cached DP.
    QueueDynamicPlaylist(
        /// DP name.
        String,
        /// Play from the start of the queue after queuing.
        /// Assume that the queue was cleared prior to calling this.
        bool,
    ),
}

#[derive(Debug, Clone, Copy)]
pub enum StickerSetMode {
    Inc,
    Set,
    Dec
}
