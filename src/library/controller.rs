use crate::{
    cache::{Cache, sqlite},
    client::{
        Error as ClientError, MpdWrapper, Result as ClientResult, StickerSetMode,
        state::StickersSupportLevel,
    },
    common::{Album, Artist, DynamicPlaylist, INode, Song, SongInfo, Stickers, tags},
    library::Tag,
    player::Player,
    utils::settings_manager,
};
use chrono::Local;
use derivative::Derivative;
use glib::subclass::Signal;
use gtk::{gio, glib, prelude::*};
use itertools::Itertools;
use rustc_hash::FxHashMap;
use std::{borrow::Cow, cell::OnceCell, rc::Rc, sync::OnceLock, vec::Vec};
use std::{
    cell::{Cell, RefCell},
    cmp::Ordering,
};

use glib::{ParamSpec, ParamSpecString, ParamSpecUInt};
use once_cell::sync::Lazy;

use adw::subclass::prelude::*;

use mpd::{EditAction, Query, SaveMode, Term, search::Operation as QueryOperation};

mod imp {

    use super::*;

    #[derive(Debug, Derivative)]
    #[derivative(Default)]
    pub struct Library {
        pub client: OnceCell<Rc<MpdWrapper>>,
        pub recent_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub recent_songs: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<INode>()"))]
        pub playlists: gio::ListStore,
        pub playlists_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<INode>()"))]
        pub dyn_playlists: gio::ListStore,
        pub dyn_playlists_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<Album>()"))]
        pub albums: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<Tag>()"))]
        pub album_tags: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<Album>()"))]
        pub recent_albums: gio::ListStore,
        // AlbumArtists are always initialised along with Albums as fetching one requires the other anyway.
        #[derivative(Default(value = "gio::ListStore::new::<Artist>()"))]
        pub albumartists: gio::ListStore,
        pub albums_and_albumartists_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<Artist>()"))]
        pub artists: gio::ListStore,
        pub artists_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<Tag>()"))]
        pub artist_tags: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<Artist>()"))]
        pub recent_artists: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<Tag>()"))]
        pub genres: gio::ListStore,
        pub genres_initialized: Cell<bool>,
        // Folder view
        // Files and folders
        pub folder_history: RefCell<Vec<String>>,
        pub folder_curr_idx: Cell<u32>, // 0 means at root.
        #[derivative(Default(value = "gio::ListStore::new::<INode>()"))]
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
            Self::default()
        }
    }

    impl ObjectImpl for Library {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecUInt::builder("folder-curr-idx")
                        .read_only()
                        .build(),
                    ParamSpecUInt::builder("folder-his-len").read_only().build(),
                    ParamSpecString::builder("folder-path").read_only().build(),
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
                _ => {
                    unimplemented!()
                }
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("album-clicked")
                        .param_types([Album::static_type(), gio::ListStore::static_type()])
                        .build(),
                ]
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
    pub fn setup(&self, client: Rc<MpdWrapper>, player: Player) {
        let _ = self.imp().client.set(client);
        let _ = self.imp().player.set(player);
    }

    pub fn clear(&self) {
        self.imp().recent_songs.remove_all();
        self.imp().genres.remove_all();
        self.imp().genres_initialized.set(false);
        self.imp().albums.remove_all();
        self.imp().album_tags.remove_all();
        self.imp().recent_albums.remove_all();
        self.imp().artists.remove_all();
        self.imp().artist_tags.remove_all();
        self.imp().artists_initialized.set(false);
        self.imp().albumartists.remove_all();
        self.imp().albums_and_albumartists_initialized.set(false);
        self.imp().recent_artists.remove_all();
        self.imp().playlists.remove_all();
        self.imp().playlists_initialized.set(false);
        self.imp().dyn_playlists.remove_all();
        self.imp().dyn_playlists_initialized.set(false);
        self.imp().folder_inodes.remove_all();
        let _ = self.imp().folder_history.replace(Vec::new());
        let _ = self.imp().folder_curr_idx.replace(0);
        self.notify("folder-path");
        self.notify("folder-his-len");
        self.notify("folder-curr-idx");
        self.imp().folder_inodes_initialized.set(false);
        self.imp().recent_initialized.set(false);
    }

    fn client(&self) -> &Rc<MpdWrapper> {
        self.imp().client.get().unwrap()
    }

    fn player(&self) -> &Player {
        self.imp().player.get().unwrap()
    }

    pub async fn get_album_songs<F>(&self, album: &Album, respond: &mut F) -> ClientResult<()>
    where
        F: FnMut(Vec<Song>),
    {
        let mut query = Query::new();
        // Prefer MBID, then album title plus optional albumartist tag.
        if let Some(mbid) = album.get_mbid() {
            query.and(Term::Tag(tags::ALBUM_MBID.into()), mbid.to_owned());
        } else {
            query.and(Term::Tag(tags::ALBUM.into()), album.get_title().to_owned());
            if let Some(albumartist) = album.get_artist_tag() {
                query.and(Term::Tag(tags::ALBUMARTIST.into()), albumartist.to_owned());
            }
        }
        self.client().get_songs_by_query(query, true, respond).await
    }

    /// Queue specific songs
    pub async fn queue_songs(&self, songs: &[Song], replace: bool, play: bool) -> ClientResult<()> {
        // TODO: support executing this atomically as a command list
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        client
            .add_multi(
                songs
                    .iter()
                    .map(|s| s.get_uri().to_owned())
                    .collect::<Vec<String>>(),
                false,
                None,
            )
            .await?;
        if play {
            client.play_at(0, false).await?;
        }
        Ok(())
    }

    pub async fn insert_songs_next(&self, songs: &[Song]) -> ClientResult<()> {
        let pos = if let Some(current_pos) = self.player().queue_pos() {
            // Insert after the position of the current song
            current_pos + 1
        } else {
            // If no current song, insert at the start of the queue
            0
        };
        self.client()
            .add_multi(
                songs
                    .iter()
                    .map(|s| s.get_uri().to_owned())
                    .collect::<Vec<String>>(),
                false,
                Some(pos as usize),
            )
            .await
    }

    /// Queue all songs in a given album by track order.
    pub async fn queue_album(
        &self,
        album: Album,
        replace: bool,
        play: bool,
        play_from: Option<u32>,
    ) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        let mut query = Query::new();
        query.and(Term::Tag(tags::ALBUM.into()), album.get_title().to_owned());
        if let Some(artist) = album.get_artist_tag() {
            query.and(Term::Tag(tags::ALBUMARTIST.into()), artist.to_owned());
        }
        if let Some(mbid) = album.get_mbid() {
            query.and(Term::Tag(tags::ALBUM_MBID.into()), mbid.to_owned());
        }
        client.find_add(query).await?;
        if play {
            client.play_at(play_from.unwrap_or(0), false).await?;
        }
        Ok(())
    }

    pub async fn rate_album(&self, album: &Album, score: Option<i8>) -> ClientResult<()> {
        let filter_expr = album.get_info().get_filter_expression();
        if let Some(score) = score {
            self.client()
                .set_sticker(
                    "filter",
                    filter_expr,
                    Stickers::ALBUM_RATING.into(),
                    score.to_string().into(),
                    StickerSetMode::Set,
                )
                .await
        } else {
            self.client()
                .delete_sticker(
                    "filter",
                    filter_expr,
                    Stickers::ALBUM_RATING.into(),
                )
                .await
        }
    }

    /// Queue all songs of an artist. TODO: allow specifying order.
    pub async fn queue_artist(
        &self,
        artist: &Artist,
        use_albumartist: bool,
        replace: bool,
        play: bool,
    ) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        let mut query = Query::new();
        query.and_with_op(
            Term::Tag(Cow::Borrowed(if use_albumartist {
                tags::ALBUMARTIST
            } else {
                tags::ARTIST
            })),
            QueryOperation::Contains,
            artist.get_name().to_owned(),
        );
        client.find_add(query).await?;
        if play {
            client.play_at(0, false).await?;
        }
        Ok(())
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
        if !history.is_empty() && curr_idx > 0 {
            history[..curr_idx as usize].join("/")
        } else {
            "".to_string()
        }
    }

    pub async fn folder_backward(&self) -> ClientResult<()> {
        let curr_idx = self.imp().folder_curr_idx.get();
        if curr_idx > 0 {
            self.imp().folder_curr_idx.set(curr_idx - 1);
            self.imp().folder_inodes_initialized.set(false);
            self.get_folder_contents().await?;
            self.notify("folder-curr-idx");
            self.notify("folder-path");
        }
        Ok(())
    }

    pub async fn folder_forward(&self) -> ClientResult<()> {
        let curr_idx = self.imp().folder_curr_idx.get();
        if curr_idx < self.imp().folder_history.borrow().len() as u32 {
            self.imp().folder_curr_idx.set(curr_idx + 1);
            self.imp().folder_inodes_initialized.set(false);
            self.get_folder_contents().await?;
            self.notify("folder-curr-idx");
            self.notify("folder-path");
        }
        Ok(())
    }

    pub async fn navigate_to(&self, name: &str) -> ClientResult<()> {
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
        self.folder_forward().await
    }

    /// Queue a song or folder (when recursive == true) for playback.
    pub async fn queue_uri(
        &self,
        uri: String,
        replace: bool,
        play: bool,
        recursive: bool,
    ) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        client.add_multi(vec![uri], recursive, None).await?;
        if play {
            client.play_at(0, false).await?;
        }
        Ok(())
    }

    /// Get all playlists
    pub async fn init_playlists(&self, refresh: bool) -> ClientResult<()> {
        if refresh || !self.imp().playlists_initialized.get() {
            self.imp().playlists_initialized.set(true);
            self.imp().playlists.remove_all();
            self.imp()
                .playlists
                .extend_from_slice(&self.client().get_playlists().await?);
        }
        Ok(())
    }

    /// Get all dynamic playlists
    pub async fn init_dyn_playlists(&self, refresh: bool) -> ClientResult<()> {
        if !self.imp().dyn_playlists_initialized.get() || refresh {
            self.imp().dyn_playlists_initialized.set(true);
            self.imp().dyn_playlists.remove_all();
            let inode_infos = gio::spawn_blocking(sqlite::get_dynamic_playlists)
                .await
                .unwrap()
                .map_err(|_| ClientError::Internal)?;
            self.imp().dyn_playlists.extend_from_slice(
                &inode_infos
                    .into_iter()
                    .map(INode::from)
                    .collect::<Vec<INode>>(),
            );
        }
        Ok(())
    }

    /// Get a reference to the local recent songs store
    pub fn recent_songs(&self) -> gio::ListStore {
        self.imp().recent_songs.clone()
    }

    pub async fn clear_recent_songs(&self) -> ClientResult<()> {
        self.imp().recent_songs.remove_all(); // Will make Recent View switch to the empty StatusPage
        gio::spawn_blocking(sqlite::clear_history)
            .await
            .unwrap()
            .map_err(|_| ClientError::Internal)
    }

    /// Get a reference to the local playlists store
    pub fn playlists(&self) -> gio::ListStore {
        self.imp().playlists.clone()
    }

    /// Get a reference to the local dynamic playlists store
    pub fn dyn_playlists(&self) -> gio::ListStore {
        self.imp().dyn_playlists.clone()
    }

    /// Get a reference to the local albums store
    pub fn albums(&self) -> gio::ListStore {
        self.imp().albums.clone()
    }

    /// Get a reference to the list of distinct genres
    pub fn genres(&self) -> gio::ListStore {
        self.imp().genres.clone()
    }

    /// Get a reference to the list of album tags
    pub fn album_tags(&self) -> gio::ListStore {
        self.imp().album_tags.clone()
    }

    /// Get a reference to the list of artist tags
    pub fn artist_tags(&self) -> gio::ListStore {
        self.imp().artist_tags.clone()
    }

    /// Get a reference to the local recent albums store
    pub fn recent_albums(&self) -> gio::ListStore {
        self.imp().recent_albums.clone()
    }

    /// Get a reference to the local artists store
    pub fn artists(&self) -> gio::ListStore {
        self.imp().artists.clone()
    }

    /// Get a reference to the local album artists store
    pub fn album_artists(&self) -> gio::ListStore {
        self.imp().albumartists.clone()
    }

    /// Get a reference to the local recent artists store
    pub fn recent_artists(&self) -> gio::ListStore {
        self.imp().recent_artists.clone()
    }

    /// Retrieve songs in a playlist
    pub async fn get_playlist_songs<F>(&self, name: String, respond: &mut F) -> ClientResult<()>
    where
        F: FnMut(Vec<Song>),
    {
        self.client().get_playlist_songs(name, respond).await
    }

    /// Queue a playlist for playback.
    pub async fn queue_playlist(
        &self,
        name: String,
        replace: bool,
        play: bool,
    ) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        client.load_playlist(name).await?;
        if play {
            client.play_at(0, false).await?;
        }
        Ok(())
    }

    pub async fn rename_playlist(&self, old_name: String, new_name: String) -> ClientResult<()> {
        self.client().rename_playlist(old_name, new_name).await
    }

    pub async fn delete_playlist(&self, name: String) -> ClientResult<()> {
        self.client().delete_playlist(name).await?;
        self.init_playlists(true).await
    }

    pub async fn add_songs_to_playlist(
        &self,
        playlist_name: String,
        songs: &[Song],
        mode: SaveMode,
    ) -> ClientResult<()> {
        let mut edits: Vec<EditAction<'static>> = Vec::with_capacity(songs.len() + 1);
        if mode == SaveMode::Replace {
            edits.push(EditAction::Clear(playlist_name.to_string().into()));
        }
        songs.iter().for_each(|s| {
            edits.push(EditAction::Add(
                playlist_name.to_string().into(),
                s.get_uri().to_string().into(),
                None,
            ));
        });
        self.client().edit_playlist(edits).await
    }

    /// Retrieve songs in a dynamic playlist
    pub async fn get_dynamic_playlist_songs(
        &self,
        dp: DynamicPlaylist,
        cache: bool,
    ) -> ClientResult<Vec<Song>> {
        self.client().get_dynamic_playlist_songs(dp, cache).await
    }

    /// Retrieve last cached state of a dynamic playlist
    pub async fn get_dynamic_playlist_songs_cached(&self, name: String) -> ClientResult<Vec<Song>> {
        self.client().get_dynamic_playlist_songs_cached(name).await
    }

    /// Get last cached results of a dynamic playlist
    pub async fn queue_cached_dynamic_playlist(
        &self,
        name: String,
        replace: bool,
        play: bool,
    ) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        client.queue_cached_dynamic_playlist(name).await?;
        if play {
            client.play_at(0, false).await?;
        }
        Ok(())
    }

    /// Delete a dynamic playlist by name. Will also remove cover entries.
    pub async fn delete_dynamic_playlist(&self, name: String) -> ClientResult<()> {
        gio::spawn_blocking(move || sqlite::delete_dynamic_playlist(&name))
            .await
            .unwrap()
            .map_err(|_| ClientError::Internal)?;
        self.init_dyn_playlists(true).await
    }

    /// Will return None if there were no songs to save.
    pub async fn save_dynamic_playlist_state(
        &self,
        dp_name: String,
    ) -> ClientResult<Option<String>> {
        let name = dp_name.clone();
        let uris = gio::spawn_blocking(move || sqlite::get_cached_dynamic_playlist_results(&name))
            .await
            .unwrap()
            .map_err(|_| ClientError::Internal)?;

        if !uris.is_empty() {
            let fixed_name = format!("{} {}", dp_name, Local::now().format("%Y-%m-%d %H:%M:%S"));
            self.client()
                .edit_playlist(
                    uris.iter()
                        .map(|uri| {
                            EditAction::Add(
                                Cow::Owned(fixed_name.clone()),
                                Cow::Owned(uri.to_string()),
                                None,
                            )
                        })
                        .collect::<Vec<EditAction<'static>>>(),
                )
                .await?;
            Ok(Some(fixed_name))
        } else {
            Ok(None)
        }
    }

    pub async fn get_folder_contents(&self) -> ClientResult<()> {
        if !self.imp().folder_inodes_initialized.get() {
            self.imp().folder_inodes_initialized.set(true);
            self.imp().folder_inodes.remove_all();
            self.imp()
                .folder_inodes
                .extend_from_slice(&self.client().lsinfo(self.folder_path()).await?);
        }
        Ok(())
    }

    pub async fn init_recent(&self, refresh: bool) -> ClientResult<()> {
        if !self.imp().recent_initialized.get() || refresh {
            self.imp().recent_initialized.set(true);
            let model = self.imp().recent_songs.clone();
            model.remove_all();
            let settings = settings_manager().child("library");
            model.extend_from_slice(
                &self
                    .client()
                    .get_recent_songs(settings.uint("n-recent-songs"))
                    .await?,
            );

            let model = self.imp().recent_albums.clone();
            model.remove_all();
            self.client()
                .get_recent_albums(&mut |album| {
                    model.append(&album);
                })
                .await?;

            let model = self.imp().recent_artists.clone();
            model.remove_all();
            self.client()
                .get_recent_artists(&|artist| {
                    model.append(&artist);
                })
                .await?;
        }
        Ok(())
    }

    pub async fn init_genres(&self) -> ClientResult<()> {
        if !self.imp().genres_initialized.get() {
            self.imp().genres_initialized.set(true);
            let genres = self.imp().genres.clone();
            genres.remove_all();
            self.client()
                .get_distinct_genres(&mut |split_genres: Vec<String>| {
                    for genre in split_genres {
                        let obj = Tag::new(genre, None, None, false, false);
                        genres.append(&obj);
                    }
                })
                .await?;
        }
        Ok(())
    }

    pub async fn refresh_album_tags(&self) -> ClientResult<()> {
        let tags = self.imp().album_tags.clone();
        tags.remove_all();
        tags.extend_from_slice(
            &sqlite::distinct_album_tags()
                .await
                .map_err(|_| ClientError::Internal)?
                .into_iter()
                .map(Tag::from)
                .collect::<Vec<Tag>>(),
        );
        Ok(())
    }

    /// Fetch basic info for all albums to display them in a grid. Will also fetch tags as stored locally
    /// and album rating stickers. 
    /// During the process we'll also produce albumartists as a side effect.
    pub async fn init_albums_and_albumartists(&self) -> ClientResult<()> {
        if !self.imp().albums_and_albumartists_initialized.get() {
            self.imp().albums_and_albumartists_initialized.set(true);
            let album_model = self.imp().albums.clone();
            album_model.remove_all();
            let albumartist_model = self.imp().albumartists.clone();
            albumartist_model.remove_all();
            let (albums, artists) = self
                .client()
                .get_albums_and_albumartists_by_query(Query::new(), true)
                .await?;
            album_model.extend_from_slice(&albums);
            albumartist_model.extend_from_slice(&artists);
        }
        Ok(())
    }

    pub async fn refresh_artist_tags(&self) -> ClientResult<()> {
        let tags = self.imp().artist_tags.clone();
        tags.remove_all();
        tags.extend_from_slice(
            &sqlite::distinct_artist_tags()
                .await
                .map_err(|_| ClientError::Internal)?
                .into_iter()
                .map(Tag::from)
                .collect::<Vec<Tag>>(),
        );
        Ok(())
    }

    /// Initialises both artist and albumartist models.
    /// For albumartists, this function calls init_albums_and_albumartists.
    pub async fn init_artists(&self) -> ClientResult<()> {
        self.init_albums_and_albumartists().await?;
        // Initialises the artists list by itself
        if !self.imp().artists_initialized.get() {
            self.imp().artists_initialized.set(true);
            // init the artists list
            let artist_model = self.imp().artists.clone();
            artist_model.remove_all();
            artist_model.extend_from_slice(
                &self
                    .client()
                    .get_artists()
                    .await?,
            );
        }
        Ok(())
    }

    /// Get songs and albums of an artist.
    /// To facilitate both discography and all-tracks subviews efficiently, we share the same song instances
    /// between them both.
    /// Albums will also be grouped & sorted by descending release year.
    /// Return type: (
    ///   - vector of songs
    ///   - vector of (
    ///     - option(year) (can be None for albums/tracks without clear release dates),
    ///     - (option(album) x songs) in that year.
    ///   )
    /// )
    /// Sort order: release year => album title => track number => URI, nulls always last.
    /// Examples:
    /// Songs with both album and release date tags will be sorted by (year, album) (will only sort by year).
    /// Songs without albums but with years will be stored per year, after album-tagged ones.
    /// Songs with album tags but no years will be put in albums under year = None.
    /// Songs with neither album tags nor years will be put dead last & ordered by URI.
    pub async fn get_artist_content(
        &self,
        artist: &Artist,
        fetch_by_artist_tag: bool,
        fetch_by_albumartist_tag: bool,
    ) -> ClientResult<(
        Vec<Song>,
        Vec<(Option<i32>, Vec<(Option<Album>, Vec<Song>)>)>,
    )> {
        let comp_id = artist.get_info().get_comp_id();
        let mut songs: FxHashMap<String, SongInfo> = FxHashMap::default();

        if fetch_by_artist_tag {
            let mut query_artist_tag = Query::new();
            query_artist_tag.and_with_op(
                Term::Tag(tags::ARTIST.into()),
                QueryOperation::Contains,
                artist.get_name().to_owned(),
            );
            self.client()
                .get_song_infos_by_query(query_artist_tag, true, &mut |batch| {
                    batch
                        .into_iter()
                        .filter(|s| s.artists.iter().any(|a| a.get_comp_id() == comp_id))
                        .for_each(|si| {
                            songs.insert(si.uri.clone(), si);
                        });
                })
                .await?;
        }
        if fetch_by_albumartist_tag {
            let mut query_albumartist_tag = Query::new();
            query_albumartist_tag.and_with_op(
                Term::Tag(tags::ALBUMARTIST.into()),
                QueryOperation::Contains,
                artist.get_name().to_owned(),
            );
            self.client()
                .get_song_infos_by_query(query_albumartist_tag, true, &mut |batch| {
                    batch
                        .into_iter()
                        .filter(|s| {
                            s.album.as_ref().is_some_and(|a| {
                                a.artists.iter().any(|a| a.get_comp_id() == comp_id)
                            })
                        })
                        .for_each(|si| {
                            songs.insert(si.uri.clone(), si);
                        });
                })
                .await?;
        }
        if !fetch_by_artist_tag && !fetch_by_albumartist_tag {
            eprintln!(
                "WARNING: both fetch_by_artist_tag and fetch_by_albumartist_tag are false (this is a no-op)"
            );
        }
        if songs.is_empty() {
            return Ok((Vec::with_capacity(0), Vec::with_capacity(0)));
        }

        let songs: Vec<Song> = songs
            .into_iter()
            .map(|p| p.1)
            .sorted_by(|s1, s2| {
                let cmp_date_available = s1.release_date.is_some().cmp(&s2.release_date.is_some());
                if cmp_date_available != Ordering::Equal {
                    return cmp_date_available.reverse(); // nulls last
                }

                if s1.release_date.is_some() {
                    // Both Some => compare release year first
                    let cmp_date = s1
                        .release_date
                        .unwrap()
                        .year()
                        .cmp(&s2.release_date.unwrap().year());
                    if cmp_date != Ordering::Equal {
                        return cmp_date.reverse(); // descending year
                    }
                }

                // Same release date or both None => check album availability
                let cmp_album_available = s1.album.is_some().cmp(&s2.album.is_some());
                if cmp_album_available != Ordering::Equal {
                    return cmp_album_available.reverse(); // nulls last
                }
                if s1.album.is_some() {
                    // Both has albums
                    let cmp_album_title = s1
                        .album
                        .as_ref()
                        .map(|a| a.title.as_str())
                        .cmp(&s2.album.as_ref().map(|a| a.title.as_ref()));
                    if cmp_album_title != Ordering::Equal {
                        return cmp_album_title; // ascending title sort
                    }

                    // Same album => sort by track number.
                    // Only available if both have the same album tags, else we skip straight to URI.
                    let cmp_track_num_available = s1.track.is_some().cmp(&s2.track.is_some());
                    if cmp_track_num_available != Ordering::Equal {
                        return cmp_track_num_available.reverse(); // nulls last
                    }
                    if s1.track.is_some() {
                        let cmp_track_num = s1.track.unwrap().cmp(&s2.track.unwrap());
                        if cmp_track_num != Ordering::Equal {
                            return cmp_track_num; // don't reverse here (ascending sort as usual)
                        }
                    }
                }
                s1.uri.as_str().cmp(s2.uri.as_str())
            })
            .map(Song::from)
            .collect();

        // At this point we'll have a well-sorted song list to efficiently organise into the return format
        let mut years: Vec<(Option<i32>, Vec<(Option<Album>, Vec<Song>)>)> = Vec::new();
        let mut curr_year = songs[0].get_release_date().map(|d| d.year());
        let mut curr_album_id = songs[0].get_album().map(|a| a.get_comp_id());
        years.push((
            curr_year,
            vec![(
                songs[0].get_album().map(|a| Album::from(a.clone())),
                vec![songs[0].clone()],
            )],
        ));

        for song in songs.iter().skip(1) {
            // If different year, append new entry
            let year = song.get_release_date().map(|d| d.year());
            let album = song.get_album();
            if year != curr_year {
                curr_year = year;
                curr_album_id = album.map(|a| a.get_comp_id());
                years.push((
                    curr_year,
                    vec![(album.map(|a| Album::from(a.clone())), vec![song.clone()])],
                ));
            } else {
                // Same year
                let years_len = years.len();
                let year_vec = &mut years[years_len - 1].1;
                // Check if different album
                if album.map(|a| a.get_comp_id()) != curr_album_id {
                    curr_album_id = album.map(|a| a.get_comp_id());
                    year_vec.push((album.map(|a| Album::from(a.clone())), vec![song.clone()]));
                } else {
                    // Same album too, or equally album-less => just push
                    let year_vec_len = year_vec.len();
                    let year_album_vec = &mut year_vec[year_vec_len - 1].1;
                    year_album_vec.push(song.clone());
                }
            }
        }

        Ok((songs, years))
    }

    /// Whether the currently-connected client supports metadata backup in some form.
    /// Right now we only support MPD. Metadata backup in MPD is done via the stickers DB, which is an optional feature
    /// and non-track stickers are only supported from MPD 0.24 onwards.
    pub fn metadata_backup_available(&self) -> bool {
        self.client().get_client_state().stickers_support_level() >= StickersSupportLevel::All
    }
}
