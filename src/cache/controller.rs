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
use asyncified::Asyncified;
use async_channel::{Receiver, Sender};
use gio::prelude::*;
use glib::clone;
use gtk::{
    gdk::{self, Texture},
    gio, glib,
};
use image::ImageReader;
use lru::LruCache;
use once_cell::sync::Lazy;
use std::{
    cell::OnceCell, fmt, fs::create_dir_all, path::PathBuf, rc::Rc, result, sync::{Arc, RwLock, Mutex}
};
use std::{num::NonZeroUsize};
use uuid::Uuid;

use crate::{
    client::{Error as ClientError, MpdWrapper, Result as ClientResult},
    common::{AlbumInfo, ArtistInfo},
    meta_providers::{MetadataChain, ProviderMessage, models, prelude::*, utils::get_best_image},
    utils::{get_app_cache_path, get_image_cache_path, get_new_image_paths, resize_convert_image, save_and_register_image, settings_manager},
};
use crate::{
    common::{DynamicPlaylist, SongInfo},
    meta_providers::{
        get_provider,
        models::{ArtistMeta, Lyrics},
    },
    utils::strip_filename_linux,
};

use super::{CacheState, sqlite};

#[derive(Debug)]
pub enum Error {
    Io,
    FileNotFound,
    UnknownFileFormat,
    Path,
    PriorFailure,  // Failed to fetch this resource externally once (denoted by empty path in DB table).
    Sqlite(sqlite::Error),
    Client(ClientError)
}

pub type Result<T> = result::Result<T, Error>;

#[derive(Default, Debug, Clone)]
pub enum ImageAction {
    #[default]
    Unknown,
    /// Bool flag indicates whether the current resource (playlist, album, etc)
    /// already has an image or not
    Existing(bool),
    /// Containing disk path to new image file.
    New(String),
    Clear,
}

// TODO: move into common module
#[inline]
fn set_image_internal(
    key: &str,
    key_prefix: Option<&'static str>,
    filepath: &str,
) -> Result<(Texture, Texture)> {
    let dyn_img = ImageReader
        ::open(filepath)
        .map_err(|_| Error::FileNotFound)?
        .decode().map_err(|_| Error::UnknownFileFormat)?;

    let (hires_path, thumbnail_path) = get_new_image_paths();
    let (hires_name, thumbnail_name) = (
        hires_path.file_name().unwrap().to_str().unwrap().to_owned(),
        thumbnail_path.file_name().unwrap().to_str().unwrap().to_owned(),
    );
    let (hires, thumbnail) = resize_convert_image(dyn_img);
    hires.save(&hires_path).map_err(|_| Error::Io)?;
    thumbnail.save(&thumbnail_path).map_err(|_| Error::Io)?;
    sqlite::register_image_key(
        key,
        key_prefix,
        Some(&hires_name),
        false,
    )
        .map_err(Error::Sqlite)?;
    sqlite::register_image_key(
        key,
        key_prefix,
        Some(&thumbnail_name),
        true,
    )
        .map_err(Error::Sqlite)?;
    // TODO: Optimise to avoid reading back from disk
    let hires_tex = gdk::Texture::from_filename(&hires_path).map_err(|_| Error::FileNotFound)?;
    let thumbnail_tex = gdk::Texture::from_filename(&thumbnail_path).map_err(|_| Error::FileNotFound)?;
    {
        let mut cache = IMAGE_CACHE.lock().unwrap();
        cache.put(
            hires_name,
            hires_tex.clone(),
        );
        cache.put(
            thumbnail_name,
            thumbnail_tex.clone(),
        );
    }
    Ok((hires_tex, thumbnail_tex))
}

#[inline]
fn clear_image_internal(key: &str, key_prefix: Option<&'static str>) -> Result<bool> {
    let mut removed: bool = false;
    if let Some(hires_name) = sqlite::find_image_by_key(key, key_prefix, false).map_err(Error::Sqlite)? {
        let mut hires_path = get_image_cache_path();
        hires_path.push(&hires_name);
        sqlite::unregister_image_key(key, key_prefix, false)
            .map_err(Error::Sqlite)?;
        IMAGE_CACHE.lock().unwrap().pop(&hires_name);
        std::fs::remove_file(hires_path).map_err(|_| Error::Io)?;
        removed = true;
    }
    if let Some(thumb_name) = sqlite::find_image_by_key(key, key_prefix, true).map_err(Error::Sqlite)? {
        let mut thumb_path = get_image_cache_path();
        thumb_path.push(&thumb_name);
        sqlite::unregister_image_key(key, key_prefix, true)
            .map_err(Error::Sqlite)?;
        IMAGE_CACHE.lock().unwrap().pop(&thumb_name);
        std::fs::remove_file(thumb_path).map_err(|_| Error::Io)?;
        removed = true;
    }
    Ok(removed)
}

#[inline]
fn get_image_internal(key: &str, prefix: Option<&'static str>, thumbnail: bool) -> Result<Option<gdk::Texture>> {
    if let Some(filename) =
        sqlite::find_image_by_key(key, prefix, thumbnail).map_err(Error::Sqlite)?
    {
        if !filename.is_empty() {
            let tex;
            {
                // Cloning GObjects is cheap since they're just references
                tex = IMAGE_CACHE.lock().unwrap().get(&filename).map(|tex| tex.clone());
            }
            if tex.is_some() {
                Ok(tex)
            } else {
                let mut cover_path = get_image_cache_path();
                cover_path.push(&filename);
                match Texture::from_filename(&cover_path) {
                    Ok(tex) => {
                        Ok(Some(tex))
                    }
                    Err(_) => {
                        // File no longer exists (maybe user had removed it). Unregister it from DB.
                        sqlite::unregister_image_key(key, prefix, thumbnail).map_err(Error::Sqlite)?;
                        println!("Unregistered image. Retrying...");
                        // Return song info object to facilitate recursive retry
                        Err(Error::FileNotFound)
                    }
                }
            }
        } else {
            // There is an entry, but it's an empty string. This is our indication that we've
            // failed to fetch the embedded art from MPD once before, so don't try again
            // (will not succeed unless the files have been re-tagged).
            Err(Error::PriorFailure)
        }
    } else {
        Ok(None)
    }
}

#[inline]
fn read_texture(path: &str) -> Result<gdk::Texture> {
    gdk::Texture::from_filename(path)
        .map_err(|_| Error::FileNotFound)
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

// We use an Asyncified container to queue tasks, such that two requests for the
// same texture are never run concurrently. This allows one request to cache the
// texture in-memory for all subsequent requests.
pub struct Cache {
    mpd_client: Rc<MpdWrapper>,
    meta_providers: Arc<Mutex<MetadataChain>>,
    // Asyncified container for local operations (no delay between calls).
    local: Asyncified<()>,
    // Asyncified container for operations involving API calls (should sleep after each call).
    external: Asyncified<()>,
    state: CacheState,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Cache").finish()
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
    pub fn new(mpd_client: Rc<MpdWrapper>) -> Rc<Self> {
        // Init folders
        create_dir_all(get_app_cache_path()).expect("ERROR: cannot create cache folders");
        create_dir_all(get_image_cache_path()).expect("ERROR: cannot create cache folders");

        let cache = Self {
            // TODO: Turn mpd_client here into a metadata provider too.
            mpd_client,
            meta_providers: Arc::new(Mutex::new(init_meta_provider_chain())),
            local: glib::MainContext::default().block_on(
                glib::spawn_future_local(async move {
                    Asyncified::builder().channel_size(128).build_ok(|| ()).await
                })
            ).unwrap(),
            external: glib::MainContext::default().block_on(
                glib::spawn_future_local(async move {
                    Asyncified::builder().channel_size(32768).build_ok(|| ()).await
                })
            ).unwrap(),
            state: CacheState::default(),
        };
        let res = Rc::new(cache);

        res
    }
    /// Re-initialise list of providers when priority order is changed
    pub fn reinit_meta_providers(&self) {
        let mut curr_providers = self.meta_providers.lock().unwrap();
        *curr_providers = init_meta_provider_chain();
    }

    pub fn get_cache_state(&self) -> CacheState {
        self.state.clone()
    }

    async fn read_texture_async(&self, path: String) -> Result<Texture> {
        self.local.call(move |_| {
            read_texture(&path)
        }).await
    }

    /// Try to get a cover image for the given song. This prioritises the embedded cover
    /// over the cover image file in the same folder.
    /// Returns a gdk::Texture from cache if available.
    /// Failing that, we'll search local storage.
    /// Still failing that, if `external` is true, it will try to get one from MPD.
    pub async fn get_song_cover(
        self: Rc<Self>,
        song: &SongInfo,
        thumbnail: bool,
        external: bool,
    ) -> Result<Option<Texture>> {
        let mut embedded_failed_before = false;
        let mut folder_failed_before = false;
        let uri = song.uri.to_owned();
        // Try to get embedded cover from in-memory cache or local storage first.
        match self.local.call(move |_| get_image_internal(&uri, None, thumbnail)).await {
            Ok(Some(tex)) => {
                return Ok(Some(tex));
            }
            Ok(None) => {}
            Err(Error::PriorFailure) => {
                embedded_failed_before = true;
            }
            Err(Error::FileNotFound) => {
                // Retry (DB entry should have been purged by get_image_internal)
                return Box::pin(self.get_song_cover(song, thumbnail, external)).await;
            }
            Err(e) => {
                return Err(e);
            }
        }

        // Now try to get a folder cover, again from in-memory cache or local storage.
        let folder_uri = strip_filename_linux(&song.uri).to_owned();
        match self.local.call(move |_| get_image_internal(&folder_uri, None, thumbnail)).await {
            Ok(Some(tex)) => {
                return Ok(Some(tex));
            }
            Ok(None) => {}
            Err(Error::PriorFailure) => {
                folder_failed_before = true;
            }
            Err(Error::FileNotFound) => {
                // Retry (DB entry should have been purged by get_image_internal)
                return Box::pin(self.get_song_cover(song, thumbnail, external)).await;
            }
            Err(e) => {
                return Err(e);
            }
        }

        if external {
            if !embedded_failed_before && settings_manager().child("client").boolean("mpd-download-album-art") {
                if let Some((hires_path, thumb_path)) = self.mpd_client.get_embedded_cover(song.uri.clone()).map_err(Error::Client).await?
                {
                    return Ok(Some(self.read_texture_async(if thumbnail {thumb_path} else {hires_path}).await?));
                }
            }
            if let (false, Some(album)) = (
                folder_failed_before,
                song.album.as_ref().cloned()
            ) {
                if let Some(meta) = self.get_album_meta(&album, true, false).await? {
                    return self.external.call(move |_| {
                        // Always check with our DB first as a prior call might have downloaded the
                        // necessary image for us.
                        let hires = sqlite::find_image_by_key(&album.folder_uri, None, false).expect("Sqlite DB error");
                        let thumb = sqlite::find_image_by_key(&album.folder_uri, None, true).expect("Sqlite DB error");
                        let tex = if let (Some(hires_path), Some(thumb_path)) = (hires, thumb) {
                            Some(read_texture(if thumbnail {&thumb_path} else {&hires_path})?)
                        } else {
                            match get_best_image(&meta.image) {
                                Ok(dyn_img) => {
                                    let (hires_path, thumb_path) = save_and_register_image(Some(dyn_img), &album.folder_uri, None).unwrap();
                                    Some(read_texture(if thumbnail {&thumb_path} else {&hires_path})?)
                                }
                                Err(e) => {
                                    dbg!(e);
                                    let _ = save_and_register_image(None, &album.folder_uri, None);
                                    None
                                }
                            }
                        };
                        Ok(tex)
                    }).await;
                }
            }
        }
        Ok(None)
    }

    /// Try to get a cover image for the given album. This prioritises the folder image
    /// file over embedded covers of its tracks.
    /// Returns a gdk::Texture from cache if available.
    /// Failing that, we'll search local storage.
    /// Still failing that, if `external` is true, it will try to get one from MPD.
    pub async fn get_album_cover(
        self: Rc<Self>,
        album: &AlbumInfo,
        thumbnail: bool,
        external: bool,
    ) -> Result<Option<Texture>> {
        let mut embedded_failed_before = false;
        let mut folder_failed_before = false;

        // Try to find the image file first.
        let folder_uri = album.folder_uri.to_owned();
        match self.local.call(move |_| get_image_internal(&folder_uri, None, thumbnail)).await {
            Ok(Some(tex)) => {
                return Ok(Some(tex));
            }
            Ok(None) => {}
            Err(Error::PriorFailure) => {
                folder_failed_before = true;
            }
            Err(Error::FileNotFound) => {
                // Retry (DB entry should have been purged by get_image_internal)
                return Box::pin(self.get_album_cover(album, thumbnail, external)).await;
            }
            Err(e) => {
                return Err(e);
            }
        }

        // Now try to get the embedded art from one of its tracks.
        let example_uri = album.example_uri.to_owned();
        match self.local.call(move |_| get_image_internal(&example_uri, None, thumbnail)).await {
            Ok(Some(tex)) => {
                return Ok(Some(tex));
            }
            Ok(None) => {}
            Err(Error::PriorFailure) => {
                embedded_failed_before = true;
            }
            Err(Error::FileNotFound) => {
                // Retry (DB entry should have been purged by get_image_internal)
                return Box::pin(self.get_album_cover(album, thumbnail, external)).await;
            }
            Err(e) => {
                return Err(e);
            }
        }

        if external {
            if !folder_failed_before && settings_manager().child("client").boolean("mpd-download-album-art") {
                if let Some((hires_path, thumb_path)) = self.mpd_client.get_folder_cover(album.folder_uri.to_owned()).map_err(Error::Client).await?
                {
                    let path = if thumbnail {thumb_path} else {hires_path};
                    let tex = self.local.call(move |_| {
                        gdk::Texture::from_filename(path)
                    }).await.map_err(|_| Error::FileNotFound)?;
                    return Ok(Some(tex));
                }
            }

            if !embedded_failed_before && settings_manager().child("client").boolean("mpd-download-album-art") {
                if let Some((hires_path, thumb_path)) = self.mpd_client.get_embedded_cover(album.example_uri.to_owned()).map_err(Error::Client).await?
                {
                    let path = if thumbnail {thumb_path} else {hires_path};
                    let tex = self.local.call(move |_| {
                        gdk::Texture::from_filename(path)
                    }).await.map_err(|_| Error::FileNotFound)?;
                    return Ok(Some(tex));
                }
            }
            if let (false, Some(meta)) = (
                folder_failed_before,
                self.get_album_meta(&album, true, false).await?
            ) {
                let album = album.to_owned();
                return self.external.call(move |_| {
                    // Always check with our DB first as a prior call might have downloaded the
                    // necessary image for us.
                    let hires = sqlite::find_image_by_key(&album.folder_uri, None, false).expect("Sqlite DB error");
                    let thumb = sqlite::find_image_by_key(&album.folder_uri, None, true).expect("Sqlite DB error");
                    let tex = if let (Some(hires_path), Some(thumb_path)) = (hires, thumb) {
                        Some(read_texture(if thumbnail {&thumb_path} else {&hires_path})?)
                    } else {
                        match get_best_image(&meta.image) {
                            Ok(dyn_img) => {
                                let (hires_path, thumb_path) = save_and_register_image(Some(dyn_img), &album.folder_uri, None).unwrap();
                                Some(read_texture(if thumbnail {&thumb_path} else {&hires_path})?)
                            }
                            Err(e) => {
                                dbg!(e);
                                let _ = save_and_register_image(None, &album.folder_uri, None);
                                None
                            }
                        }
                    };
                    Ok(tex)
                }).await;
            }
        }

        Ok(None)
    }

    /// Load the specified image, resize it, load into cache then send a message to frontend.
    async fn set_image(
        &self,
        key: String,
        key_prefix: Option<&'static str>,
        path: &str,
        notify_signal: Option<&'static str>,
    ) -> Result<(gdk::Texture, gdk::Texture)> {
        // Assume ashpd always return filesystem spec
        let filepath = String::from(urlencoding::decode(if path.starts_with("file://") {
            &path[7..]
        } else {
            &path
        })
        .map_err(|_| Error::Path)?);
        let state = self.get_cache_state();
        self.local.call(move |_| {
            let (hires, thumb) = set_image_internal(&key, key_prefix, &filepath)?;
            // For updates, still notify via signals to update all widgets wherever they are.
            if let Some(signal) = notify_signal {
                state.emit_texture(signal, &key, &hires, &thumb);
            }
            Ok((hires, thumb))
        }).await
    }

    /// Evict the image from cache and delete from cache folder on disk.
    /// This does not by itself yeet the image from memory (UI elements will still hold refs to it).
    /// We'll need to signal to these elements to clear themselves.
    pub async fn clear_image(
        &self,
        key: String,
        key_prefix: Option<&'static str>,
        notify_signal: Option<&'static str>,
    ) -> Result<()> {
        // Assume ashpd always return filesystem spec
        let state = self.get_cache_state();
        self.local.call(move |_| {
            clear_image_internal(&key, key_prefix)?;
            // For updates, still notify via signals to update all widgets wherever they are.
            if let Some(signal) = notify_signal {
                state.emit_with_param(signal, &key);
            }
            Ok(())
        }).await
    }

    pub async fn set_cover(&self, folder_uri: String, path: &str) -> Result<(gdk::Texture, gdk::Texture)> {
        self.set_image(folder_uri, None, path, Some("album-art-set")).await
    }

    pub async fn clear_cover(&self, folder_uri: String) -> Result<()> {
        self.clear_image(folder_uri, None, Some("album-art-cleared")).await
    }

    pub async fn set_artist_avatar(&self, tag: String, path: &str) -> Result<(gdk::Texture, gdk::Texture)> {
        self.set_image(tag, Some("avatar"), path, Some("artist-avatar-set")).await
    }

    pub async fn clear_artist_avatar(&self, tag: String) -> Result<()> {
        self.clear_image(tag, Some("avatar"), Some("artist-avatar-cleared")).await
    }

    pub async fn set_playlist_cover(&self, playlist_name: String, path: &str) -> Result<(gdk::Texture, gdk::Texture)> {
        self.set_image(playlist_name, Some("playlist"), path, None).await
    }

    pub async fn clear_playlist_cover(&self, playlist_name: String) -> Result<()> {
        self.clear_image(playlist_name, Some("playlist"), None).await
    }

    pub async fn get_album_meta(&self, album: &AlbumInfo, external: bool, overwrite: bool) -> Result<Option<models::AlbumMeta>> {
        if !(overwrite && external) {
            // Check whether we have this album cached
            let title = album.title.to_owned();
            let mbid = album.mbid.clone();
            let artist = album.get_artist_tag().map(String::from);

            let local = self.local.call(move |_| {
                sqlite::find_album_meta(&title, mbid.as_deref(), artist.as_deref())
            }).await.map_err(Error::Sqlite)?;

            if local.is_some() {
                return Ok(local)
            }
        }

        if external && (album.mbid.is_some() || album.albumartist.is_some()) {
            let mut album = album.to_owned();
            let providers = self.meta_providers.clone();
            return self.external.call(move |_| {
                if !overwrite {
                    if let  Some(existing) = sqlite::find_album_meta(
                        &album.title, album.mbid.as_deref(), album.albumartist.as_deref()
                    ).map_err(Error::Sqlite)? {
                        return Ok(Some(existing));
                    }
                }
                let res = providers.lock().unwrap().get_album_meta(&mut album, None);
                if let Some(meta) = res {
                    sqlite::write_album_meta(&album, &meta).map_err(Error::Sqlite)?;
                    Ok(Some(meta))
                }
                else {
                    // Push an empty AlbumMeta to block further calls for this album.
                    println!("No album meta could be found for {}. Pushing empty document...", &album.folder_uri);
                    sqlite::write_album_meta(&album, &models::AlbumMeta::from_key(&album)).map_err(Error::Sqlite)?;
                    Ok(None)
                }
            }).await;
        }

        Ok(None)
    }

    pub async fn get_artist_meta(&self, artist: &ArtistInfo, external: bool, overwrite: bool) -> Result<Option<ArtistMeta>> {
        let name = artist.name.to_owned();
        let mbid = artist.mbid.clone();

        let local = self.local.call(move |_| {
            sqlite::find_artist_meta(&name, mbid.as_deref())
        }).await.map_err(Error::Sqlite)?;

        if local.is_some() {
            return Ok(local)
        }

        if external && (overwrite || local.is_none()) {
            let mut artist = artist.to_owned();
            let providers = self.meta_providers.clone();
            return self.external.call(move |_| {
                if !overwrite {
                    if let  Some(existing) = sqlite::find_artist_meta(
                        &artist.name, artist.mbid.as_deref()
                    ).map_err(Error::Sqlite)? {
                        return Ok(Some(existing));
                    }
                }
                let res = providers.lock().unwrap().get_artist_meta(&mut artist, None);
                if let Some(meta) = res {
                    sqlite::write_artist_meta(&artist, &meta).map_err(Error::Sqlite)?;
                    Ok(Some(meta))
                }
                else {
                    // Push an empty ArtistMeta to block further calls for this artist.
                    println!("No artist meta could be found for {}. Pushing empty document...", &artist.name);
                    sqlite::write_artist_meta(&artist, &models::ArtistMeta::from_key(&artist)).map_err(Error::Sqlite)?;
                    Ok(None)
                }
            }).await;
        }

        Ok(None)
    }

    /// Try to get an avatar for the given artist.
    /// Returns a gdk::Texture from cache if available.
    /// Failing that, we'll search local storage.
    /// Still failing that, if `external` is true, it will try to get one from external providers.
    pub async fn get_artist_avatar(
        self: Rc<Self>,
        artist: &ArtistInfo,
        thumbnail: bool,
        external: bool
    ) -> Result<Option<Texture>> {
        // First try to get from cache, then from local storage
        let name = artist.name.to_owned();
        let mut failed_before = false;
        match self.local.call(move |_| get_image_internal(&name, Some("avatar"), thumbnail)).await {
            Ok(Some(tex)) => {
                return Ok(Some(tex));
            }
            Ok(None) => {}
            Err(Error::PriorFailure) => {
                failed_before = true;
            }
            Err(e) => {
                return Err(e);
            }
        }

        // Failing the above, ask external providers
        if external && !failed_before {
            if let Some(meta) = self.get_artist_meta(&artist, true, false).await? {
                let artist = artist.to_owned();
                return self.external.call(move |_| {
                    // Always check with our DB first as a prior call might have downloaded the
                    // necessary image for us.
                    let hires = sqlite::find_image_by_key(&artist.name, Some("avatar"), false).expect("Sqlite DB error");
                    let thumb = sqlite::find_image_by_key(&artist.name, Some("avatar"), true).expect("Sqlite DB error");
                    let tex = if let (Some(hires_path), Some(thumb_path)) = (hires, thumb) {
                        Some(read_texture(if thumbnail {&thumb_path} else {&hires_path})?)
                    } else {
                        match get_best_image(&meta.image) {
                            Ok(dyn_img) => {
                                let (hires_path, thumb_path) = save_and_register_image(Some(dyn_img), &artist.name, Some("avatar")).unwrap();
                                Some(read_texture(if thumbnail {&thumb_path} else {&hires_path})?)
                            }
                            Err(e) => {
                                dbg!(e);
                                let _ = save_and_register_image(None, &artist.name, Some("avatar"));
                                None
                            }
                        }
                    };
                    Ok(tex)
                }).await;
            }
        }

        Ok(None)
    }

    pub async fn get_playlist_cover(
        &self,
        playlist_name: String,
        is_dynamic_playlist: bool,
        thumbnail: bool,
    ) -> Result<Option<gdk::Texture>> {
        self.local.call(move |_| {
            let prefix = Some(if is_dynamic_playlist {
                "dynamic_playlist"
            } else {
                "playlist"
            });
            get_image_internal(&playlist_name, prefix, thumbnail)
        }).await
    }

    pub async fn insert_dynamic_playlist(
        &self,
        dp: DynamicPlaylist,
        cover_action: ImageAction,
        overwrite_name: Option<String>,
    ) -> Result<()> {
        self.local.call(move |_| {
            // If updating an existing DP, use old name first. SQLite code will migrate it for us.
            let should_overwrite = overwrite_name.is_some();
            let current_cover_key = overwrite_name
                .unwrap_or_else(|| dp.name.to_owned());
            match cover_action {
                ImageAction::Clear => {
                    clear_image_internal(&current_cover_key, Some("dynamic_playlist"))?;
                }
                ImageAction::New(path) => {
                    set_image_internal(&current_cover_key, Some("dynamic_playlist"), &path)?;
                }
                _ => {}
            };

            sqlite::insert_dynamic_playlist(
                &dp,
                if should_overwrite { Some(&current_cover_key) } else { None }
            ).map_err(Error::Sqlite)
        }).await
    }

    pub async fn get_lyrics(&self, song: &SongInfo, external: bool) -> Result<Option<Lyrics>> {
        let uri = song.uri.to_owned();
        let local = self.local.call(move |_| {
            sqlite::find_lyrics(&uri)
        }).await.map_err(Error::Sqlite)?;
        if local.is_some() {
            return Ok(local);
        }

        if external {
            let providers = self.meta_providers.clone();
            let song = song.to_owned();
            return self.external.call(move |_| {
                let res = providers.lock().unwrap().get_lyrics(&song);
                if let Some(lyrics) = res {
                    sqlite::write_lyrics(&song, Some(&lyrics)).map_err(Error::Sqlite)?;
                    return Ok(Some(lyrics));
                }
                Ok(None)
            }).await;
        }
        Ok(None)
    }
}
