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
extern crate bson;
extern crate stretto;
use async_channel::{Receiver, Sender};
use image::io::Reader as ImageReader;
use gio::prelude::*;
use glib::clone;
use gtk::{gdk::{self, Texture}, gio, glib};
use once_cell::sync::Lazy;
use uuid::Uuid;
use std::{
    cell::OnceCell,
    fmt,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, RwLock},
};

use crate::{common::SongInfo, meta_providers::{get_provider_with_priority, models::{ArtistMeta, Lyrics}}, utils::strip_filename_linux};
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

// In-memory image cache. Declared here to ease usage between threads as Stretto
// is already internally-mutable.
// gdk::Textures are GObjects, which by themselves are boxed reference-counted.
// This means that even if a texture is evicted from this cache, as long as there
// is a widget on screen still using it, it will not actually leave RAM.
// This cache merely holds an additional reference to each texture to keep them
// around when no widget using them are being displayed, so as to reduce disk
// thrashing while quickly scrolling through like a million albums.
// This cache's keys are the filenames themselves.
// TODO: figure out max cost based on user-selectable RAM limit
// TODO: figure out cost of textures based on user-selectable resolution
static IMAGE_CACHE: Lazy<stretto::Cache<String, Texture>> =
    Lazy::new(|| stretto::Cache::new(327680, 32768).unwrap());

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
    let mut providers = MetadataChain::new(0);
    providers.providers = settings_manager()
        .child("metaprovider")
        .value("order")
        .array_iter_str()
        .unwrap()
        .enumerate()
        .map(|(prio, key)| get_provider_with_priority(key, prio as u32))
        .collect();
    providers
}

impl Cache {
    pub fn new() -> Rc<Self> {
        let (fg_sender, fg_receiver): (Sender<ProviderMessage>, Receiver<ProviderMessage>) =
            async_channel::bounded(1);
        let (bg_sender, bg_receiver): (Sender<ProviderMessage>, Receiver<ProviderMessage>) =
            async_channel::unbounded();

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
                        ProviderMessage::AlbumMeta(mut key) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                providers,
                                move || {
                                    // Check whether there is one already
                                    if key.mbid.is_some() || key.artist_tag.is_some() {
                                        let folder_uri = key.folder_uri.to_owned();
                                        let existing = sqlite::find_album_meta(&key);
                                        if let Ok(None) = existing {
                                            let res = providers.read().unwrap().get_album_meta(&mut key, None);
                                            if let Some(album) = res {
                                                sqlite::write_album_meta(&key, &album)
                                                    .expect("Unable to store downloaded album meta");
                                            }
                                            else {
                                                // Push an empty AlbumMeta to block further calls for this album.
                                                println!("No album meta could be found for {}. Pushing empty document...", &folder_uri);
                                                sqlite::write_album_meta(&key, &models::AlbumMeta::from_key(&key))
                                                    .expect("Unable to store placeholder album meta");
                                            }
                                            let _ = fg_sender.send_blocking(ProviderMessage::AlbumMetaAvailable(folder_uri));
                                            sleep_after_request();
                                        }
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::ArtistMeta(mut key) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                providers,
                                move || {
                                    // Check whether there is one already
                                    let existing = sqlite::find_artist_meta(&key);
                                    if let Ok(None) = existing {
                                        // Guaranteed to have this field so just unwrap it
                                        let name = key.name.to_owned();
                                        let res = providers.read().unwrap().get_artist_meta(&mut key, None);
                                        if let Some(artist) = res {
                                            sqlite::write_artist_meta(&key, &artist)
                                                .expect("Unable to write downloaded artist meta");
                                            if sqlite::find_image_by_key(&key.name, false).expect("Sqlite DB error").is_some() {
                                                let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name.clone()));
                                            }
                                            else {
                                                // Try to download artist avatar too
                                                let res = get_best_image(&artist.image);
                                                let (path, thumbnail_path) = get_new_image_paths();
                                                if res.is_ok() {
                                                    let (hires, thumbnail) = resize_convert_image(res.unwrap());
                                                    if let (Ok(_), Ok(_)) = (
                                                        hires.save(&path),
                                                        thumbnail.save(&thumbnail_path)
                                                    ) {
                                                        sqlite::register_image_key(&key.name, Some(path.file_name().unwrap().to_str().unwrap()), false);
                                                        sqlite::register_image_key(&key.name, Some(thumbnail_path.file_name().unwrap().to_str().unwrap()), true);
                                                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name.clone()));
                                                    }
                                                }
                                                else {
                                                    println!("[Cache] Failed to download artist avatar: {:?}", res.err());
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
                        ProviderMessage::FolderCover(album, fallback) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                move || {
                                    if sqlite::find_image_by_key(&album.folder_uri, false).expect("Sqlite DB error").is_some() {
                                        let _ = fg_sender.send_blocking(ProviderMessage::CoverAvailable(album.folder_uri.to_owned()));
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
                                                sqlite::register_image_key(&album.folder_uri, Some(path.file_name().unwrap().to_str().unwrap()), false);
                                                sqlite::register_image_key(&album.folder_uri, Some(thumbnail_path.file_name().unwrap().to_str().unwrap()), true);
                                                let _ = fg_sender.send_blocking(ProviderMessage::CoverAvailable(album.folder_uri.to_owned()));
                                            }
                                        }
                                        sleep_after_request();
                                    }
                                    else {
                                        println!("Cannot download cover: no local album meta could be found for {}", album.folder_uri);
                                        if fallback {
                                            let _ = fg_sender.send_blocking(ProviderMessage::FallbackToEmbeddedCover(album));
                                        }
                                        else {
                                            // Write empty entries to prevent further (fruitless) lookups
                                            sqlite::register_image_key(&album.folder_uri, None, false);
                                            sqlite::register_image_key(&album.folder_uri, None, true);
                                        }
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
                                        sqlite::write_lyrics(&key, &lyrics)
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
                    ProviderMessage::CoverAvailable(uri) => {
                        this.on_cover_downloaded(&uri)
                    }
                    ProviderMessage::CoverNotAvailable(_uri) => {
                        // TODO: use this to implement loading spinners for cover widgets
                    }
                    ProviderMessage::ClearFolderCover(uri) => {
                        this.on_cover_cleared(&uri);
                    }
                    ProviderMessage::FallbackToFolderCover(album) => {
                        println!(
                            "Falling back to looking for a cover image in folder {}.",
                            &album.folder_uri
                        );
                        if this.load_cached_folder_cover(&album, true, true, false).is_some() {
                            // Already have it in-memory
                            this.on_cover_downloaded(&album.folder_uri);
                        }
                    }
                    ProviderMessage::FetchFolderCoverExternally(album, fallback) => {
                        println!(
                            "MPD does not have cover for folder {}, will try fetching from external providers.",
                            &album.folder_uri
                        );
                        // Fill out metadata before attempting to fetch album art from external sources.
                        let _ = this.bg_sender.send_blocking(ProviderMessage::AlbumMeta(album.clone()));
                        let _ = this.bg_sender.send_blocking(ProviderMessage::FolderCover(album, fallback));
                    }
                    ProviderMessage::ArtistAvatarAvailable(name) => {
                        this.on_artist_avatar_downloaded(&name)
                    }
                    ProviderMessage::ClearArtistAvatar(name) => {
                        this.on_artist_avatar_cleared(&name)
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

    fn on_cover_downloaded(&self, folder_uri: &str) {
        self.state
            .emit_with_param("album-art-downloaded", folder_uri);
    }

    fn on_cover_cleared(&self, folder_uri: &str) {
        self.state
            .emit_with_param("album-art-cleared", folder_uri);
    }

    fn on_artist_avatar_downloaded(&self, name: &str) {
        self.state.emit_with_param("artist-avatar-downloaded", name);
    }

    fn on_artist_avatar_cleared(&self, name: &str) {
        self.state.emit_with_param("artist-avatar-cleared", name);
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
    /// This version accepts a Song object only and works for songs without Album tags,
    /// but in return it won't be able to query from external sources if said Song object
    /// has no album tag.
    /// Additionally returns a bool to indicate whether this cover is embedded or not, especially after fallbacks.
    pub fn load_cached_embedded_cover(
        &self,
        song: &SongInfo,
        thumbnail: bool,
        schedule: bool,
        fallback: bool
    ) -> Option<(Texture, bool)> {
        if let Some(filename) = sqlite::find_image_by_key(&song.uri, thumbnail).expect("Sqlite DB error") {
            if filename.len() == 0 {
                // Tried fetching before, nothing found, don't try again.
                println!("Won't fetch embedded cover for song {} again (failed before)", &song.uri);
                return None;
            }
            if let Some(tex) = IMAGE_CACHE.get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some((tex.value().clone(), true));
            }
            else {
                let mut cover_path = get_image_cache_path();
                let song_uri = song.uri.to_owned();
                cover_path.push(&filename);
                if cover_path.exists() {
                    let fg_sender = self.fg_sender.clone();
                    gio::spawn_blocking(move || {
                        if let Ok(tex) = Texture::from_filename(&cover_path) {
                            IMAGE_CACHE.insert(
                                cover_path.to_str().unwrap().to_owned(),
                                tex.clone(),
                                if thumbnail { 1 } else { 16 },
                            );
                            IMAGE_CACHE.wait().unwrap();
                            let _ =
                                fg_sender.send_blocking(ProviderMessage::CoverAvailable(song_uri));
                        }
                    });
                } else {
                    // TODO: handle situation when Sqlite table is delusional more gracefully
                    panic!("Couldn't find file {} (cached embedded cover of song {})", filename, &song.uri);
                }
            }
        }
        else if schedule {
            if let Some(album) = song.album.as_ref() {
                // Call MPD readpicture with optional fallback
                self.mpd_client
                    .get()
                    .unwrap()
                    .queue_background(
                        BackgroundTask::DownloadEmbeddedCoverForAlbum(album.clone(), fallback),
                        false
                    );
            }
            else {
                // Call MPD readpicture, without fallback
                self.mpd_client
                    .get()
                    .unwrap()
                    .queue_background(
                        BackgroundTask::DownloadEmbeddedCover(song.clone()),
                        false
                    );
            }
        }
        else if fallback {
            if let Some(album) = song.album.as_ref() {
                return self.load_cached_folder_cover(album, thumbnail, schedule, false);
            }
        }
        None
    }

    /// Returns a gdk::Texture directly if one is currently cached in-memory.
    /// If not, it'll try to fetch from secondary storage and return asynchronously.
    /// Failing that, if schedule is set to true, it will try to get one from MPD.
    /// If fallback is enabled, it will also look for folder-level covers in case no embedded art is available.
    /// This version exists for widgets that don't keep references to a song, only to an album.
    /// Additionally returns a bool to indicate whether this cover is embedded or not, especially after fallbacks.
    pub fn load_cached_embedded_cover_for_album(
        &self,
        album: &AlbumInfo,
        thumbnail: bool,
        schedule: bool,
        fallback: bool
    ) -> Option<(Texture, bool)> {
        if let Some(filename) = sqlite::find_image_by_key(&album.example_uri, thumbnail).expect("Sqlite DB error") {
            if filename.len() == 0 {
                // Tried fetching before, nothing found, don't try again.
                println!("Won't fetch embedded cover for song {} again (failed before)", &album.example_uri);
                return None;
            }
            if let Some(tex) = IMAGE_CACHE.get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some((tex.value().clone(), true));
            }
            else {
                let mut cover_path = get_image_cache_path();
                let song_uri = album.example_uri.to_owned();
                cover_path.push(&filename);
                if cover_path.exists() {
                    let fg_sender = self.fg_sender.clone();
                    gio::spawn_blocking(move || {
                        if let Ok(tex) = Texture::from_filename(&cover_path) {
                            IMAGE_CACHE.insert(
                                cover_path.to_str().unwrap().to_owned(),
                                tex.clone(),
                                if thumbnail { 1 } else { 16 },
                            );
                            IMAGE_CACHE.wait().unwrap();
                            let _ =
                                fg_sender.send_blocking(ProviderMessage::CoverAvailable(song_uri));
                        }
                    });
                } else {
                    // TODO: handle situation when Sqlite table is delusional more gracefully
                    panic!("Couldn't find file {} (cached embedded cover of song {})", filename, &album.example_uri);
                }
            }
        }
        else if schedule {
            // Call MPD readpicture
            self.mpd_client
                .get()
                .unwrap()
                .queue_background(
                    BackgroundTask::DownloadEmbeddedCoverForAlbum(album.clone(), fallback),
                    false
                );
        }
        else if fallback {
            return self.load_cached_folder_cover(album, thumbnail, schedule, false);
        }
        None
    }

    /// Returns a gdk::Texture directly if one is currently cached in-memory.
    /// If not, it'll try to fetch from secondary storage and return asynchronously.
    /// Failing that, if schedule is set to true, it will try to get one from MPD.
    /// If fallback is enabled, it will also look for embedded covers in case no folder-level cover is available.
    /// Additionally returns a bool to indicate whether this cover is embedded or not, especially after fallbacks.
    pub fn load_cached_folder_cover(
        &self,
        album: &AlbumInfo,
        thumbnail: bool,
        schedule: bool,
        fallback: bool
    ) -> Option<(Texture, bool)> {
        let folder_uri = &album.folder_uri;
        if let Some(filename) = sqlite::find_image_by_key(folder_uri, thumbnail)
            .expect("Sqlite DB error") {
            if filename.len() == 0 {
                // Tried fetching before, nothing found, don't try again.
                println!("Won't fetch embedded cover for folder {} again (failed before)", &album.folder_uri);
                return None;
            }
            if let Some(tex) = IMAGE_CACHE.get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some((tex.value().clone(), false));
            }
            else {
                let mut cover_path = get_image_cache_path();
                let folder_uri = folder_uri.to_owned();
                cover_path.push(&filename);
                if cover_path.exists() {
                    let fg_sender = self.fg_sender.clone();
                    gio::spawn_blocking(move || {
                        if let Ok(tex) = Texture::from_filename(&cover_path) {
                            IMAGE_CACHE.insert(
                                cover_path.to_str().unwrap().to_owned(),
                                tex.clone(),
                                if thumbnail { 1 } else { 16 },
                            );
                            IMAGE_CACHE.wait().unwrap();
                            let _ =
                                fg_sender.send_blocking(ProviderMessage::CoverAvailable(folder_uri));
                        }
                    });
                } else {
                    // TODO: handle situation when Sqlite table is delusional more gracefully
                    panic!("Couldn't find file {} (cached embedded cover of album {})", filename, &folder_uri);
                }
            }
        }
        else if schedule {
            self.mpd_client
                .get()
                .unwrap()
                .queue_background(
                    BackgroundTask::DownloadFolderCover(album.clone(), fallback),
                    false
                );
        }
        else if fallback {
            return self.load_cached_embedded_cover_for_album(album, thumbnail, schedule, false);
        }
        None
    }

    /// Returns a gdk::Texture directly if one is currently cached in-memory.
    /// If not, it'll try to fetch from secondary storage and return asynchronously.
    /// Failing that, if schedule is set to true, it will try to get one from MPD.
    /// If fallback is enabled, it will also look for embedded covers in case no folder-level cover is available.
    /// Additionally returns a bool to indicate whether this cover is embedded or not, especially after fallbacks.
    /// This version accepts a Song object only and works for songs without Album tags,
    /// but in return it won't be able to query from external sources if said Song object
    /// has no album tag.
    pub fn load_cached_folder_cover_for_song(
        &self,
        song: &SongInfo,
        thumbnail: bool,
        schedule: bool,
        fallback: bool
    ) -> Option<(Texture, bool)> {
        let folder_uri = strip_filename_linux(&song.uri);
        if let Some(filename) = sqlite::find_image_by_key(folder_uri, thumbnail)
            .expect("Sqlite DB error") {
            if filename.len() == 0 {
                // Tried fetching before, nothing found, don't try again.
                println!("Won't fetch embedded cover for folder {} again (failed before)", &folder_uri);
                return None;
            }
            if let Some(tex) = IMAGE_CACHE.get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some((tex.value().clone(), false));
            }
            else {
                let mut cover_path = get_image_cache_path();
                let folder_uri = folder_uri.to_owned();
                cover_path.push(&filename);
                if cover_path.exists() {
                    let fg_sender = self.fg_sender.clone();
                    gio::spawn_blocking(move || {
                        if let Ok(tex) = Texture::from_filename(&cover_path) {
                            IMAGE_CACHE.insert(
                                cover_path.to_str().unwrap().to_owned(),
                                tex.clone(),
                                if thumbnail { 1 } else { 16 },
                            );
                            IMAGE_CACHE.wait().unwrap();
                            let _ =
                                fg_sender.send_blocking(ProviderMessage::CoverAvailable(folder_uri));
                        }
                    });
                } else {
                    // TODO: handle situation when Sqlite table is delusional more gracefully
                    panic!("Couldn't find file {} (cached embedded cover of album {})", filename, &folder_uri);
                }
            }
        }
        else if schedule {
            if let Some(album) = song.album.as_ref() {
                self.mpd_client
                .get()
                .unwrap()
                .queue_background(
                    BackgroundTask::DownloadFolderCover(album.clone(), fallback),
                    false
                );
            }
            else {
                self.mpd_client
                    .get()
                    .unwrap()
                    .queue_background(
                        BackgroundTask::DownloadFolderCoverForSong(song.clone()),
                        false
                    );
            }
        }
        else if fallback {
            return self.load_cached_embedded_cover(song, thumbnail, schedule, false);
        }
        None
    }

    /// Load the specified image, resize it, load into cache then send a message to frontend.
    /// All of the above must be done in the background to avoid blocking UI.
    pub fn set_cover(&self, folder_uri: &str, path: &str) {
        let fg_sender = self.fg_sender.clone();
        let folder_uri = folder_uri.to_owned();
        let (hires_path, thumbnail_path) = get_new_image_paths();
        // Assume ashpd always return filesystem spec
        let filepath = urlencoding::decode(if path.starts_with("file://") {
            &path[7..]
        } else {
            path
        }).expect("UTF-8").into_owned();
        println!("{:?}", filepath);
        gio::spawn_blocking(move || {
            let maybe_ptr = ImageReader::open(&filepath);
            if let Ok(ptr) = maybe_ptr {
                if let Ok(dyn_img) = ptr.decode() {
                    let (hires, thumbnail) = resize_convert_image(dyn_img);
                    let _ = hires.save(&hires_path);
                    let _ = thumbnail.save(&thumbnail_path);
                    sqlite::register_image_key(&folder_uri, Some(hires_path.file_name().unwrap().to_str().unwrap()), false);
                    sqlite::register_image_key(&folder_uri, Some(thumbnail_path.file_name().unwrap().to_str().unwrap()), true);
                    // TODO: Optimise to avoid reading back from disk
                    IMAGE_CACHE.insert(
                        hires_path.file_name().unwrap().to_str().unwrap().to_owned(),
                        gdk::Texture::from_filename(&hires_path).unwrap(),
                        16
                    );
                    IMAGE_CACHE.insert(
                        hires_path.file_name().unwrap().to_str().unwrap().to_owned(),
                        gdk::Texture::from_filename(&thumbnail_path).unwrap(),
                        1
                    );
                    IMAGE_CACHE.wait().unwrap();
                    let _ = fg_sender.send_blocking(ProviderMessage::CoverAvailable(folder_uri));
                }
            }
            else {
                println!("{:?}", maybe_ptr.err());
            }
        });
   }

    /// Evict the album art from cache and delete from cache folder on disk.
    /// This does not by itself yeet the art from memory (UI elements will still hold refs to it).
    /// We'll need to signal to these elements to clear themselves.
    pub fn clear_cover(&self, folder_uri: &str) {
        let fg_sender = self.fg_sender.clone();
        let folder_uri = folder_uri.to_owned();
        gio::spawn_blocking(move || {
            if let Some(hires_name) = sqlite::find_image_by_key(&folder_uri, false).unwrap() {
                let mut hires_path = get_image_cache_path();
                hires_path.push(&hires_name);
                sqlite::unregister_image_key(&folder_uri, false);
                IMAGE_CACHE.remove(&hires_name);
                let _ = std::fs::remove_file(hires_path);
            }
            if let Some(thumb_name) = sqlite::find_image_by_key(&folder_uri, true).unwrap() {
                let mut thumb_path = get_image_cache_path();
                thumb_path.push(&thumb_name);
                sqlite::unregister_image_key(&folder_uri, true);
                IMAGE_CACHE.remove(&thumb_name);
                let _ = std::fs::remove_file(thumb_path);
            }
            let _ = fg_sender.send_blocking(ProviderMessage::ClearFolderCover(folder_uri));
        });
    }

    /// Load the specified image, resize it, load into cache then send a message to frontend.
    /// All of the above must be done in the background to avoid blocking UI.
    pub fn set_artist_avatar(&self, tag: &str, path: &str) {
        let fg_sender = self.fg_sender.clone();
        let tag = tag.to_owned();
        let (hires_path, thumbnail_path) = get_new_image_paths();
        // Assume ashpd always return filesystem spec
        let filepath = urlencoding::decode(if path.starts_with("file://") {
            &path[7..]
        } else {
            path
        }).expect("UTF-8").into_owned();
        println!("{:?}", filepath);
        gio::spawn_blocking(move || {
            let maybe_ptr = ImageReader::open(&filepath);
            if let Ok(ptr) = maybe_ptr {
                if let Ok(dyn_img) = ptr.decode() {
                    let (hires, thumbnail) = resize_convert_image(dyn_img);
                    let _ = hires.save(&hires_path);
                    let _ = thumbnail.save(&thumbnail_path);
                    sqlite::register_image_key(&tag, Some(hires_path.file_name().unwrap().to_str().unwrap()), false);
                    sqlite::register_image_key(&tag, Some(thumbnail_path.file_name().unwrap().to_str().unwrap()), true);
                    // TODO: Optimise to avoid reading back from disk
                    IMAGE_CACHE.insert(
                        hires_path.file_name().unwrap().to_str().unwrap().to_owned(),
                        gdk::Texture::from_filename(&hires_path).unwrap(),
                        16
                    );
                    IMAGE_CACHE.insert(
                        hires_path.file_name().unwrap().to_str().unwrap().to_owned(),
                        gdk::Texture::from_filename(&thumbnail_path).unwrap(),
                        1
                    );
                    IMAGE_CACHE.wait().unwrap();
                    let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(tag));
                }
            }
            else {
                println!("{:?}", maybe_ptr.err());
            }
        });
    }

    /// Evict the album art from cache and delete from cache folder on disk.
    /// This does not by itself yeet the art from memory (UI elements will still hold refs to it).
    /// We'll need to signal to these elements to clear themselves.
    pub fn clear_artist_avatar(&self, tag: &str) {
        let fg_sender = self.fg_sender.clone();
        let tag = tag.to_owned();
        gio::spawn_blocking(move || {
            if let Some(hires_name) = sqlite::find_image_by_key(&tag, false).unwrap() {
                let mut hires_path = get_image_cache_path();
                hires_path.push(&hires_name);
                sqlite::unregister_image_key(&tag, false);
                IMAGE_CACHE.remove(&hires_name);
                let _ = std::fs::remove_file(hires_path);
            }
            if let Some(thumb_name) = sqlite::find_image_by_key(&tag, true).unwrap() {
                let mut thumb_path = get_image_cache_path();
                thumb_path.push(&thumb_name);
                sqlite::unregister_image_key(&tag, true);
                IMAGE_CACHE.remove(&thumb_name);
                let _ = std::fs::remove_file(thumb_path);
            }
            let _ = fg_sender.send_blocking(ProviderMessage::ClearArtistAvatar(tag));
        });
    }


    pub fn load_cached_album_meta(&self, album: &AlbumInfo) -> Option<models::AlbumMeta> {
        // Check whether we have this album cached
        let result = sqlite::find_album_meta(album);
        if let Ok(res) = result {
            if let Some(info) = res {
                println!("Album info cache hit!");
                return Some(info);
            }
            println!("Album info cache miss");
            return None;
        }
        println!("{:?}", result.err());
        return None;
    }

    pub fn ensure_cached_album_meta(&self, album: &AlbumInfo) {
        // Check whether we have this album cached
        let result = sqlite::find_album_meta(album);
        if let Ok(response) = result {
            if response.is_none() {
                self.bg_sender
                    .send_blocking(ProviderMessage::AlbumMeta(album.clone()))
                    .expect("[Cache] Unable to schedule album meta fetch task");
            }
        } else {
            println!("{:?}", result.err());
        }
    }

    pub fn load_cached_artist_meta(&self, artist: &ArtistInfo) -> Option<ArtistMeta> {
        let result = sqlite::find_artist_meta(artist);
        if let Ok(res) = result {
            if let Some(info) = res {
                println!("Artist info cache hit!");
                return Some(info);
            }
            println!("Artist info cache miss");
            return None;
        }
        println!("{:?}", result.err());
        return None;
    }

    pub fn ensure_cached_artist_meta(&self, artist: &ArtistInfo) {
        // Check whether we have this artist cached
        let result = sqlite::find_artist_meta(artist);
        if let Ok(response) = result {
            if response.is_none() {
                let _ = self.bg_sender.send_blocking(ProviderMessage::ArtistMeta(artist.clone()));
            }
        } else {
            println!("{:?}", result.err());
        }
    }

    /// Public method to allow other controllers to get artist avatars for
    /// directly if possible.
    /// Without this, they can only get the textures via signals, which have overhead.
    /// To queue downloading artist avatars, simply use ensure_cached_artist_meta, which
    /// will also download artist avatars if the provider is configured to do so.
    pub fn load_cached_artist_avatar(
        &self,
        artist: &ArtistInfo,
        thumbnail: bool,
    ) -> Option<Texture> {
        // First try to get from cache
        let name = &artist.name;
        if let Some(filename) = sqlite::find_image_by_key(name, thumbnail).expect("Sqlite DB error") {
            if let Some(tex) = IMAGE_CACHE.get(&filename) {
                // Cloning GObjects is cheap since they're just references
                return Some(tex.value().clone());
            }
            // If missed, try loading from disk
            let name = name.to_owned();
            let fg_sender = self.fg_sender.clone();
            gio::spawn_blocking(move || {
                let mut path = get_image_cache_path();
                path.push(&filename);
                // Try to load from disk. Do this using the threadpool to avoid blocking UI.
                if path.exists() {
                    if let Ok(tex) = Texture::from_filename(&path) {
                        IMAGE_CACHE.insert(filename, tex.clone(), if thumbnail { 1 } else { 16 });
                        IMAGE_CACHE.wait().unwrap();
                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name));
                    }
                }
            });
        }
        None
    }

    pub fn load_cached_lyrics(&self, song: &SongInfo) -> Option<Lyrics> {
        let result = sqlite::find_lyrics(song);
        if let Ok(res) = result {
            if let Some(info) = res {
                println!("Lyrics cache hit!");
                return Some(info);
            }
            println!("Lyrics cache miss");
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
