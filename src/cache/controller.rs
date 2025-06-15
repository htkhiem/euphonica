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
use base64::{engine::general_purpose::URL_SAFE, Engine as _};
use async_channel::{Receiver, Sender};
use image::io::Reader as ImageReader;
use gio::prelude::*;
use glib::clone;
use gtk::{gdk::{self, Texture}, gio, glib};
use once_cell::sync::Lazy;
use rustc_hash::FxHashSet;
use std::{
    cell::OnceCell,
    fmt,
    fs::create_dir_all,
    path::PathBuf,
    rc::Rc,
    sync::{Arc, RwLock},
};

use crate::{common::SongInfo, meta_providers::{get_provider_with_priority, models::{ArtistMeta, Lyrics}}};
use crate::{
    client::{BackgroundTask, MpdWrapper},
    common::{AlbumInfo, ArtistInfo},
    meta_providers::{
        models, prelude::*, utils::get_best_image, MetadataChain, MetadataType, ProviderMessage,
    },
    utils::{resize_convert_image, settings_manager},
};

use super::{
    CacheState,
    sqlite::LocalMetaDb
};

// In-memory image cache. Declared here to ease usage between threads as Stretto
// is already internally-mutable.
// gdk::Textures are GObjects, which by themselves are boxed reference-counted.
// This means that even if a texture is evicted from this cache, as long as there
// is a widget on screen still using it, it will not actually leave RAM.
// This cache merely holds an additional reference to each texture to keep them
// around when no widget using them are being displayed, so as to reduce disk
// thrashing while quickly scrolling through like a million albums.
// This cache's keys are uri:<folder-level URI>s (for album arts) or artist:<name>
// (for avatars).
fn init_image_cache() -> stretto::Cache<(String, bool), Texture> {
    // TODO: figure out max cost based on user-selectable RAM limit
    // TODO: figure out cost of textures based on user-selectable resolution
    // let setting = settings_manager();
    stretto::Cache::new(327680, 32768).unwrap()
}

static IMAGE_CACHE: Lazy<stretto::Cache<(String, bool), Texture>> =
    Lazy::new(|| init_image_cache());

pub struct Cache {
    app_cache_path: PathBuf,
    albumart_path: PathBuf,
    avatar_path: PathBuf,
    doc_path: PathBuf,
    // Embedded document database for caching responses from metadata providers.
    // Think MongoDB x SQLite x Rust.
    doc_cache: Arc<LocalMetaDb>,
    mpd_client: OnceCell<Rc<MpdWrapper>>,
    fg_sender: Sender<ProviderMessage>, // For receiving notifications from other threads
    bg_sender: Sender<ProviderMessage>,
    meta_providers: Arc<RwLock<MetadataChain>>,
    state: CacheState,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Cache")
            .field("albumart_path", &self.albumart_path)
            .field("avatar_path", &self.avatar_path)
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
    pub fn new(app_cache_path: &PathBuf) -> Rc<Self> {
        let (fg_sender, fg_receiver): (Sender<ProviderMessage>, Receiver<ProviderMessage>) =
            async_channel::bounded(1);
        let (bg_sender, bg_receiver): (Sender<ProviderMessage>, Receiver<ProviderMessage>) =
            async_channel::unbounded();
        let mut albumart_path = app_cache_path.clone();
        albumart_path.push("albumart");
        create_dir_all(&albumart_path).expect("ERROR: cannot create albumart cache folder");

        let mut avatar_path = app_cache_path.clone();
        avatar_path.push("avatar");
        create_dir_all(&avatar_path).expect("ERROR: cannot create albumart cache folder");

        let mut doc_path = app_cache_path.clone();
        doc_path.push("metadata.sqlite");
        let doc_cache = Arc::new(LocalMetaDb::new(&doc_path).expect("Failed to connect to the local metadata cache"));
        let providers = init_meta_provider_chain();

        let cache = Self {
            app_cache_path: app_cache_path.to_path_buf(),
            albumart_path,
            avatar_path,
            doc_path,
            doc_cache,
            meta_providers: Arc::new(RwLock::new(providers)),
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

    pub fn get_app_cache_path(&self) -> &PathBuf {
        &self.app_cache_path
    }

    pub fn get_albumart_path(&self) -> &PathBuf {
        &self.albumart_path
    }

    pub fn get_avatar_path(&self) -> &PathBuf {
        &self.avatar_path
    }

    pub fn get_doc_cache_path(&self) -> &PathBuf {
        &self.doc_path
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
        let doc_cache = self.clone().doc_cache.clone();
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
                                doc_cache,
                                #[strong]
                                providers,
                                move || {
                                    // Check whether there is one already
                                    let folder_uri = key.uri.to_owned();
                                    let existing = doc_cache
                                        .find_album_meta(&key);
                                    if let Ok(None) = existing {
                                        let res = providers.read().unwrap().get_album_meta(&mut key, None);
                                        if let Some(album) = res {
                                            doc_cache.write_album_meta(&key, &album)
                                                .expect("Unable to store downloaded album meta");
                                        }
                                        else {
                                            // Push an empty AlbumMeta to block further calls for this album.
                                            println!("No album meta could be found for {}. Pushing empty document...", &folder_uri);
                                            doc_cache.write_album_meta(&key, &models::AlbumMeta::from_key(&key))
                                                .expect("Unable to store placeholder album meta");
                                        }
                                        let _ = fg_sender.send_blocking(ProviderMessage::AlbumMetaAvailable(folder_uri));
                                        sleep_after_request();
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::ArtistMeta(mut key, path, thumbnail_path) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                doc_cache,
                                #[strong]
                                providers,
                                move || {
                                    // Check whether there is one already
                                    let existing = doc_cache.find_artist_meta(&key);
                                    if let Ok(None) = existing {
                                        // Guaranteed to have this field so just unwrap it
                                        let name = key.name.to_owned();
                                        let res = providers.read().unwrap().get_artist_meta(&mut key, None);
                                        if let Some(artist) = res {
                                            // Try to download artist avatar too
                                            let res = get_best_image(&artist.image);
                                            if res.is_ok() {
                                                let (hires, thumbnail) = resize_convert_image(res.unwrap());
                                                if !path.exists() || !thumbnail_path.exists() {
                                                    if let (Ok(_), Ok(_)) = (
                                                        hires.save(path),
                                                        thumbnail.save(thumbnail_path)
                                                    ) {
                                                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name.clone()));
                                                    }
                                                }
                                            }
                                            else {
                                                println!("[Cache] Failed to download artist avatar: {:?}", res.err());
                                            }
                                            doc_cache.write_artist_meta(&key, &artist)
                                                .expect("Unable to write downloaded artist meta");
                                        }

                                        else {
                                            // Push an empty ArtistMeta to block further calls for this album.
                                            println!("No artist meta could be found for {:?}. Pushing empty document...", &key);
                                            doc_cache.write_artist_meta(&key, &models::ArtistMeta::from_key(&key)).expect("Unable to write downloaded artist meta");
                                        }
                                        let _ = fg_sender.send_blocking(ProviderMessage::ArtistMetaAvailable(name));
                                        sleep_after_request();
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::AlbumArt(key, path, thumbnail_path) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                doc_cache,
                                move || {
                                    if let Ok(Some(meta)) = doc_cache.find_album_meta(&key)
                                    {
                                        let res = get_best_image(&meta.image);
                                            if res.is_ok() {
                                                let (hires, thumbnail) = resize_convert_image(res.unwrap());
                                                if !path.exists() || !thumbnail_path.exists() {
                                                    if let (Ok(_), Ok(_)) = (
                                                        hires.save(&path),
                                                        thumbnail.save(&thumbnail_path)
                                                    ) {
                                                        let _ = fg_sender.send_blocking(ProviderMessage::AlbumArtAvailable(key.uri));
                                                    }
                                                }
                                            }
                                            sleep_after_request();
                                        }
                                    else {
                                        println!("Cannot download album art: no local album meta could be found for {}", key.uri);
                                    }
                                }
                            )).await;
                        },
                        ProviderMessage::Lyrics(key) => {
                            let _ = gio::spawn_blocking(clone!(
                                #[strong]
                                fg_sender,
                                #[strong]
                                doc_cache,
                                #[strong]
                                providers,
                                move || {
                                    // Guaranteed to have this field so just unwrap it
                                    let res = providers.read().unwrap().get_lyrics(&key);
                                    if let Some(lyrics) = res {
                                        doc_cache.write_lyrics(&key, &lyrics)
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
                    ProviderMessage::AlbumArtAvailable(folder_uri) => {
                        this.on_album_art_downloaded(&folder_uri)
                    }
                    ProviderMessage::ClearAlbumArt(folder_uri) => {
                        this.on_album_art_cleared(&folder_uri);
                    }
                    ProviderMessage::AlbumArtNotAvailable(key) => {
                        let folder_uri = &key.uri;
                        println!(
                            "MPD does not have album art for {}, fetching remotely...",
                            folder_uri
                        );
                        // Fill out metadata before attempting to fetch album art from external sources.
                        let _ = this.bg_sender.send_blocking(ProviderMessage::AlbumMeta(
                            key.clone(),
                        ));
                        let path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, false));
                        let thumbnail_path =
                            self.get_path_for(&MetadataType::AlbumArt(&folder_uri, true));
                        let _ = this.bg_sender.send_blocking(ProviderMessage::AlbumArt(
                            key,
                            path,
                            thumbnail_path,
                        ));
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

    fn on_album_art_downloaded(&self, folder_uri: &str) {
        self.state
            .emit_with_param("album-art-downloaded", folder_uri);
    }

    fn on_album_art_cleared(&self, folder_uri: &str) {
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

    pub fn get_path_for(&self, content_type: &MetadataType) -> PathBuf {
        match content_type {
            // Returns the full-resolution path.
            // Do not include filename in URI.
            MetadataType::AlbumArt(folder_uri, thumbnail) => {
                let encoded = URL_SAFE.encode(folder_uri);

                let mut path = self.albumart_path.clone();
                if *thumbnail {
                    path.push(encoded + "_thumb.png");
                } else {
                    path.push(encoded + ".png");
                }
                path
            }
            MetadataType::ArtistAvatar(name, thumbnail) => {
                let hashed = URL_SAFE.encode(name);

                let mut path = self.avatar_path.clone();
                if *thumbnail {
                    path.push(hashed + "_thumb.png");
                } else {
                    path.push(hashed + ".png");
                }
                path
            }
            _ => unreachable!(),
        }
    }

    /// This is a public method to allow other controllers to get cached album arts for
    /// specific songs/albums directly if possible.
    /// Without this, they can only get the textures via signals, which have overhead.
    pub fn load_cached_album_art(
        &self,
        album: &AlbumInfo,
        thumbnail: bool,
        schedule: bool,
    ) -> Option<Texture> {
        let key = (format!("uri:{}", &album.uri), thumbnail);
        if let Some(tex) = IMAGE_CACHE.get(&key) {
            // Cloning GObjects is cheap since they're just references
            return Some(tex.value().clone());
        }
        // If missed, try loading from disk into cache or fetch remotely
        if schedule {
            self.ensure_cached_album_art(album, thumbnail);
        }
        None
    }

    /// Convenience method to check whether album art for a given album is locally available,
    /// and if not, queue its downloading from MPD.
    /// If MPD doesn't have one locally, we'll try fetching from all the enabled metadata providers.
    pub fn ensure_cached_album_art(&self, album: &AlbumInfo, thumbnail: bool) {
        let folder_uri = &album.uri;
        let stretto_key = (format!("uri:{}", &folder_uri), thumbnail);
        if let Some(_) = IMAGE_CACHE.get(&stretto_key) {
            // Already cached. Simply notify UI.
            self.on_album_art_downloaded(&folder_uri);
            return;
        }
        // Not in memory => try loading from disk
        let thumbnail_path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, true));
        let path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, false));
        let bg_sender = self.bg_sender.clone();
        let fg_sender = self.fg_sender.clone();
        let settings = settings_manager().child("client");
        // First, try to load from disk. Do this using the threadpool to avoid blocking UI.
        let path_to_use = if thumbnail { &thumbnail_path } else { &path };
        if path_to_use.exists() {
            let path_to_use = path_to_use.to_owned();
            let folder_uri = folder_uri.to_owned();
            gio::spawn_blocking(move || {
                if let Ok(tex) = Texture::from_filename(path_to_use) {
                    IMAGE_CACHE.insert(
                        stretto_key,
                        tex.clone(),
                        if thumbnail { 1 } else { 16 },
                    );
                    IMAGE_CACHE.wait().unwrap();
                    let _ =
                        fg_sender.send_blocking(ProviderMessage::AlbumArtAvailable(folder_uri));
                }
            });
        }
        // Not on disk either. Try downloading it.
        else if settings.boolean("mpd-download-album-art") {
            self.mpd_client().queue_background(
                BackgroundTask::DownloadAlbumArt(
                    album.clone(),
                    path,
                    thumbnail_path,
                ),
                false,
            );
        }
        // Not allowed to load from MPD. Check external providers
        else {
            // For this we'll need to have album metas ready,
            // so schedule that first.
            let _ = bg_sender.send_blocking(ProviderMessage::AlbumMeta(
                album.clone(),
            ));
            let _ = bg_sender.send_blocking(ProviderMessage::AlbumArt(
                album.clone(),
                path,
                thumbnail_path,
            ));
        }
    }

    /// Load the specified image, resize it, load into cache then send a message to frontend.
    /// All of the above must be done in the background to avoid blocking UI.
    pub fn set_album_art(&self, folder_uri: &str, path: &str) {
        let fg_sender = self.fg_sender.clone();
        let folder_uri = folder_uri.to_owned();
        let hires_path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, false));
        let thumbnail_path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, true));
        // Assume ashpd always return filesystem spec
        let filepath = urlencoding::decode(if path.starts_with("file://") {
            &path[7..]
        } else {
            path
        }).expect("UTF-8").into_owned();
        gio::spawn_blocking(move || {
            let maybe_ptr = ImageReader::open(&filepath);
            if let Ok(ptr) = maybe_ptr {
                if let Ok(dyn_img) = ptr.decode() {
                    let (hires, thumbnail) = resize_convert_image(dyn_img);
                    let _ = hires.save(&hires_path);
                    let _ = thumbnail.save(&thumbnail_path);
                    // TODO: Optimise to avoid reading back from disk
                    IMAGE_CACHE.insert(
                        (format!("uri:{}", &folder_uri), false),
                        gdk::Texture::from_filename(&hires_path).unwrap(),
                        16
                    );
                    IMAGE_CACHE.insert(
                        (format!("uri:{}", &folder_uri), true),
                        gdk::Texture::from_filename(&thumbnail_path).unwrap(),
                        1
                    );
                    IMAGE_CACHE.wait().unwrap();
                    let _ = fg_sender.send_blocking(ProviderMessage::AlbumArtAvailable(folder_uri));
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
    pub fn clear_album_art(&self, folder_uri: &str) {
        let fg_sender = self.fg_sender.clone();
        let folder_uri = folder_uri.to_owned();
        let hires_path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, false));
        let thumbnail_path = self.get_path_for(&MetadataType::AlbumArt(&folder_uri, true));

        gio::spawn_blocking(move || {
            let _ = std::fs::remove_file(hires_path);
            let _ = std::fs::remove_file(thumbnail_path);
            IMAGE_CACHE.remove(&(format!("uri:{}", &folder_uri), false));
            IMAGE_CACHE.remove(&(format!("uri:{}", &folder_uri), true));
            let _ = fg_sender.send_blocking(ProviderMessage::ClearAlbumArt(folder_uri));
        });
    }

    /// Load the specified image, resize it, load into cache then send a message to frontend.
    /// All of the above must be done in the background to avoid blocking UI.
    pub fn set_artist_avatar(&self, tag: &str, path: &str) {
        let fg_sender = self.fg_sender.clone();
        let tag = tag.to_owned();
        let hires_path = self.get_path_for(&MetadataType::ArtistAvatar(&tag, false));
        let thumbnail_path = self.get_path_for(&MetadataType::ArtistAvatar(&tag, true));
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
                    // TODO: Optimise to avoid reading back from disk
                    IMAGE_CACHE.insert(
                        (format!("uri:{}", &tag), false),
                        gdk::Texture::from_filename(&hires_path).unwrap(),
                        16
                    );
                    IMAGE_CACHE.insert(
                        (format!("uri:{}", &tag), true),
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
        let hires_path = self.get_path_for(&MetadataType::ArtistAvatar(&tag, false));
        let thumbnail_path = self.get_path_for(&MetadataType::ArtistAvatar(&tag, true));

        gio::spawn_blocking(move || {
            let _ = std::fs::remove_file(hires_path);
            let _ = std::fs::remove_file(thumbnail_path);
            IMAGE_CACHE.remove(&(format!("uri:{}", &tag), false));
            IMAGE_CACHE.remove(&(format!("uri:{}", &tag), true));
            let _ = fg_sender.send_blocking(ProviderMessage::ClearArtistAvatar(tag));
        });
    }

    // TODO: GUI for downloading album arts from external providers.
    /// Batched version of ensure_cached_album_art.
    /// The list of folder-level URIs will be deduplicated internally to avoid fetching the same
    /// album art multiple times. This is useful for fetching album arts of songs in the queue,
    /// for example.
    /// From MPD, this method only supports downloading thumbnails. Remote sources always provide
    /// both sizes.
    pub fn ensure_cached_album_arts(&self, albums: &[&AlbumInfo]) {
        let mut seen = FxHashSet::default();
        for album in albums.iter() {
            let folder_uri = &album.uri;
            if seen.insert(folder_uri.to_owned()) {
                // println!("ensure_cached_album_arts ({}): calling ensure_cached_album_art", &folder_uri);
                self.ensure_cached_album_art(album, false);
            }
        }
    }

    pub fn load_cached_album_meta(&self, album: &AlbumInfo) -> Option<models::AlbumMeta> {
        // Check whether we have this album cached
        let result = self.doc_cache.find_album_meta(album);
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
        let result = self.doc_cache.find_album_meta(album);
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
        let result = self.doc_cache.find_artist_meta(artist);
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
        let result = self.doc_cache.find_artist_meta(artist);
        if let Ok(response) = result {
            if response.is_none() {
                let path = self.get_path_for(&MetadataType::ArtistAvatar(&artist.name, false));
                let thumbnail_path =
                    self.get_path_for(&MetadataType::ArtistAvatar(&artist.name, true));
                let _ = self.bg_sender.send_blocking(ProviderMessage::ArtistMeta(
                    artist.clone(),
                    path,
                    thumbnail_path,
                ));
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
        let stretto_key = (format!("artist:{}", name), thumbnail);
        if let Some(tex) = IMAGE_CACHE.get(&stretto_key) {
            // Cloning GObjects is cheap since they're just references
            return Some(tex.value().clone());
        }
        // If missed, try loading from disk
        let name = name.to_owned();
        let fg_sender = self.fg_sender.clone();
        let path = self.get_path_for(&MetadataType::ArtistAvatar(&name, thumbnail));
        gio::spawn_blocking(move || {
            // Try to load from disk. Do this using the threadpool to avoid blocking UI.
            if path.exists() {
                if let Ok(tex) = Texture::from_filename(&path) {
                    IMAGE_CACHE.insert(stretto_key, tex.clone(), if thumbnail { 1 } else { 16 });
                    IMAGE_CACHE.wait().unwrap();
                    let _ = fg_sender.send_blocking(ProviderMessage::ArtistAvatarAvailable(name));
                }
            }
        });
        None
    }

    pub fn load_cached_lyrics(&self, song: &SongInfo) -> Option<Lyrics> {
        let result = self.doc_cache.find_lyrics(song);
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
        let result = self.doc_cache.find_lyrics(song);
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
