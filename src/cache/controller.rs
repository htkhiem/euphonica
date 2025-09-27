// Cache system to store album arts, artist avatars, wikis, bios,
// you name it.
// This helps avoid having to query the same thing multiple times,
// whether from MPD or from Last.fm.
// Images are stored as resized PNG files on disk.
// - Album arts are named with hashes of their URIs (down to the album's
//   folder). This is because all albums have URIs, but not all have
//   MusicBrainz IDs.
// - Artist avatars are named with hashes of their names. Artist names can be substrings
//   of artist tags instead of the full tags.
// - Text data is stored as BSON blobs in SQLite.
use futures::TryFutureExt;
extern crate bson;
use lru::LruCache;
use std::{num::NonZeroUsize, sync::Mutex};
use async_channel::{Receiver, Sender};
use image::ImageReader;
use gio::prelude::*;
use glib::clone;
use gtk::{gdk::{self, Texture}, gio, glib};
use once_cell::sync::Lazy;
use uuid::Uuid;
use std::{
    cell::OnceCell, fmt, fs::create_dir_all, path::PathBuf, rc::Rc, sync::{Arc, RwLock}
};

use crate::{common::SongInfo, meta_providers::{get_provider, models::{ArtistMeta, Lyrics}}, utils::strip_filename_linux};
use crate::{
    client::{BackgroundTask, MpdWrapper},
    common::{AlbumInfo, ArtistInfo},
    meta_providers::{
        models, prelude::*, utils::get_best_image, MetadataChain, ProviderMessage,
    },
    utils::{resize_convert_image, settings_manager},
};

use super::{
    CacheState,
    sqlite
};

static APP_CACHE_PATH: Lazy<PathBuf> =
    Lazy::new(|| {
        let mut res = glib::user_cache_dir();
        res.push("euphonica");
        res
    });

pub fn get_app_cache_path() -> PathBuf {
    APP_CACHE_PATH.clone()
}

pub fn get_image_cache_path() -> PathBuf {
    let mut res = get_app_cache_path();
    res.push("images");
    res
}

pub fn get_doc_cache_path() -> PathBuf {
    let mut res = get_app_cache_path();
    res.push("metadata.sqlite");
    res
}

pub fn get_new_image_paths() -> (PathBuf, PathBuf) {
    let mut path = get_image_cache_path();
    let mut thumbnail_path = path.clone();
    path.push(Uuid::new_v4().simple().to_string() + ".png");
    thumbnail_path.push(Uuid::new_v4().simple().to_string() + ".png");
    (path, thumbnail_path)
}

// In-memory image cache.
// gdk::Textures are GObjects, which by themselves are boxed reference-counted.
// This means that even if a texture is evicted from this cache, as long as there
// is a widget on screen still using it, it will not actually leave RAM.
// This cache merely holds an additional reference to each texture to keep them
// around when no widget using them are being displayed, so as to reduce disk
// thrashing while quickly scrolling through like a million albums.
// This cache's keys are the filenames themselves.

// Currently we allow at most 15 columns in GridViews, which results in a maximum
// of 15 * (30 + 2) = 480 widgets being bound at any one time for each GridView.
// There are two big GridViews always kept in memory: the Album and Artist Views.
// To be safe, allow 960 textures to be kept in the cache at any one time.
static IMAGE_CACHE: Lazy<Mutex<LruCache<String, Texture>>> =
    Lazy::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(960).unwrap())));

pub struct Cache {
    mpd_client: OnceCell<Rc<MpdWrapper>>,
    fg_sender: Sender<ProviderMessage>, // For receiving notifications from other threads
    bg_sender: Sender<ProviderMessage>,
    meta_providers: Arc<RwLock<MetadataChain>>,
    state: CacheState,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Cache")
            .finish()
    }
}

fn init_meta_provider_chain() -> MetadataChain {
    let mut providers = MetadataChain::new();
    providers.providers = settings_manager()
        .child("metaprovider")
        .value("order")
        .array_iter_str()
        .unwrap()
        .map(|key| get_provider(key))
        .collect();
    providers
}

impl Cache {
    pub fn new() -> Rc<Self> {
        let (fg_sender, fg_receiver): (Sender<ProviderMessage>, Receiver<ProviderMessage>) =
            async_channel::unbounded();
        let (bg_sender, bg_receiver): (Sender<ProviderMessage>, Receiver<ProviderMessage>) =
            async_channel::unbounded();

        // Init folders
        create_dir_all(get_app_cache_path()).expect("ERROR: cannot create cache folders");
        create_dir_all(get_image_cache_path()).expect("ERROR: cannot create cache folders");

        let cache = Self {
            meta_providers: Arc::new(RwLock::new(init_meta_provider_chain())),
            mpd_client: OnceCell::new(),
            fg_sender: fg_sender.clone(),
            bg_sender,
            state: CacheState::default(),
        };
        let res = Rc::new(cache);

        res.clone()
            .setup_channel(bg_receiver, fg_sender, fg_receiver);
        res
    }
    /// Re-initialise list of providers when priority order is changed
    pub fn reinit_meta_providers(&self) {
        let mut curr_providers = self.meta_providers.write().unwrap();
        *curr_providers = init_meta_provider_chain();
    }

    pub fn set_mpd_client(&self, client: Rc<MpdWrapper>) {
        let _ = self.mpd_client.set(client);
    }

    pub fn get_sender(&self) -> Sender<ProviderMessage> {
        self.fg_sender.clone()
    }

    fn setup_channel(
        self: Rc<Self>,
        bg_receiver: Receiver<ProviderMessage>,
        fg_sender: Sender<ProviderMessage>,
        fg_receiver: Receiver<ProviderMessage>,
    ) {
        // Handle remote metadata fetching tasks in another thread
        let providers = self.clone().meta_providers.clone();
        glib::MainContext::default().spawn_local(
            async move {
                use futures::prelude::*;
                // Allow receiver to be mutated, but keep it at the same memory address.
                // See Receiver::next doc for why this is needed.
                let mut receiver = std::pin::pin!(bg_receiver);

                while let Some(request) = receiver.next().await {
                    match request {
                        ProviderMessage::AlbumMeta(mut key, overwrite) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                providers,
                                move || {
                                    // Check whether there is one already
                                    if key.mbid.is_some() || key.albumartist.is_some() {
                                        let folder_uri = key.folder_uri.to_owned();
                                        if overwrite || sqlite::find_album_meta(&key).ok().flatten().is_none() {
                                            let res = providers.read().unwrap().get_album_meta(&mut key, None);
                                            if let Some(album) = res {
                                                let _ = sqlite::write_album_meta(&key, &album);
                                            }
                                            else {
                                                // Push an empty AlbumMeta to block further calls for this album.
                                                println!("No album meta could be found for {}. Pushing empty document...", &folder_uri);
                                                let _ = sqlite::write_album_meta(&key, &models::AlbumMeta::from_key(&key));
                                            }
                                            let _ = fg_sender.send_blocking(ProviderMessage::AlbumMetaAvailable(folder_uri));
                                            sleep_after_request();
                                        }
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::ArtistMeta(mut key, overwrite) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                providers,
                                move || {
                                    // Check whether there is one already
                                    if overwrite || sqlite::find_artist_meta(&key).ok().flatten().is_none() {
                                        // Guaranteed to have this field so just unwrap it
                                        let name = key.name.to_owned();
                                        let res = providers.read().unwrap().get_artist_meta(&mut key, None);
                                        if let Some(artist) = res {
                                            sqlite::write_artist_meta(&key, &artist)
                                                .expect("Unable to write downloaded artist meta");
                                            if !sqlite::find_image_by_key(&key.name, Some("avatar"), false).expect("Sqlite DB error").is_some() {
                                                // Try to download artist avatar too
                                                let res = get_best_image(&artist.image);
                                                let (path, thumbnail_path) = get_new_image_paths();
                                                if res.is_ok() {
                                                    let (hires, thumbnail) = resize_convert_image(res.unwrap());
                                                    if let (Ok(_), Ok(_)) = (
                                                        hires.save(&path),
                                                        thumbnail.save(&thumbnail_path)
                                                    ) {
                                                        let _ = sqlite::register_image_key(
                                                            key.name.clone(), Some("avatar"),
                                                            Some(path.file_name().unwrap().to_str().unwrap().to_string()), false
                                                        );
                                                        let _ = sqlite::register_image_key(
                                                            key.name.clone(), Some("avatar"),
                                                            Some(thumbnail_path.file_name().unwrap().to_str().unwrap().to_string()), true
                                                        );
                                                        let hires_tex = gdk::Texture::from_filename(&path).unwrap();
                                                        let thumbnail_tex = gdk::Texture::from_filename(&thumbnail_path).unwrap();
                                                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name.clone(), false, hires_tex));
                                                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name.clone(), true, thumbnail_tex));
                                                    }
                                                }
                                                else {
                                                    println!("[Cache] Failed to download artist avatar for {:?} (perhaps all providers were disabled)", res.err());
                                                }
                                            }
                                        }
                                        else {
                                            // Push an empty ArtistMeta to block further calls for this album.
                                            println!("No artist meta could be found for {:?}. Pushing empty document...", &key);
                                            sqlite::write_artist_meta(&key, &models::ArtistMeta::from_key(&key))
                                                .expect("Unable to write downloaded artist meta");
                                        }
                                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistMetaAvailable(name));
                                        sleep_after_request();
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::FolderCover(album) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                move || {
                                    let mut success: bool = false;
                                    if sqlite::find_image_by_key(&album.folder_uri, None, false).expect("Sqlite DB error").is_some() {
                                        success = true;
                                    }
                                    else if let Ok(Some(meta)) = sqlite::find_album_meta(&album) {
                                        let res = get_best_image(&meta.image);
                                        if res.is_ok() {
                                            let (hires, thumbnail) = resize_convert_image(res.unwrap());
                                            let (path, thumbnail_path) = get_new_image_paths();
                                            if let (Ok(_), Ok(_)) = (
                                                hires.save(&path),
                                                thumbnail.save(&thumbnail_path)
                                            ) {
                                                let _ = sqlite::register_image_key(
                                                    album.folder_uri.clone(), None,
                                                    Some(path.file_name().unwrap().to_str().unwrap().to_string()), false
                                                );
                                                let _ = sqlite::register_image_key(
                                                    album.folder_uri.clone(), None,
                                                    Some(thumbnail_path.file_name().unwrap().to_str().unwrap().to_string()), true
                                                );
                                                let hires_tex = gdk::Texture::from_filename(&path).unwrap();
                                                let thumbnail_tex = gdk::Texture::from_filename(&thumbnail_path).unwrap();
                                                let _ = fg_sender.send_blocking(ProviderMessage::CoverAvailable(album.folder_uri.to_owned(), false, hires_tex));
                                                let _ = fg_sender.send_blocking(ProviderMessage::CoverAvailable(album.folder_uri.to_owned(), true, thumbnail_tex));
                                                success = true;
                                            }
                                        }
                                        sleep_after_request();
                                    }
                                    if !success {
                                        // End of the road, still unable to find anything for this folder (or its songs).
                                        println!("Cannot download folder cover for {} externally (perhaps all providers were disabled)", album.folder_uri);
                                        // Write empty entries to prevent further (fruitless) lookups
                                        let _ = sqlite::register_image_key(album.folder_uri.clone(), None, None, false);
                                        let _ = sqlite::register_image_key(album.folder_uri.clone(), None, None, true);
                                        let _ = fg_sender.send_blocking(ProviderMessage::CoverNotAvailable(album.folder_uri));
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::Lyrics(key) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                providers,
                                move || {
                                    // Guaranteed to have this field so just unwrap it
                                    let res = providers.read().unwrap().get_lyrics(&key);
                                    if let Some(lyrics) = res {
                                        sqlite::write_lyrics(&key, Some(&lyrics))
                                                 .expect("Unable to write downloaded lyrics");
                                        let _ = fg_sender.send_blocking(ProviderMessage::LyricsAvailable(key.uri));
                                    }
                                    sleep_after_request();
                                }
                            )).await;
                        }
                        _ => {}
                    };
                }
            }
        );
        let this = self.clone();
        // Listen to the background thread.
        glib::MainContext::default().spawn_local(async move {
            use futures::prelude::*;
            // Allow receiver to be mutated, but keep it at the same memory address.
            // See Receiver::next doc for why this is needed.
            let mut receiver = std::pin::pin!(fg_receiver);
            while let Some(notify) = receiver.next().await {
                match notify {
                    ProviderMessage::AlbumMetaAvailable(folder_uri) => {
                        this.on_album_meta_downloaded(&folder_uri)
                    }
                    ProviderMessage::ArtistMetaAvailable(name) => {
                        this.on_artist_meta_downloaded(&name)
                    }
                    ProviderMessage::CoverAvailable(uri, thumb, tex) => {
                        this.on_cover_downloaded(&uri, thumb, &tex)
                    }
                    ProviderMessage::CoverNotAvailable(_uri) => {
                        // TODO: use this to implement loading spinners for cover widgets
                    }
                    ProviderMessage::FolderCoverCleared(uri) => {
                        this.on_cover_cleared(&uri);
                    }
                    ProviderMessage::FetchFolderCoverExternally(album) => {
                        println!(
                            "MPD does not have cover for folder {}, will try fetching from external providers.",
                            &album.folder_uri
                        );
                        // Fill out metadata before attempting to fetch album art from external sources.
                        let _ = this.bg_sender.send_blocking(ProviderMessage::AlbumMeta(album.clone(), false));
                        let _ = this.bg_sender.send_blocking(ProviderMessage::FolderCover(album));
                    }
                    ProviderMessage::ArtistAvatarAvailable(name, thumb, tex) => {
                        this.on_artist_avatar_downloaded(&name, thumb, &tex)
                    }
                    ProviderMessage::ArtistAvatarCleared(name) => {
                        this.on_artist_avatar_cleared(&name)
                    }
                    ProviderMessage::PlaylistCoverAvailable(name, thumb, tex) => {
                        this.on_playlist_cover_downloaded(&name, thumb, &tex)
                    }
                    ProviderMessage::PlaylistCoverCleared(name) => {
                        this.on_playlist_cover_cleared(&name)
                    }
                    ProviderMessage::LyricsAvailable(key) => {
                        this.on_lyrics_downloaded(&key)
                    }
                    _ => {}
                }
            }
        });
    }

    fn on_album_meta_downloaded(&self, folder_uri: &str) {
        self.state
            .emit_with_param("album-meta-downloaded", folder_uri);
    }

    fn on_artist_meta_downloaded(&self, name: &str) {
        self.state.emit_with_param("artist-meta-downloaded", name);
    }

    fn on_cover_downloaded(&self, folder_uri: &str, is_thumbnail: bool, tex: &gdk::Texture) {
        self.state
            .emit_texture("album-art-downloaded", folder_uri, is_thumbnail, tex);
    }

    fn on_cover_cleared(&self, folder_uri: &str) {
        self.state
            .emit_with_param("album-art-cleared", folder_uri);
    }

    fn on_artist_avatar_downloaded(&self, name: &str, is_thumbnail: bool, tex: &gdk::Texture) {
        self.state.emit_texture("artist-avatar-downloaded", name, is_thumbnail, tex);
    }

    fn on_artist_avatar_cleared(&self, name: &str) {
        self.state.emit_with_param("artist-avatar-cleared", name);
    }

    fn on_playlist_cover_downloaded(&self, name: &str, is_thumbnail: bool, tex: &gdk::Texture) {
        self.state.emit_texture("playlist-cover-downloaded", name, is_thumbnail, tex);
    }

    fn on_playlist_cover_cleared(&self, name: &str) {
        self.state.emit_with_param("playlist-cover-cleared", name);
    }

    fn on_lyrics_downloaded(&self, uri: &str) {
        self.state.emit_with_param("song-lyrics-downloaded", uri);
    }

    pub fn get_cache_state(&self) -> CacheState {
        self.state.clone()
    }

    pub fn mpd_client(&self) -> Rc<MpdWrapper> {
        self.mpd_client.get().unwrap().clone()
    }

    /// Returns a gdk::Texture directly if one is currently cached in-memory.
    /// If not, it'll try to fetch from secondary storage and return asynchronously.
    /// Failing that, if schedule is set to true, it will try to get one from MPD.
    pub fn load_cached_embedded_cover(
        self: Rc<Self>,
        song: &SongInfo,
        thumbnail: bool,
        schedule: bool
    ) -> Option<(Texture, bool)> {
        let mut embedded_failed_before = false;
        let mut folder_failed_before = false;
        if let Some(filename) = sqlite::find_image_by_key(&song.uri, None, thumbnail)
            .expect("Sqlite DB error")
        {
            if !filename.is_empty() {
                if let Some(tex) = IMAGE_CACHE.lock().unwrap().get(&filename) {
                    // Cloning GObjects is cheap since they're just references
                    return Some((tex.clone(), true));
                }
                else {
                    let mut cover_path = get_image_cache_path();
                    cover_path.push(&filename);
                    let song = song.to_owned();
                    let fg_sender = self.fg_sender.clone();
                    glib::spawn_future_local(clone!(
                        #[weak(rename_to = this)]
                        self,
                        async move {
                            let uri = song.uri.to_owned();
                            let _ = gio::spawn_blocking(move || {
                                if let Ok(tex) = Texture::from_filename(&cover_path) {
                                    IMAGE_CACHE.lock().unwrap().put(
                                        filename,
                                        tex.clone()
                                    );
                                    let _ =
                                        fg_sender.send_blocking(ProviderMessage::CoverAvailable(uri, thumbnail, tex));
                                    // End here
                                    return None;
                                } else {
                                    // File no longer exists (maybe user had removed it). Unregister it from DB
                                    // and repeat process.
                                    sqlite::unregister_image_key(song.uri.clone(), None, thumbnail)
                                        .join().unwrap().expect("Sqlite DB error");
                                    println!("Unregistered image. Retrying...");
                                    // Return song info object to facilitate recursive retry
                                    return Some(song);

                                }
                            }).map_ok(move |song_to_retry| {
                                if let Some(song) = song_to_retry {
                                    this.clone().load_cached_embedded_cover(&song, thumbnail, schedule);
                                }
                            }).await;
                        }
                    ));
                    return None; // For now. Widgets will receive a signal later.
                }
            }
            else {
                embedded_failed_before = true;
            }
        }
        // Failed to get embedded art locally. Try falling back to folder art first.
        let folder_uri = strip_filename_linux(&song.uri);
        if let Some(filename) = sqlite::find_image_by_key(folder_uri, None, thumbnail)
            .expect("Sqlite DB error")
        {
            if !filename.is_empty() {
                if let Some(tex) = IMAGE_CACHE.lock().unwrap().get(&filename) {
                    // Note how we're returning false here since this isn't an embedded cover.
                    return Some((tex.clone(), false));
                } else {
                    let mut cover_path = get_image_cache_path();
                    cover_path.push(&filename);
                    let song = song.clone();
                    let folder_uri = folder_uri.to_owned();
                    let fg_sender = self.fg_sender.clone();
                    glib::spawn_future_local(clone!(
                        #[weak(rename_to = this)]
                        self,
                        async move {
                            let _ = gio::spawn_blocking(move || {
                                if let Ok(tex) = Texture::from_filename(&cover_path) {
                                    IMAGE_CACHE.lock().unwrap().put(
                                        filename,
                                        tex.clone()
                                    );
                                    let _ =
                                        fg_sender.send_blocking(
                                            ProviderMessage::CoverAvailable(
                                                folder_uri, thumbnail, tex
                                            )
                                        );
                                    // End here
                                    return None;
                                } else {
                                    // File no longer exists (maybe user had removed it). Unregister it from DB
                                    // and repeat process.
                                    sqlite::unregister_image_key(folder_uri.clone(), None, thumbnail)
                                        .join().unwrap().expect("Sqlite DB error");
                                    println!("Unregistered image. Retrying...");
                                    // Return song info object to facilitate recursive retry
                                    return Some(song);
                                }
                            }).map_ok(move |song_to_retry| {
                                if let Some(song) = song_to_retry {
                                    this.load_cached_embedded_cover(&song, thumbnail, schedule);
                                }
                            }).await;
                        }
                    ));
                    return None; // For now. Widgets will receive a signal later.
                }
            } else {
                folder_failed_before = true;
            }
        }
        if schedule {
            if !embedded_failed_before && settings_manager().child("client").boolean("mpd-download-album-art") {
                self.mpd_client
                    .get()
                    .unwrap()
                    .queue_background(
                        BackgroundTask::DownloadEmbeddedCover(song.clone()),
                        false
                    );
            } else if let (false, Some(album)) = (folder_failed_before, song.album.as_ref()) {
                // Skip straight to external providers
                self.bg_sender
                    .send_blocking(ProviderMessage::FolderCover(album.clone()))
                    .expect("Unable to schedule cover fetch");
            }
        }
        None
    }

    /// Returns a gdk::Texture directly if one is currently cached in-memory.
    /// If not, it'll try to fetch from secondary storage and return asynchronously.
    /// Failing that, if schedule is set to true, it will try to get one from MPD.
    /// Additionally returns a bool to indicate whether this cover is embedded or not, especially after fallbacks.
    pub fn load_cached_folder_cover(
        self: Rc<Self>,
        album: &AlbumInfo,
        thumbnail: bool,
        schedule: bool
    ) -> Option<(Texture, bool)> {
        let folder_uri = &album.folder_uri;
        let mut failed_before = false;
        // Try folder cover first
        if let Some(filename) = sqlite::find_image_by_key(folder_uri, None, thumbnail)
            .expect("Sqlite DB error")
        {
            if !filename.is_empty() {
                if let Some(tex) = IMAGE_CACHE.lock().unwrap().get(&filename) {
                    // Cloning GObjects is cheap since they're just references
                    return Some((tex.clone(), false));
                }
                else {
                    let mut cover_path = get_image_cache_path();
                    cover_path.push(&filename);
                    let folder_uri = folder_uri.to_owned();
                    let album = album.to_owned();
                    let fg_sender = self.fg_sender.clone();
                    glib::spawn_future_local(clone!(
                        #[weak(rename_to = this)]
                        self,
                        async move {
                            let _ = gio::spawn_blocking(move || {
                                if let Ok(tex) = Texture::from_filename(&cover_path) {
                                    IMAGE_CACHE.lock().unwrap().put(
                                        filename,
                                        tex.clone()
                                    );
                                    let _ =
                                        fg_sender.send_blocking(ProviderMessage::CoverAvailable(folder_uri, thumbnail, tex));
                                    return None;
                                }
                                else {
                                    // File no longer exists (maybe user had removed it). Unregister it from DB
                                    // and repeat process.
                                    sqlite::unregister_image_key(folder_uri.clone(), None, thumbnail)
                                        .join().unwrap().expect("Sqlite DB error");
                                    return Some(album);
                                }
                            }).map_ok(move |album_to_retry| {
                                if let Some(album) = album_to_retry {
                                    this.load_cached_folder_cover(&album, thumbnail, schedule);
                                }
                            }).await;
                        }
                    ));
                    return None; // For now. Widgets will receive a signal later.
                }
            } else {
                failed_before = true;
            }
        }
        // Failed to get folder cover locally. Try looking for a locally cached embedded
        // cover of a song in this folder.
        let uri = &album.example_uri;
        if let Some(filename) = sqlite::find_image_by_key(uri, None, thumbnail)
            .expect("Sqlite DB error")
            .map_or(None, |name| if name.len() > 0 {Some(name)} else {None})
        {
            if let Some(tex) = IMAGE_CACHE.lock().unwrap().get(&filename) {
                // Cloning GObjects is cheap since they're just references
                // Note the "true" here. We're returning an embedded cover instead due to fallback.
                return Some((tex.clone(), true));
            }
            else {
                let mut cover_path = get_image_cache_path();
                let uri = uri.to_owned();
                let album = album.clone();
                let fg_sender = self.fg_sender.clone();
                cover_path.push(&filename);
                glib::spawn_future_local(clone!(
                    #[weak(rename_to = this)]
                    self,
                    async move {
                        let _ = gio::spawn_blocking(move || {
                            if let Ok(tex) = Texture::from_filename(&cover_path) {
                                IMAGE_CACHE.lock().unwrap().put(
                                    filename,
                                    tex.clone()
                                );
                                let _ =
                                // Notify for this song. Albums whose folder contains this song should
                                // catch wind of this too.
                                    fg_sender.send_blocking(ProviderMessage::CoverAvailable(uri, thumbnail, tex));
                                return None;
                            } else {
                                // File no longer exists (maybe user had removed it). Unregister it from DB
                                // and repeat process.
                                sqlite::unregister_image_key(uri.clone(), None, thumbnail)
                                    .join().unwrap().expect("Sqlite DB error");
                                println!("Unregistered image. Retrying...");
                                return Some(album);
                            }
                        }).map_ok(|album_to_retry| {
                            if let Some(album) = album_to_retry {
                                this.load_cached_folder_cover(&album, thumbnail, schedule);
                            }
                        });
                    }
                ));
                return None; // For now. Widgets will receive a signal later.
            }
        }
        if schedule && !failed_before {
            if settings_manager().child("client").boolean("mpd-download-album-art") {
                self.mpd_client
                    .get()
                    .unwrap()
                    .queue_background(
                        BackgroundTask::DownloadFolderCover(album.clone()),
                        false
                    );
            }
            else {
                // Jump straight to external providers
                self.bg_sender
                    .send_blocking(ProviderMessage::FolderCover(album.clone()))
                    .expect("Unable to schedule cover fetch");
            }
        }
        None
    }

    /// Load the specified image, resize it, load into cache then send a message to frontend.
    /// All of the above is done in the background to avoid blocking UI.
    /// respond is a closure that uses the provided sender to send the hires and thumbnail versions to UI.
    fn set_image<F>(&self, key: &str, key_prefix: Option<&'static str>, path: &str, respond: F)
    where F: Send + FnOnce(Sender<ProviderMessage>, String, gdk::Texture, gdk::Texture) + 'static
    {
        let fg_sender = self.fg_sender.clone();
        let key = key.to_owned();
        let (hires_path, thumbnail_path) = get_new_image_paths();
        // Assume ashpd always return filesystem spec
        let filepath = urlencoding::decode(if path.starts_with("file://") {
            &path[7..]
        } else {
            path
        }).expect("Path must be in UTF-8").into_owned();
        gio::spawn_blocking(move || {
            let maybe_ptr = ImageReader::open(&filepath);
            if let Ok(ptr) = maybe_ptr {
                if let Ok(dyn_img) = ptr.decode() {
                    let (hires, thumbnail) = resize_convert_image(dyn_img);
                    if let (Ok(_), Ok(_)) = (hires.save(&hires_path), thumbnail.save(&thumbnail_path)) {
                        sqlite::register_image_key(
                            key.clone(), key_prefix,
                            Some(hires_path.file_name().unwrap().to_str().unwrap().to_owned()), false
                        ).join().unwrap().expect("Unable to register image key");
                        sqlite::register_image_key(
                            key.clone(), key_prefix,
                            Some(thumbnail_path.file_name().unwrap().to_str().unwrap().to_owned()), true
                        ).join().unwrap().expect("Unable to register image key");
                    }
                    // TODO: Optimise to avoid reading back from disk
                    let hires_tex = gdk::Texture::from_filename(&hires_path).unwrap();
                    let thumbnail_tex = gdk::Texture::from_filename(&thumbnail_path).unwrap();
                    {
                        let mut cache = IMAGE_CACHE.lock().unwrap();
                        cache.put(
                            hires_path.file_name().unwrap().to_str().unwrap().to_owned(),
                            hires_tex.clone()
                        );
                        cache.put(
                            thumbnail_path.file_name().unwrap().to_str().unwrap().to_owned(),
                            thumbnail_tex.clone(),
                        );
                    }
                    respond(fg_sender, key, hires_tex, thumbnail_tex);
                }
            }
            else {
                println!("{:?}", maybe_ptr.err());
            }
        });
    }


    /// Evict the image from cache and delete from cache folder on disk.
    /// This does not by itself yeet the image from memory (UI elements will still hold refs to it).
    /// We'll need to signal to these elements to clear themselves.
    pub fn clear_image<F>(&self, key: &str, key_prefix: Option<&'static str>, respond: F)
    where F: Send + FnOnce(Sender<ProviderMessage>, String) + 'static
    {
        let fg_sender = self.fg_sender.clone();
        let key = key.to_owned();
        gio::spawn_blocking(move || {
            if let Some(hires_name) = sqlite::find_image_by_key(&key, key_prefix, false).unwrap() {
                let mut hires_path = get_image_cache_path();
                hires_path.push(&hires_name);
                sqlite::unregister_image_key(key.clone(), key_prefix, false)
                    .join().unwrap().expect("Unable to unregister image key");
                IMAGE_CACHE.lock().unwrap().pop(&hires_name);
                let _ = std::fs::remove_file(hires_path);
            }
            if let Some(thumb_name) = sqlite::find_image_by_key(&key, key_prefix, true).unwrap() {
                let mut thumb_path = get_image_cache_path();
                thumb_path.push(&thumb_name);
                sqlite::unregister_image_key(key.clone(), key_prefix, true)
                    .join().unwrap().expect("Unable to unregister image key");
                IMAGE_CACHE.lock().unwrap().pop(&thumb_name);
                let _ = std::fs::remove_file(thumb_path);
            }
            respond(fg_sender, key);
            // let _ = fg_sender.send_blocking(ProviderMessage::FolderCoverCleared(folder_uri));
        });
    }


    pub fn set_cover(&self, folder_uri: &str, path: &str) {
        self.set_image(
            folder_uri, None, path,
            |sender, folder_uri, hires, thumb| {
                let _ = sender.send_blocking(ProviderMessage::CoverAvailable(folder_uri.clone(), false, hires));
                let _ = sender.send_blocking(ProviderMessage::CoverAvailable(folder_uri, true, thumb));
            }
        );
    }


    pub fn clear_cover(&self, folder_uri: &str) {
        self.clear_image(
            folder_uri, None,
            |sender, folder_uri| {
                let _ = sender.send_blocking(ProviderMessage::FolderCoverCleared(folder_uri));
            }
        );
    }


    pub fn set_artist_avatar(&self, tag: &str, path: &str) {
        self.set_image(
            tag, Some("avatar"), path,
            |sender, tag, hires, thumb| {
                let _ = sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(tag.clone(), false, hires));
                let _ = sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(tag, true, thumb));
            }
        );
    }


    pub fn clear_artist_avatar(&self, tag: &str) {
        self.clear_image(
            tag, Some("avatar"),
            |sender, tag| {
                let _ = sender.send_blocking(ProviderMessage::ArtistAvatarCleared(tag));
            }
        );
    }


    pub fn set_playlist_cover(&self, playlist_name: &str, path: &str) {
        self.set_image(
            playlist_name, Some("playlist"), path,
            |sender, playlist_name, hires, thumb| {
                let _ = sender.send_blocking(ProviderMessage::PlaylistCoverAvailable(playlist_name.clone(), false, hires));
                let _ = sender.send_blocking(ProviderMessage::PlaylistCoverAvailable(playlist_name, true, thumb));
            }
        );
    }


    pub fn clear_playlist_cover(&self, playlist_name: &str) {
        self.clear_image(
            playlist_name, Some("playlist"),
            |sender, playlist_name| {
                let _ = sender.send_blocking(ProviderMessage::PlaylistCoverCleared(playlist_name));
            }
        );
    }


    pub fn load_cached_album_meta(&self, album: &AlbumInfo) -> Option<models::AlbumMeta> {
        // Check whether we have this album cached
        let result = sqlite::find_album_meta(album);
        if let Ok(res) = result {
            if let Some(info) = res {
                return Some(info);
            }
            return None;
        }
        println!("{:?}", result.err());
        return None;
    }

    pub fn fetch_album_meta(&self, album: &AlbumInfo, overwrite: bool) {
        // Check whether we have this album cached
        if overwrite || sqlite::find_album_meta(album).ok().flatten().is_none() {
            self.bg_sender
                .send_blocking(ProviderMessage::AlbumMeta(album.clone(), overwrite))
                .expect("[Cache] Unable to schedule album meta fetch task");
        }
    }

    pub fn load_cached_artist_meta(&self, artist: &ArtistInfo) -> Option<ArtistMeta> {
        let result = sqlite::find_artist_meta(artist);
        if let Ok(res) = result {
            if let Some(info) = res {
                return Some(info);
            }
            return None;
        }
        println!("{:?}", result.err());
        return None;
    }

    pub fn fetch_artist_meta(&self, artist: &ArtistInfo, overwrite: bool) {
        // Check whether we have this artist cached
        if overwrite || sqlite::find_artist_meta(artist).ok().flatten().is_none() {
            self.bg_sender.send_blocking(ProviderMessage::ArtistMeta(artist.clone(), overwrite))
                .expect("[Cache] Unable to schedule artist meta fetch task");
        }
    }

    /// Public method to allow other controllers to get artist avatars for
    /// directly if possible.
    /// Without this, they can only get the textures via signals, which have overhead.
    /// To queue downloading artist avatars, simply use ensure_cached_artist_meta, which
    /// will also download artist avatars if the provider is configured to do so.
    pub fn load_cached_artist_avatar(
        self: Rc<Self>,
        artist: &ArtistInfo,
        thumbnail: bool,
    ) -> Option<Texture> {
        // First try to get from cache
        let name = &artist.name;
        if let Some(filename) = sqlite::find_image_by_key(name, Some("avatar"), thumbnail).expect("Sqlite DB error") {
            if let Some(tex) = IMAGE_CACHE.lock().unwrap().get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some(tex.clone());
            }
            // If missed, try loading from disk
            let fg_sender = self.fg_sender.clone();
            let artist = artist.to_owned();
            glib::spawn_future_local(clone!(
                #[weak(rename_to = this)]
                self,
                async move {
                    let _ = gio::spawn_blocking(move || {
                        let mut path = get_image_cache_path();
                        path.push(&filename);
                        // Try to load from disk. Do this using the threadpool to avoid blocking UI.
                        if let Ok(tex) = Texture::from_filename(&path) {
                            IMAGE_CACHE.lock().unwrap().put(filename, tex.clone());
                            let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(artist.name.to_owned(), thumbnail, tex));
                            return None;
                        } else {
                            // File no longer exists (maybe user had removed it). Unregister it from DB
                            // and repeat process.
                            sqlite::unregister_image_key(artist.name.clone(), Some("avatar"), thumbnail)
                                .join().unwrap().expect("Sqlite DB error");
                            println!("Unregistered image. Retrying...");
                            // Return song info object to facilitate recursive retry
                            return Some(artist);
                        }
                    }).map_ok(move |artist_to_retry| {
                        if let Some(artist) = artist_to_retry {
                            this.load_cached_artist_avatar(&artist, thumbnail);
                        }
                    }).await;
                }
            ));
        }
        None
    }

    /// Public method to allow other controllers to get playlist cover images for
    /// directly if possible.
    /// Without this, they can only get the textures via signals, which have overhead.
    pub fn load_cached_playlist_cover(
        self: Rc<Self>,
        playlist_name: &str,
        thumbnail: bool,
    ) -> Option<Texture> {
        // First try to get from cache
        if let Some(filename) = sqlite::find_image_by_key(playlist_name, Some("playlist"), thumbnail).expect("Sqlite DB error") {
            if let Some(tex) = IMAGE_CACHE.lock().unwrap().get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some(tex.clone());
            }
            // If missed, try loading from disk
            let fg_sender = self.fg_sender.clone();
            let playlist_name = playlist_name.to_owned();
            let _ = gio::spawn_blocking(move || {
                let mut path = get_image_cache_path();
                path.push(&filename);
                // Try to load from disk. Do this using the threadpool to avoid blocking UI.
                if let Ok(tex) = Texture::from_filename(&path) {
                    IMAGE_CACHE.lock().unwrap().put(filename, tex.clone());
                    let _ = fg_sender.send_blocking(ProviderMessage::PlaylistCoverAvailable(playlist_name, thumbnail, tex));
                } else {
                    // File no longer exists (maybe user had removed it). Unregister it from DB.
                    // NO NEED TO REPEAT PROCESS. There is nothing to fetch automatically.
                    println!("Playlist cover no longer present on disk. Unregistering...");
                    sqlite::unregister_image_key(playlist_name, Some("playlist"), thumbnail)
                        .join().unwrap().expect("Sqlite DB error");
                }
            });
        }
        None
    }

    pub fn load_cached_lyrics(&self, song: &SongInfo) -> Option<Lyrics> {
        let result = sqlite::find_lyrics(song);
        if let Ok(res) = result {
            if let Some(info) = res {
                return Some(info);
            }
            return None;
        }
        println!("{:?}", result.err());
        return None;
    }

    pub fn ensure_cached_lyrics(&self, song: &SongInfo) {
        // Check whether we have this artist cached
        let result = sqlite::find_lyrics(song);
        if let Ok(response) = result {
            if response.is_none() {
                let _ = self.bg_sender.send_blocking(ProviderMessage::Lyrics(
                    song.clone()
                ));
            }
        } else {
            println!("{:?}", result.err());
        }
    }
}
