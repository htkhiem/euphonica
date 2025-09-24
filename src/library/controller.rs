use crate::{
    cache::{sqlite, Cache},
    client::{BackgroundTask, ClientState, MpdWrapper},
    common::{Album, Artist, INode, Song, Stickers}, 
    utils::settings_manager,
    player::Player,
};
use glib::{closure_local, subclass::Signal};
use gtk::{gio, glib, prelude::*};
use std::{borrow::Cow, cell::OnceCell, rc::Rc, sync::OnceLock, vec::Vec};

use adw::subclass::prelude::*;

use mpd::{error::Error as MpdError, search::Operation, EditAction, Query, SaveMode, Term};

mod imp {
    use std::cell::{Cell, RefCell};

    use glib::{ParamSpec, ParamSpecString, ParamSpecUInt};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Debug)]
    pub struct Library {
        pub client: OnceCell<Rc<MpdWrapper>>,
        pub recent_songs: gio::ListStore,
        // Album/Artist retrieval routine:
        // 1. Library places a background task to fetch albums.
        // 3. Background client gets list of unique album tags
        // 4. For each album tag:
        // 4.1. Get the first song with that tag
        // 4.2. Infer folder_uri, sound quality, albumartist, etc. & pack into AlbumInfo class.
        // 4.3. Send AlbumInfo class to main thread via AsyncClientMessage.
        // 4.4. Wrapper tells Library controller to create an Album GObject with that AlbumInfo &
        // append to the list store.
        pub playlists: gio::ListStore,
        pub playlists_initialized: Cell<bool>,
        pub albums: gio::ListStore,
        pub albums_initialized: Cell<bool>,
        pub recent_albums: gio::ListStore,
        pub artists: gio::ListStore,
        pub artists_initialized: Cell<bool>,
        pub recent_artists: gio::ListStore,

        // Folder view
        // Files and folders
        pub folder_history: RefCell<Vec<String>>,
        pub folder_curr_idx: Cell<u32>, // 0 means at root.
        pub folder_inodes: gio::ListStore,
        pub folder_inodes_initialized: Cell<bool>,

        pub cache: OnceCell<Rc<Cache>>,
        pub player: OnceCell<Player>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Library {
        const NAME: &'static str = "EuphonicaLibrary";
        type Type = super::Library;

        fn new() -> Self {
            Self {
                recent_songs: gio::ListStore::new::<Song>(),
                playlists: gio::ListStore::new::<INode>(),
                playlists_initialized: Cell::new(false),
                albums: gio::ListStore::new::<Album>(),
                albums_initialized: Cell::new(false),
                recent_albums: gio::ListStore::new::<Album>(),
                artists: gio::ListStore::new::<Artist>(),
                artists_initialized: Cell::new(false),
                recent_artists: gio::ListStore::new::<Artist>(),
                client: OnceCell::new(),
                cache: OnceCell::new(),
                player: OnceCell::new(),

                folder_history: RefCell::new(Vec::new()),
                folder_curr_idx: Cell::new(0),
                folder_inodes: gio::ListStore::new::<INode>(),
                folder_inodes_initialized: Cell::new(false),
            }
        }
    }

    impl ObjectImpl for Library {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecUInt::builder("folder-curr-idx")
                        .read_only()
                        .build(),
                    ParamSpecUInt::builder("folder-his-len")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("folder-path")
                        .read_only()
                        .build()
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "folder-curr-idx" => self.folder_curr_idx.get().to_value(),
                "folder-his-len" => (self.folder_history.borrow().len() as u32).to_value(),
                "folder-path" => obj.folder_path().to_value(),
                _ => {unimplemented!()}
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![Signal::builder("album-clicked")
                    .param_types([Album::static_type(), gio::ListStore::static_type()])
                    .build()]
            })
        }
    }
}

glib::wrapper! {
    pub struct Library(ObjectSubclass<imp::Library>);
}

impl Default for Library {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Library {
    pub fn setup(&self, client: Rc<MpdWrapper>, cache: Rc<Cache>, player: Player) {
        let client_state = client.get_client_state();
        let _ = self.imp().cache.set(cache);
        let _ = self.imp().client.set(client);
        let _ = self.imp().player.set(player);

        client_state.connect_closure(
            "album-basic-info-downloaded",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |_: ClientState, album: Album| {
                    this.imp().albums.append(&album);
                }
            ),
        );

        client_state.connect_closure(
            "recent-album-downloaded",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |_: ClientState, album: Album| {
                    this.imp().recent_albums.append(&album);
                }
            ),
        );

        client_state.connect_closure(
            "artist-basic-info-downloaded",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |_: ClientState, artist: Artist| {
                    this.imp().artists.append(&artist);
                }
            ),
        );

        client_state.connect_closure(
            "recent-artist-downloaded",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |_: ClientState, artist: Artist| {
                    this.imp().recent_artists.append(&artist);
                }
            ),
        );

        client_state.connect_closure(
            "folder-contents-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, uri: String, entries: glib::BoxedAnyObject| {
                    // If original URI matches current URI, refresh current view
                    if uri == this.folder_path() {
                        this.imp().folder_inodes.remove_all();
                        this.imp()
                            .folder_inodes
                            .extend_from_slice(entries.borrow::<Vec<INode>>().as_ref());
                    }
                }
            ),
        );

        client_state.connect_closure(
            "recent-songs-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, entries: glib::BoxedAnyObject| {
                    this.imp().recent_songs.remove_all();
                    this.imp()
                        .recent_songs
                        .extend_from_slice(entries.borrow::<Vec<Song>>().as_ref());
                }
            ),
        );
    }

    pub fn clear(&self) {
        self.imp().recent_songs.remove_all();
        self.imp().albums.remove_all();
        self.imp().albums_initialized.set(false);
        self.imp().recent_albums.remove_all();
        self.imp().artists.remove_all();
        self.imp().artists_initialized.set(false);
        self.imp().recent_artists.remove_all();
        self.imp().playlists.remove_all();
        self.imp().playlists_initialized.set(false);
        self.imp().folder_inodes.remove_all();
        let _ = self.imp().folder_history.replace(Vec::new());
        let _ = self.imp().folder_curr_idx.replace(0);
        self.notify("folder-path");
        self.notify("folder-his-len");
        self.notify("folder-curr-idx");
        self.imp().folder_inodes_initialized.set(false);
    }

    fn client(&self) -> &Rc<MpdWrapper> {
        self.imp().client.get().unwrap()
    }

    fn cache(&self) -> &Rc<Cache> {
        self.imp().cache.get().unwrap()
    }

    fn player(&self) -> &Player {
        self.imp().player.get().unwrap()
    }

    /// Get all the information available about an album & its contents (won't block;
    /// UI will get notified of result later if one does arrive late).
    pub fn init_album(&self, album: &Album) {
        if let Some(cache) = self.imp().cache.get() {
            cache.fetch_album_meta(album.get_info(), false);
        }
        self.client()
            .queue_background(BackgroundTask::FetchAlbumSongs(
                album.get_title().to_owned(),
            ), true);
    }

    pub fn refetch_album_metadata(&self, album: &Album) {
        if let Some(cache) = self.imp().cache.get() {
            cache.fetch_album_meta(album.get_info(), true);
        }
    }

    /// Queue specific songs
    pub fn queue_songs(&self, songs: &[Song], replace: bool, play: bool) {
        // TODO: support executing this atomically as a command list
        if replace {
            self.client().clear_queue();
        }
        self.client().queue_background(
            BackgroundTask::QueueUris(
                songs
                    .iter()
                    .map(|s| s.get_uri().to_owned())
                    .collect::<Vec<String>>(),
                false,
                if replace && play {
                    Some(0)
                } else {
                    None
                },
                None
            ),
            true
        );
    }

    pub fn insert_songs_next(&self, songs: &[Song]) {
        let pos = if let Some(current_pos) = self.player().queue_pos() {
            // Insert after the position of the current song
            current_pos + 1
        } else {
            // If no current song, insert at the start of the queue
            0
        };
        self.client().queue_background(
            BackgroundTask::QueueUris(
                songs
                    .iter()
                    .map(|s| s.get_uri().to_owned())
                    .collect::<Vec<String>>(),
                false,
                None,
                Some(pos),
            ),
            true
        );
    }

    /// Queue all songs in a given album by track order.
    pub fn queue_album(&self, album: Album, replace: bool, play: bool, play_from: Option<u32>) {
        if replace {
            self.client().clear_queue();
        }
        let mut query = Query::new();
        query.and(
            Term::Tag(Cow::Borrowed("album")),
            album.get_title().to_owned(),
        );
        if let Some(artist) = album.get_artist_tag() {
            query.and(Term::Tag(Cow::Borrowed("albumartist")), artist.to_owned());
        }
        if let Some(mbid) = album.get_mbid() {
            query.and(Term::Tag(Cow::Borrowed("musicbrainz_albumid")), mbid.to_owned());
        }
        self.client().queue_background(
            BackgroundTask::QueueQuery(
                query,
                if replace && play {
                    play_from
                } else {
                    None
                }
            ),
            true
        );
    }

    pub fn rate_album(&self, album: &Album, score: Option<i8>) {
        if let Some(score) = score {
            self.client().set_sticker("album", album.get_title(), Stickers::RATING_KEY, &score.to_string());
        }
        else {
            self.client().delete_sticker("album", album.get_title(), Stickers::RATING_KEY);
        }
    }

    /// Queue all songs of an artist. TODO: allow specifying order.
    pub fn queue_artist(&self, artist: Artist, use_albumartist: bool, replace: bool, play: bool) {
        if replace {
            self.client().clear_queue();
        }
        let mut query = Query::new();
        query.and_with_op(
            Term::Tag(Cow::Borrowed(if use_albumartist {
                "albumartist"
            } else {
                "artist"
            })),
            Operation::Contains,
            artist.get_name().to_owned(),
        );
        self.client().queue_background(
            BackgroundTask::QueueQuery(
                query,
                if replace && play {
                    Some(0)
                } else {
                    None
                }
            ),
            true
        );
    }

    /// Get all the information available about an artist (won't block;
    /// UI will get notified of result later via signals).
    pub fn init_artist(&self, artist: &Artist) {
        if let Some(cache) = self.imp().cache.get() {
            cache.fetch_artist_meta(artist.get_info(), false);
        }
        self.client()
            .get_artist_content(artist.get_name().to_owned());
    }

    pub fn refetch_artist_metadata(&self, artist: &Artist) {
        if let Some(cache) = self.imp().cache.get() {
            cache.fetch_artist_meta(artist.get_info(), true);
        }
    }

    pub fn folder_inodes(&self) -> gio::ListStore {
        self.imp().folder_inodes.clone()
    }

    pub fn folder_curr_idx(&self) -> u32 {
        self.imp().folder_curr_idx.get()
    }

    pub fn folder_history_len(&self) -> u32 {
        self.imp().folder_history.borrow().len() as u32
    }

    pub fn folder_path(&self) -> String {
        let history = self.imp().folder_history.borrow();
        let curr_idx = self.imp().folder_curr_idx.get();
        if history.len() > 0 && curr_idx > 0 {
            history[..curr_idx as usize].join("/")
        } else {
            "".to_string()
        }
    }

    pub fn folder_backward(&self) {
        let curr_idx = self.imp().folder_curr_idx.get();
        if curr_idx > 0 {
            self.imp().folder_curr_idx.set(curr_idx - 1);
            self.notify("folder-curr-idx");
            self.get_folder_contents(&self.folder_path());
            self.notify("folder-path");
        }
    }

    pub fn folder_forward(&self) {
        let curr_idx = self.imp().folder_curr_idx.get();
        if curr_idx < self.imp().folder_history.borrow().len() as u32 {
            self.imp().folder_curr_idx.set(curr_idx + 1);
            self.notify("folder-curr-idx");
            self.get_folder_contents(&self.folder_path());
            self.notify("folder-path");
        }
    }

    pub fn navigate_to(&self, name: &str) {
        let curr_idx = self.imp().folder_curr_idx.get();
        {
            // Limit scope of mut borrow
            let mut history = self.imp().folder_history.borrow_mut();
            let hist_len = history.len();
            if curr_idx < hist_len as u32 {
                history.truncate(curr_idx as usize);
            }
            history.push(name.to_owned());
        }
        self.imp().folder_inodes_initialized.set(false);
        self.folder_forward();
    }
    
    /// Queue a song or folder (when recursive == true) for playback.
    pub fn queue_uri(&self, uri: &str, replace: bool, play: bool, recursive: bool) {
        if replace {
            self.client().clear_queue();
        }
        self.client().queue_background(
            BackgroundTask::QueueUris(
                vec![uri.to_owned()],
                recursive,
                if replace && play {
                    Some(0)
                } else {
                    None
                },
                None
            ),
            true
        );
    }

    /// Queue a playlist for playback.
    pub fn init_playlists(&self) {
        if !self.imp().playlists_initialized.get() {
            self.imp().playlists.remove_all();
            self.imp()
                .playlists
                .extend_from_slice(&self.client().get_playlists());
            self.imp().playlists_initialized.set(true);
        }
    }

    /// Get a reference to the local recent songs store
    pub fn recent_songs(&self) -> gio::ListStore {
        self.imp().recent_songs.clone()
    }

    pub fn clear_recent_songs(&self) {
        self.imp().recent_songs.remove_all();  // Will make Recent View switch to the empty StatusPage
        sqlite::clear_history().expect("Unable to clear history");
    }

    /// Get a reference to the local playlists store
    pub fn playlists(&self) -> gio::ListStore {
        self.imp().playlists.clone()
    }

    /// Get a reference to the local albums store
    pub fn albums(&self) -> gio::ListStore {
        self.imp().albums.clone()
    }

    /// Get a reference to the local recent albums store
    pub fn recent_albums(&self) -> gio::ListStore {
        self.imp().recent_albums.clone()
    }

    /// Get a reference to the local artists store
    pub fn artists(&self) -> gio::ListStore {
        self.imp().artists.clone()
    }

    /// Get a reference to the local recent artists store
    pub fn recent_artists(&self) -> gio::ListStore {
        self.imp().recent_artists.clone()
    }

    /// Retrieve songs in a playlist
    pub fn init_playlist(&self, name: &str) {
        self.client()
            .queue_background(BackgroundTask::FetchPlaylistSongs(name.to_owned()), true);
    }

    /// Queue a playlist for playback.
    pub fn queue_playlist(&self, name: &str, replace: bool, play: bool) {
        if replace {
            self.client().clear_queue();
        }
        self.client().queue_background(
            BackgroundTask::QueuePlaylist(
                name.to_owned(),
                if replace && play {
                    Some(0)
                } else {
                    None
                }
            ),
            true
        );
    }

    pub fn rename_playlist(&self, old_name: &str, new_name: &str) -> Result<(), Option<MpdError>> {
        self.client().rename_playlist(old_name, new_name)
    }

    pub fn delete_playlist(&self, name: &str) -> Result<(), Option<MpdError>> {
        self.client().delete_playlist(name)
    }

    pub fn add_songs_to_playlist(
        &self,
        playlist_name: &str,
        songs: &[Song],
        mode: SaveMode,
    ) -> Result<(), Option<MpdError>> {
        let mut edits: Vec<EditAction> = Vec::with_capacity(songs.len() + 1);
        if mode == SaveMode::Replace {
            edits.push(EditAction::Clear(Cow::Borrowed(playlist_name)));
        }
        songs.iter().for_each(|s| {
            edits.push(EditAction::Add(
                Cow::Borrowed(playlist_name),
                Cow::Borrowed(s.get_uri()),
                None,
            ));
        });
        self.client().edit_playlist(&edits)
    }

    pub fn get_folder_contents(&self, uri: &str) {
        if !self.imp().folder_inodes_initialized.get() {
            self.client()
                .queue_background(BackgroundTask::FetchFolderContents(uri.to_owned()), true);
            self.imp().folder_inodes_initialized.set(true);
        }
    }

    pub fn init_albums(&self) {
        if !self.imp().albums_initialized.get() {
            self.client().queue_background(BackgroundTask::FetchAlbums, false);
            self.imp().albums_initialized.set(true);
        }
    }

    pub fn fetch_recent_albums(&self) {
        // High-priority as this is lightweight (only a few will be fetched)
        // and is triggered by the user navigating to the Recent view
        self.imp().recent_albums.remove_all();
        self.client().queue_background(BackgroundTask::FetchRecentAlbums, true);
    }
 
    pub fn init_artists(&self, use_albumartists: bool) {
        if !self.imp().artists_initialized.get() {
            self.client()
                .queue_background(BackgroundTask::FetchArtists(use_albumartists), false);
            self.imp().artists_initialized.set(true);
        }
    }

    pub fn fetch_recent_artists(&self) {
        // High-priority as this is lightweight (only a few will be fetched)
        // and is triggered by the user navigating to the Recent view
        self.imp().recent_artists.remove_all();
        self.client().queue_background(BackgroundTask::FetchRecentArtists, true);
    }

    pub fn set_cover(&self, folder_uri: &str, path: &str) {
        self.cache().set_cover(folder_uri, path);
    }

    pub fn clear_cover(&self, folder_uri: &str) {
        self.cache().clear_cover(folder_uri);
    }

    pub fn set_artist_avatar(&self, tag: &str, path: &str) {
        self.cache().set_artist_avatar(tag, path);
    }

    pub fn clear_artist_avatar(&self, tag: &str) {
        self.cache().clear_artist_avatar(tag);
    }

    pub fn fetch_recent_songs(&self) {
        self.imp().recent_songs.remove_all();
        let settings = settings_manager().child("library");
        self.client()
            .queue_background(BackgroundTask::FetchRecentSongs(settings.uint("n-recent-songs")), true);
    }
}
