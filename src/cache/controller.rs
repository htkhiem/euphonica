// Cache system to store album arts, artist avatars, wikis, bios,
// you name it.
// This helps avoid having to query the same thing multiple times,
// whether from MPD or from Last.fm.
// - Images are stored as resized PNG files on disk.
// - Text data is stored as BSON blobs in SQLite.
use futures::TryFutureExt;
extern crate bson;
use asyncified::Asyncified;
use gio::prelude::*;
use gtk::{
    gdk::{self, Texture},
    gio, glib,
};
use image::ImageReader;
use lru::LruCache;
use once_cell::sync::Lazy;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{fmt, fs::create_dir_all, rc::Rc, result, sync::Mutex};

use crate::{
    client::{Error as ClientError, MpdWrapper},
    common::{AlbumInfo, ArtistInfo},
    meta_providers::{MetadataChain, models, prelude::*, utils::get_best_image},
    utils::{
        get_app_cache_path, get_image_cache_path, register_image_as_failure,
        save_and_register_image, settings_manager,
    },
    window::EuphonicaWindow,
};
use crate::{
    common::{DynamicPlaylist, SongInfo},
    meta_providers::models::{ArtistMeta, Lyrics},
    utils::strip_filename_linux,
};

use super::{CacheState, sqlite};

#[derive(Debug)]
pub enum Error {
    Download(String),
    Io,
    FileNotFound,
    UnknownFileFormat,
    Path,
    PriorFailure, // Failed to fetch this resource externally once (denoted by empty path in DB table).
    Sqlite(sqlite::Error),
    Client(ClientError),
    Metadata(MetadataError<()>),
    GlibError(glib::Error),
}

impl Error {
    pub fn message(&self) -> String {
        match self {
            Self::Download(msg) => msg.to_owned(),
            Self::Io => "I/O error".into(),
            Self::FileNotFound => "file not found".into(),
            Self::UnknownFileFormat => "unknown file format".into(),
            Self::Path => "invalid path".into(),
            Self::PriorFailure => "failed before".into(), // Shouldn't show this to UI
            Self::Sqlite(_) => "SQLite error".into(),     // TODO: better error message
            Self::Client(_) => "MPD error".into(),        // TODO: better error message
            Self::Metadata(e) => e.message(),
            Self::GlibError(e) => e.to_string(),
        }
    }
}

impl From<glib::Error> for Error {
    fn from(value: glib::Error) -> Self {
        Error::GlibError(value)
    }
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
    let dyn_img = ImageReader::open(filepath)
        .map_err(|_| Error::FileNotFound)?
        .decode()
        .map_err(|_| Error::UnknownFileFormat)?;

    let bundle = save_and_register_image(dyn_img, key, key_prefix);
    let hires_tex = bundle.hires.take_texture()?;
    let thumb_tex = bundle.thumb.take_texture()?;

    {
        let mut cache = IMAGE_CACHE.lock().unwrap();

        cache.put(bundle.hires.name, hires_tex.clone());
        cache.put(bundle.thumb.name, thumb_tex.clone());
    }
    Ok((hires_tex, thumb_tex))
}

#[inline]
fn clear_image_internal(key: &str, key_prefix: Option<&'static str>) -> Result<bool> {
    let mut removed: bool = false;
    if let Some(hires_name) =
        sqlite::find_image_by_key(key, key_prefix, false).map_err(Error::Sqlite)?
    {
        let mut hires_path = get_image_cache_path();
        hires_path.push(&hires_name);
        sqlite::unregister_image_key(key, key_prefix, false).map_err(Error::Sqlite)?;
        IMAGE_CACHE.lock().unwrap().pop(&hires_name);
        removed = std::fs::remove_file(hires_path).is_ok();
    }
    if let Some(thumb_name) =
        sqlite::find_image_by_key(key, key_prefix, true).map_err(Error::Sqlite)?
    {
        let mut thumb_path = get_image_cache_path();
        thumb_path.push(&thumb_name);
        sqlite::unregister_image_key(key, key_prefix, true).map_err(Error::Sqlite)?;
        IMAGE_CACHE.lock().unwrap().pop(&thumb_name);
        removed = std::fs::remove_file(thumb_path)
            .map_err(|_| Error::Io)
            .is_ok();
    }
    Ok(removed)
}

#[inline]
fn get_image_internal(
    key: &str,
    prefix: Option<&'static str>,
    thumbnail: bool,
) -> Result<Option<gdk::Texture>> {
    if let Some(filename) =
        sqlite::find_image_by_key(key, prefix, thumbnail).map_err(Error::Sqlite)?
    {
        if !filename.is_empty() {
            let tex;
            {
                // Cloning GObjects is cheap since they're just references
                tex = IMAGE_CACHE.lock().unwrap().get(&filename).cloned();
            }
            if tex.is_some() {
                Ok(tex)
            } else {
                let mut cover_path = get_image_cache_path();
                cover_path.push(&filename);
                match Texture::from_filename(&cover_path) {
                    Ok(tex) => Ok(Some(tex)),
                    Err(_) => {
                        // File no longer exists (maybe user had removed it). Unregister it from DB.
                        sqlite::unregister_image_key(key, prefix, thumbnail)
                            .map_err(Error::Sqlite)?;
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
fn read_texture_from_name(name: &str) -> Result<gdk::Texture> {
    let mut res = get_image_cache_path();
    res.push(name);

    gdk::Texture::from_filename(res).map_err(|e| {
        dbg!(e);
        Error::FileNotFound
    })
}

#[inline]
fn download_image_from_provider(
    key: &str,
    prefix: Option<&'static str>,
    fallback_images: &[models::ImageMeta],
    thumbnail: bool,
) -> Result<Option<Texture>> {
    // Always check with our DB first as a prior call might have downloaded the
    // necessary image for us.
    if let Some(file) = sqlite::find_image_by_key(key, prefix, thumbnail).expect("Sqlite DB error")
    {
        read_texture_from_name(&file).map(Some)
    } else {
        match get_best_image(fallback_images) {
            Ok(dyn_img) => {
                let bundle = save_and_register_image(dyn_img, key, prefix);
                Ok(bundle.take_texture(thumbnail).map(Some)?)
            }
            Err(e) => {
                dbg!(e);
                register_image_as_failure(key, prefix);
                Ok(None)
            }
        }
    }
}

// In-memory image cache.
// gdk::Textures are GObjects, which by themselves are boxed reference-counted.
// This means that even if a texture is evicted from this cache, as long as there
// is a widget on screen still using it, it will not actually leave RAM.
// This cache merely holds an additional reference to each texture to keep them
// around when no widget using them are being displayed, so as to reduce disk
// thrashing while quickly scrolling through like a million albums.
// This cache's keys are the filenames themselves.

// Keeping ~1024 textures around doesn't mean there can only be 1024 on screen at any time.
// This cache merely keeps an additional strong ref alive in case a texture goes out of view.
// As long as one is in view it is held by the widget displaying it.
static IMAGE_CACHE: Lazy<Mutex<LruCache<String, Texture>>> =
    Lazy::new(|| Mutex::new(LruCache::new(NonZeroUsize::new(1024).unwrap())));

/// Threshold for deferring hires album art loading. When the pending task count
/// reaches this value, new album art requests fall back to thumbnails.
pub static BACKLOG_THRESHOLD: Lazy<usize> = Lazy::new(|| {
    settings_manager()
        .child("library")
        .uint("use-hires-backlog-threshold")
        .try_into()
        .unwrap_or(10)
});

// We use an Asyncified container to queue tasks, such that two requests for the
// same texture are never run concurrently. This allows one request to cache the
// texture in-memory for all subsequent requests.
pub struct Cache {
    mpd_client: Rc<MpdWrapper>,
    meta_providers: MetadataChain,
    // Asyncified container for local operations (no delay between calls). Serves as a task queue.
    // Background tasks that shouldn't be parallelised, such as image downloads, should be run there.
    // This allows us to cleanly avoid duplicate downloads.
    local: Asyncified<()>,
    // Same as above but for operations involving API calls (should sleep after each call).
    external: Asyncified<()>,
    // Thread pool for parallelisable operations such as texture read from disk and resizing ops.
    pool: glib::ThreadPool,
    // Cache state object for emitting signals.
    state: CacheState,
    // Tracks in-flight get_cover requests for backpressure-aware hires loading.
    pending_tasks: AtomicUsize,
}

impl fmt::Debug for Cache {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Cache").finish()
    }
}

impl Cache {
    pub fn new(mpd_client: Rc<MpdWrapper>) -> Rc<Self> {
        // Init folders
        create_dir_all(get_app_cache_path()).expect("ERROR: cannot create cache folders");
        create_dir_all(get_image_cache_path()).expect("ERROR: cannot create cache folders");

        let cache = Self {
            // TODO: Turn mpd_client here into a metadata provider too.
            mpd_client,
            meta_providers: glib::MainContext::default()
                .block_on(glib::spawn_future_local(async move {
                    MetadataChain::new().await
                }))
                .unwrap(),
            local: glib::MainContext::default()
                .block_on(glib::spawn_future_local(async move {
                    Asyncified::builder()
                        .channel_size(usize::MAX)
                        .build_ok(|| ())
                        .await
                }))
                .unwrap(),
            external: glib::MainContext::default()
                .block_on(glib::spawn_future_local(async move {
                    Asyncified::builder()
                        .channel_size(usize::MAX)
                        .build_ok(|| ())
                        .await
                }))
                .unwrap(),
            pool: glib::ThreadPool::shared(Some(
                settings_manager()
                    .child("library")
                    .uint("n-image-threads"),
            ))
            .expect("Unable to start threadpool for cache operations"),
            state: CacheState::default(),
            pending_tasks: AtomicUsize::new(0),
        };

        Rc::new(cache)
    }

    pub fn get_cache_state(&self) -> CacheState {
        self.state.clone()
    }

    /// Returns the number of pending cover lookup tasks.
    pub fn backlog(&self) -> usize {
        self.pending_tasks.load(Ordering::Relaxed)
    }

    /// Try to get a cover image for the given song. This prioritises the folder-level
    /// cover image over embedded covers of the track.
    /// Returns a gdk::Texture from cache if available.
    /// Failing that, we'll search local storage.
    /// Still failing that, it will try to get one from MPD or external metadata providers.
    pub async fn get_song_cover(
        self: Rc<Self>,
        song: &SongInfo,
        thumbnail: bool,
    ) -> Result<Option<Texture>> {
        let folder_uri = strip_filename_linux(&song.uri).to_owned();
        let album = song.album.as_ref().cloned();
        self.get_cover(&folder_uri, &song.uri, thumbnail, album)
            .await
    }

    /// Try to get a cover image for the given album. This prioritises the folder image
    /// file over embedded covers of its tracks.
    /// Returns a gdk::Texture from cache if available.
    /// Failing that, we'll search local storage.
    /// Still failing that, it will try to get one from MPD or external metadata providers.
    pub async fn get_album_cover(
        self: Rc<Self>,
        album: &AlbumInfo,
        thumbnail: bool,
    ) -> Result<Option<Texture>> {
        self.get_cover(
            &album.folder_uri,
            &album.example_uri,
            thumbnail,
            Some(album.to_owned()),
        )
        .await
    }

    #[inline]
    async fn get_cover_internal(
        self: Rc<Self>,
        folder_key: &str,
        embedded_key: &str,
        thumbnail: bool,
        album: Option<AlbumInfo>,
    ) -> Result<Option<Texture>> {
        let mut failed_before = false;

        let folder_key_owned = folder_key.to_owned();
        let embedded_key_owned = embedded_key.to_owned();

        // 1. Check if we have it cached locally. This is parallelisable so we'll use the threadpool.
        let folder_key_cached = folder_key_owned.clone();
        match self
            .pool
            .push_future(move || get_image_internal(&folder_key_cached, None, thumbnail))
            .expect("get_cover_internal: cache threadpool error")
            .await
            .expect("get_cover_internal: cache threadpool error")
        {
            Ok(Some(tex)) => return Ok(Some(tex)),
            Ok(None) => {}
            Err(Error::PriorFailure) => failed_before = true,
            Err(Error::FileNotFound) => {
                // Retry (DB entry should have been purged by get_image_internal)
                // No need to increment/decrement pending_tasks here.
                return Box::pin(self.get_cover_internal(
                    &folder_key_owned,
                    &embedded_key_owned,
                    thumbnail,
                    album,
                ))
                .await;
            }
            Err(e) => return Err(e),
        }

        // 2. Nope, fall back to downloading fresh
        if !failed_before {
            if settings_manager()
                .child("client")
                .boolean("mpd-download-album-art")
            {
                // 2a. MPD folder-level cover
                if let Some(bundle) = self
                    .mpd_client
                    .get_folder_cover(embedded_key_owned.to_owned())
                    .map_err(Error::Client)
                    .await?
                {
                    return Ok(bundle.take_texture(thumbnail).map(Some)?);
                }
                // 2b. MPD embedded cover, WILL BE SAVED AS FOLDER-LEVEL ART such that all future calls from tracks in the
                // same folder will point to this one. This way we avoid downloading the same embedded art from each track.
                if let Some(bundle) = self
                    .mpd_client
                    .get_embedded_cover(embedded_key_owned.to_owned())
                    .map_err(Error::Client)
                    .await?
                {
                    return Ok(bundle.take_texture(thumbnail).map(Some)?);
                }
            }

            // 2c. Go outside & scream
            if let Some(album) = album
                && let Some(meta) = self.get_album_meta(&album, true, false, None).await?
            {
                return self
                    .external
                    .call(move |_| {
                        download_image_from_provider(
                            &album.folder_uri,
                            None,
                            &meta.image,
                            thumbnail,
                        )
                    })
                    .await;
            }
        }

        Ok(None)
    }

    /// Shared cover lookup. `folder_key` is the URI used for folder-level images,
    /// `embedded_key` is the URI used for embedded (track-level) images.
    /// If `album` is provided, it is used for external metadata lookups.
    /// Actual implementation is in get_cover_internal. This is a thin RAII wrapper
    /// to handle incrementing/decrementing self.pending_tasks.
    async fn get_cover(
        self: Rc<Self>,
        folder_key: &str,
        embedded_key: &str,
        thumbnail: bool,
        album: Option<AlbumInfo>,
    ) -> Result<Option<Texture>> {
        // Track pending tasks for backpressure-aware hires loading.
        self.pending_tasks.fetch_add(1, Ordering::Relaxed);
        let res = self
            .clone()
            .get_cover_internal(folder_key, embedded_key, thumbnail, album)
            .await;
        self.pending_tasks.fetch_sub(1, Ordering::Relaxed);
        res
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
        let filepath = String::from(
            urlencoding::decode(if path.starts_with("file://") {
                &path[7..]
            } else {
                path
            })
            .map_err(|_| Error::Path)?,
        );
        let cloned_key = key.clone();
        let res = self
            .local
            .call(move |_| {
                let (hires, thumb) = set_image_internal(&cloned_key, key_prefix, &filepath)?;
                Ok((hires, thumb))
            })
            .await;

        if let (Ok(texs), Some(signal)) = (res.as_ref(), notify_signal) {
            // For updates, still notify via signals to update all widgets wherever they are.
            self.get_cache_state()
                .emit_texture(signal, &key, &texs.0, &texs.1);
        }
        res
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
        let cloned_key = key.clone();
        self.local
            .call(move |_| {
                clear_image_internal(&cloned_key, key_prefix)?;
                Ok::<(), Error>(())
            })
            .await?;
        // For updates, still notify via signals to update all widgets wherever they are.
        if let Some(signal) = notify_signal {
            state.emit_with_param(signal, &key);
        }
        Ok(())
    }

    pub async fn set_cover(
        &self,
        folder_uri: String,
        path: &str,
        notify: bool,
    ) -> Result<(gdk::Texture, gdk::Texture)> {
        self.set_image(
            folder_uri,
            None,
            path,
            if notify {
                Some("folder-cover-set")
            } else {
                None
            },
        )
        .await
    }

    pub async fn clear_cover(&self, folder_uri: String, notify: bool) -> Result<()> {
        self.clear_image(
            folder_uri,
            None,
            if notify {
                Some("folder-cover-cleared")
            } else {
                None
            },
        )
        .await
    }

    pub async fn set_artist_avatar(
        &self,
        tag: String,
        path: &str,
        notify: bool,
    ) -> Result<(gdk::Texture, gdk::Texture)> {
        self.set_image(
            tag,
            Some("avatar"),
            path,
            if notify {
                Some("artist-avatar-set")
            } else {
                None
            },
        )
        .await
    }

    pub async fn clear_artist_avatar(&self, tag: String, notify: bool) -> Result<()> {
        self.clear_image(
            tag,
            Some("avatar"),
            if notify {
                Some("artist-avatar-cleared")
            } else {
                None
            },
        )
        .await
    }

    pub async fn set_playlist_cover(
        &self,
        playlist_name: String,
        path: &str,
    ) -> Result<(gdk::Texture, gdk::Texture)> {
        self.set_image(playlist_name, Some("playlist"), path, None)
            .await
    }

    pub async fn clear_playlist_cover(&self, playlist_name: String) -> Result<()> {
        self.clear_image(playlist_name, Some("playlist"), None)
            .await
    }

    pub async fn get_album_meta(
        &self,
        album: &AlbumInfo,
        external: bool,
        overwrite: bool,
        window: Option<&EuphonicaWindow>,
    ) -> Result<Option<models::AlbumMeta>> {
        if !(overwrite && external) {
            // Check whether we have this album cached
            let title = album.title.to_owned();
            let mbid = album.mbid.clone();
            let artist = album.get_artist_tag().map(String::from);

            let local = self
                .local
                .call(move |_| sqlite::find_album_meta(&title, mbid.as_deref(), artist.as_deref()))
                .await
                .map_err(Error::Sqlite)?;

            if local.is_some() {
                return Ok(local);
            }
        }

        if external && (album.mbid.is_some() || album.albumartist.is_some()) {
            let mbid = album.mbid.clone();
            let artist = album.get_artist_tag().map(String::from);
            let title = album.title.to_owned();
            if !overwrite
                && let Some(existing) = self
                    .local
                    .call(move |_| {
                        sqlite::find_album_meta(&title, mbid.as_deref(), artist.as_deref())
                    })
                    .await
                    .map_err(Error::Sqlite)?
            {
                return Ok(Some(existing));
            }
            let res = self
                .meta_providers
                .get_album_meta(album.clone(), None, window)
                .await;
            if let Some(meta) = res {
                sqlite::write_album_meta(album, &meta).map_err(Error::Sqlite)?;
                Ok(Some(meta))
            } else {
                // Push an empty AlbumMeta to block further calls for this album.
                println!(
                    "No album meta could be found for {}. Pushing empty document...",
                    &album.folder_uri
                );
                sqlite::write_album_meta(album, &models::AlbumMeta::from_key(album))
                    .map_err(Error::Sqlite)?;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    pub async fn get_artist_meta(
        &self,
        artist: &ArtistInfo,
        external: bool,
        overwrite: bool,
        window: Option<&EuphonicaWindow>,
    ) -> Result<Option<ArtistMeta>> {
        if !(overwrite && external) {
            // Check whether we have this album cached
            let name = artist.name.to_owned();
            let mbid = artist.mbid.clone();

            let local = self
                .local
                .call(move |_| sqlite::find_artist_meta(&name, mbid.as_deref()))
                .await
                .map_err(Error::Sqlite)?;

            if local.is_some() {
                return Ok(local);
            }
        }

        if external && artist.mbid.is_some() {
            let mbid = artist.mbid.clone();
            let name = artist.name.to_owned();
            if !overwrite
                && let Some(existing) = self
                    .local
                    .call(move |_| sqlite::find_artist_meta(&name, mbid.as_deref()))
                    .await
                    .map_err(Error::Sqlite)?
            {
                return Ok(Some(existing));
            }
            let res = self
                .meta_providers
                .get_artist_meta(artist.clone(), None, window)
                .await;
            if let Some(meta) = res {
                sqlite::write_artist_meta(artist, &meta).map_err(Error::Sqlite)?;
                Ok(Some(meta))
            } else {
                // Push an empty ArtistMeta to block further calls for this artist.
                println!(
                    "No artist meta could be found for {}. Pushing empty document...",
                    &artist.name
                );
                sqlite::write_artist_meta(artist, &models::ArtistMeta::from_key(artist))
                    .map_err(Error::Sqlite)?;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Try to get an avatar for the given artist.
    /// Returns a gdk::Texture from cache if available.
    /// Failing that, we'll search local storage.
    /// Still failing that, if `external` is true, it will try to get one from external providers.
    pub async fn get_artist_avatar(
        self: Rc<Self>,
        artist: &ArtistInfo,
        thumbnail: bool,
        external: bool,
    ) -> Result<Option<Texture>> {
        // First try to get from cache, then from local storage
        let name = artist.name.to_owned();
        let mut failed_before = false;
        match self
            .pool
            .push_future(move || get_image_internal(&name, Some("avatar"), thumbnail))
            .expect("get_artist_avatar: threadpool error")
            .await
            .expect("get_artist_avatar: threadpool error")
        {
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
        if external
            && !failed_before
            && let Some(meta) = self.get_artist_meta(artist, true, false, None).await?
        {
            let artist = artist.to_owned();
            return self
                .external
                .call(move |_| {
                    download_image_from_provider(
                        &artist.name,
                        Some("avatar"),
                        &meta.image,
                        thumbnail,
                    )
                })
                .await;
        }

        Ok(None)
    }

    pub async fn get_playlist_cover(
        &self,
        playlist_name: String,
        is_dynamic_playlist: bool,
        thumbnail: bool,
    ) -> Result<Option<gdk::Texture>> {
        self.pool
            .push_future(move || {
                let prefix = Some(if is_dynamic_playlist {
                    "dynamic_playlist"
                } else {
                    "playlist"
                });
                get_image_internal(&playlist_name, prefix, thumbnail)
            })
            .expect("get_playlist_cover: threadpool error")
            .await
            .expect("get_playlist_cover: threadpool error")
    }

    pub async fn insert_dynamic_playlist(
        &self,
        dp: DynamicPlaylist,
        cover_action: ImageAction,
        overwrite_name: Option<String>,
    ) -> Result<()> {
        self.local
            .call(move |_| {
                // If updating an existing DP, use old name first. SQLite code will migrate it for us.
                let should_overwrite = overwrite_name.is_some();
                let current_cover_key = overwrite_name.unwrap_or_else(|| dp.name.to_owned());
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
                    if should_overwrite {
                        Some(&current_cover_key)
                    } else {
                        None
                    },
                )
                .map_err(Error::Sqlite)
            })
            .await
    }

    pub async fn get_lyrics(
        &self,
        song: &SongInfo,
        external: bool,
        window: Option<&EuphonicaWindow>,
    ) -> Result<Option<Lyrics>> {
        let uri = song.uri.to_owned();
        let local = self
            .local
            .call(move |_| sqlite::find_lyrics(&uri))
            .await
            .map_err(Error::Sqlite)?;
        if local.is_some() {
            return Ok(local);
        }

        if external {
            let song = song.to_owned();
            let res = self.meta_providers.get_lyrics(song.clone(), window).await;
            sqlite::write_lyrics(&song, res.as_ref()).map_err(Error::Sqlite)?;
            Ok(res)
        } else {
            Ok(None)
        }
    }

    pub async fn clear_image_cache(&self) -> Result<()> {
        self.local
            .call(move |_| sqlite::clear_all_images().map_err(Error::Sqlite))
            .await
    }
}
