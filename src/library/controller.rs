use crate::{
    cache::{Cache, sqlite},
    client::{ClientState, MpdWrapper, StickerSetMode, Result as ClientResult, Error as ClientError},
    common::{Album, Artist, DynamicPlaylist, INode, Song, Stickers},
    player::Player,
    utils::settings_manager,
};
use chrono::Local;
use derivative::Derivative;
use glib::{clone, closure_local, subclass::Signal};
use gtk::{gio, glib, prelude::*};
use std::{borrow::Cow, cell::OnceCell, rc::Rc, sync::OnceLock, vec::Vec};
use std::cell::{Cell, RefCell};

use glib::{ParamSpec, ParamSpecString, ParamSpecUInt};
use once_cell::sync::Lazy;

use adw::subclass::prelude::*;

use mpd::{EditAction, Query, SaveMode, Term, error::Error as MpdError, search::Operation as QueryOperation};

mod imp {


    use super::*;

    #[derive(Debug, Derivative)]
    #[derivative(Default)]
    pub struct Library {
        pub client: OnceCell<Rc<MpdWrapper>>,
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
        pub albums_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<Album>()"))]
        pub recent_albums: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<Artist>()"))]
        pub artists: gio::ListStore,
        pub artists_initialized: Cell<bool>,
        #[derivative(Default(value = "gio::ListStore::new::<Artist>()"))]
        pub recent_artists: gio::ListStore,

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
        self.imp().albums.remove_all();
        self.imp().albums_initialized.set(false);
        self.imp().recent_albums.remove_all();
        self.imp().artists.remove_all();
        self.imp().artists_initialized.set(false);
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
    }

    fn client(&self) -> &Rc<MpdWrapper> {
        self.imp().client.get().unwrap()
    }

    fn player(&self) -> &Player {
        self.imp().player.get().unwrap()
    }

    pub async fn get_album_songs<F>(&self, title: String, respond: &mut F) -> ClientResult<()>
    where F: FnMut(Vec<Song>) {
        let mut query = Query::new();
        query.and(Term::Tag(Cow::Borrowed("album")), title);
        self.client().get_songs_by_query(query, respond).await
    }

    /// Queue specific songs
    pub async fn queue_songs(&self, songs: &[Song], replace: bool, play: bool) -> ClientResult<()> {
        // TODO: support executing this atomically as a command list
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        client.add_multi(
            songs
                .iter()
                .map(|s| s.get_uri().to_owned())
                .collect::<Vec<String>>(),
            false,
            None
        ).await?;
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
        self.client().add_multi(
            songs
                .iter()
                .map(|s| s.get_uri().to_owned())
                .collect::<Vec<String>>(),
            false,
            Some(pos as usize)
        ).await
    }

    /// Queue all songs in a given album by track order.
    pub async fn queue_album(
        &self, album: Album, replace: bool, play: bool, play_from: Option<u32>
    ) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
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
            query.and(
                Term::Tag(Cow::Borrowed("musicbrainz_albumid")),
                mbid.to_owned(),
            );
        }
        client.find_add(query).await?;
        if play {
            client.play_at(play_from.unwrap_or(0), false).await?;
        }
        Ok(())
    }

    pub async fn rate_album(&self, album: &Album, score: Option<i8>) -> ClientResult<()> {
        if let Some(score) = score {
            self.client().set_sticker(
                "album",
                album.get_title().to_owned(),
                Stickers::RATING_KEY.into(),
                score.to_string().into(),
                StickerSetMode::Set,
            ).await
        } else {
            self.client()
                .delete_sticker("album", album.get_title().to_owned(), Stickers::RATING_KEY.into()).await
        }
    }

    /// Queue all songs of an artist. TODO: allow specifying order.
    pub async fn queue_artist(&self, artist: &Artist, use_albumartist: bool, replace: bool, play: bool) -> ClientResult<()> {
        let client = self.client();
        if replace {
            client.clear_queue().await?;
        }
        let mut query = Query::new();
        query.and_with_op(
            Term::Tag(Cow::Borrowed(if use_albumartist {
                "albumartist"
            } else {
                "artist"
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
    pub async fn queue_uri(&self, uri: String, replace: bool, play: bool, recursive: bool) -> ClientResult<()> {
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
            self.imp().playlists.remove_all();
            self.imp()
                .playlists
                .extend_from_slice(&self.client().get_playlists().await?);
            self.imp().playlists_initialized.set(true);
        }
        Ok(())
    }

    /// Get all dynamic playlists
    pub async fn init_dyn_playlists(&self, refresh: bool) -> ClientResult<()> {
        if !self.imp().dyn_playlists_initialized.get() || refresh {
            self.imp().dyn_playlists.remove_all();
            let inode_infos = gio::spawn_blocking(|| sqlite::get_dynamic_playlists())
                .await
                .unwrap()
                .map_err(|_| ClientError::Internal)?;
            println!("Received {} dynamic playlists", inode_infos.len());
            self.imp().dyn_playlists.extend_from_slice(
                &inode_infos
                    .into_iter()
                    .map(INode::from)
                    .collect::<Vec<INode>>(),
            );
            self.imp().dyn_playlists_initialized.set(true);
        }
        Ok(())
    }

    /// Get a reference to the local recent songs store
    pub fn recent_songs(&self) -> gio::ListStore {
        self.imp().recent_songs.clone()
    }

    pub async fn clear_recent_songs(&self) -> ClientResult<()> {
        self.imp().recent_songs.remove_all(); // Will make Recent View switch to the empty StatusPage
        gio::spawn_blocking(|| sqlite::clear_history()).await.unwrap().map_err(|_| ClientError::Internal)
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
    pub async fn get_playlist_songs<F>(&self, name: String, respond: F) -> ClientResult<()>
    where F: FnMut(Vec<Song>) {
        self.client().get_playlist_songs(name, respond).await
    }

    /// Queue a playlist for playback.
    pub async fn queue_playlist(&self, name: String, replace: bool, play: bool) -> ClientResult<()> {
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
    pub async fn get_dynamic_playlist_songs(&self, dp: DynamicPlaylist, cache: bool) -> ClientResult<Vec<Song>> {
        self.client().get_dynamic_playlist_songs(dp, cache).await
    }

    /// Retrieve last cached state of a dynamic playlist
    pub async fn get_dynamic_playlist_songs_cached(&self, name: String) -> ClientResult<Vec<Song>> {
        self.client().get_dynamic_playlist_songs_cached(name).await
    }

    /// Get last cached results of a dynamic playlist
    pub async fn queue_cached_dynamic_playlist(&self, name: String, replace: bool, play: bool) -> ClientResult<()> {
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
    pub async fn save_dynamic_playlist_state(&self, dp_name: String) -> ClientResult<Option<String>> {
        let name = dp_name.clone();
        let uris = gio::spawn_blocking(move || sqlite::get_cached_dynamic_playlist_results(&name))
            .await
            .unwrap()
            .map_err(|_| ClientError::Internal)?;

        if !uris.is_empty() {
            let fixed_name =
                format!("{} {}", dp_name, Local::now().format("%Y-%m-%d %H:%M:%S"));
            self.client().edit_playlist(
                uris.iter()
                    .map(|uri| {
                        EditAction::Add(
                            Cow::Owned(fixed_name.clone()),
                            Cow::Owned(uri.to_string()),
                            None,
                        )
                    })
                    .collect::<Vec<EditAction<'static>>>(),
            ).await?;
            Ok(Some(fixed_name))
        } else {
            Ok(None)
        }
    }

    pub async fn get_folder_contents(&self) -> ClientResult<()> {
        if !self.imp().folder_inodes_initialized.get() {
            self.imp().folder_inodes.remove_all();
            self.imp().folder_inodes.extend_from_slice(
                &self.client().lsinfo(self.folder_path()).await?
            );
            self.imp().folder_inodes_initialized.set(true);
        }
        Ok(())
    }

    pub async fn init_albums(&self) -> ClientResult<()> {
        if !self.imp().albums_initialized.get() {
            let model = self.imp().albums.clone();
            model.remove_all();

            self.client().get_albums_by_query(Query::new(), &mut |album| {
                model.append(&album);
            }).await?;
            self.imp().albums_initialized.set(true);
        }
        Ok(())
    }

    pub async fn get_recent_albums(&self) -> ClientResult<()> {
        let model = self.imp().recent_albums.clone();
        model.remove_all();

        self.client().get_recent_albums(&mut |album| {model.append(&album);}).await
    }

    pub async fn init_artists(&self, use_album_artist: bool) -> ClientResult<()> {
        if !self.imp().artists_initialized.get() {
            let model = self.imp().artists.clone();
            model.remove_all();

            self.client().get_artists(use_album_artist, &mut |artist| {
                model.append(&artist);
            }).await?;
            self.imp().artists_initialized.set(true);
        }
        Ok(())
    }

    pub async fn get_albums_of_artist<F>(&self, artist: &Artist, mut respond: F) -> ClientResult<()>
    where F: FnMut(Album) {
        let mut query = Query::new();
        query.and_with_op(
            Term::Tag("artist".into()),
            QueryOperation::Contains,
            artist.get_name().to_owned(),
        );
        self.client().get_albums_by_query(query, &mut respond).await
    }

    pub async fn get_songs_of_artist<F>(&self, artist: &Artist, mut respond: F) -> ClientResult<()>
    where F: FnMut(Vec<Song>) {
        let mut query = Query::new();
        query.and_with_op(
            Term::Tag("artist".into()),
            QueryOperation::Contains,
            artist.get_name().to_owned(),
        );
        self.client().get_songs_by_query(query, &mut respond).await
    }

    pub async fn get_recent_artists(&self) -> ClientResult<()> {
        let model = self.imp().recent_artists.clone();
        model.remove_all();

        self.client().get_recent_artists(&mut |artist| {model.append(&artist);}).await
    }

    pub async fn get_recent_songs(&self) -> ClientResult<Vec<Song>> {
        let model = self.imp().recent_songs.clone();
        model.remove_all();
        let settings = settings_manager().child("library");
        self.client().get_recent_songs(settings.uint("n-recent-songs")).await
    }
}
