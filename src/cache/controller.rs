// Cache system to store album arts, artist avatars, wikis, bios,
// you name it.
// This helps avoid having to query the same thing multiple times,
// whether from MPD or from Last.fm.
// - Images are stored as resized WebP files on disk.
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
use std::{cmp, num::NonZeroUsize};
use std::{fmt, fs::create_dir_all, rc::Rc, result, sync::Mutex};
use time::OffsetDateTime;

use crate::{
    client::{Error as ClientError, MpdWrapper},
    common::{AlbumInfo, ArtistInfo},
    meta_providers::{
        MetadataChain,
        models::{self, AlbumMeta},
        prelude::*,
        utils::get_best_image,
    },
    utils::{
        get_app_cache_path, get_image_cache_path, register_image_as_failure,
        save_and_register_image, settings_manager,
    },
    window::EuphonicaWindow,
};
use crate::{
    common::{DynamicPlaylist, SongInfo},
    meta_providers::models::Lyrics,
    utils::strip_filename_linux,
};

use super::{CacheState, sqlite};

#[derive(Debug)]
pub enum Error {
    Download(String),
    Io,
    NotFound,
    AlreadyExists,
    UnknownFormat,
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
            Self::NotFound => "resource not found".into(),
            Self::AlreadyExists => "resource already exists".into(),
            Self::UnknownFormat => "unknown resource format".into(),
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
        if value.matches(gio::IOErrorEnum::NotFound) {
            Error::NotFound
        } else {
            Error::GlibError(value)
        }
    }
}

pub type Result<T> = result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_missing_files_from_load_failures() {
        let path = std::env::temp_dir().join(format!(
            "euphonica-invalid-texture-{}.webp",
            uuid::Uuid::new_v4().simple()
        ));

        let missing_error = Texture::from_filename(&path).err().unwrap();
        assert!(matches!(Error::from(missing_error), Error::NotFound));

        std::fs::write(&path, b"not an image").unwrap();
        let load_error = Texture::from_filename(&path).err().unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(matches!(Error::from(load_error), Error::GlibError(_)));
    }
}

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
        .map_err(|_| Error::NotFound)?
        .decode()
        .map_err(|_| Error::UnknownFormat)?;

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
                match Texture::from_filename(&cover_path).map_err(Error::from) {
                    Ok(tex) => {
                        IMAGE_CACHE.lock().unwrap().put(filename, tex.clone());
                        Ok(Some(tex))
                    }
                    Err(Error::NotFound) => {
                        // File no longer exists (maybe user had removed it). Unregister it from DB.
                        sqlite::unregister_image_key(key, prefix, thumbnail)
                            .map_err(Error::Sqlite)?;
                        Ok(None)
                    }
                    Err(error) => Err(error),
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

    gdk::Texture::from_filename(res).map_err(Error::from)
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
    let maybe_file = sqlite::find_image_by_key(key, prefix, thumbnail).expect("Sqlite DB error");
    if maybe_file.as_ref().is_some_and(|s| !s.is_empty()) {
        read_texture_from_name(&maybe_file.unwrap()).map(Some)
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
    state: CacheState
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
                settings_manager().child("library").uint("n-image-threads"),
            ))
            .expect("Unable to start threadpool for cache operations"),
            state: CacheState::default()
        };

        Rc::new(cache)
    }

    pub fn get_cache_state(&self) -> CacheState {
        self.state.clone()
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
        let res = self
            .clone()
            .get_cover_internal(&folder_uri, &song.uri, thumbnail, album)
            .await;
        res
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
        let res = self
            .clone()
            .get_cover_internal(
                &album.folder_uri,
                &album.example_uri,
                thumbnail,
                Some(album.to_owned()),
            )
            .await;
        res
    }

    /// Lite version of get_album_cover that only takes an example URI.
    /// Used by widgets with limited access to album metadata, such as ArtistCells.
    /// Since it does not take the full album metadata, external fetch is not supported.
    pub async fn get_album_cover_lite(
        self: Rc<Self>,
        example_uri: &str,
        thumbnail: bool,
    ) -> Result<Option<Texture>> {
        let res = self
            .clone()
            .get_cover_internal(
                strip_filename_linux(example_uri),
                example_uri,
                thumbnail,
                None,
            )
            .await;
        res
    }

    /// Shared cover lookup. `folder_key` is the URI used for folder-level images,
    /// `embedded_key` is the URI used for embedded (track-level) images.
    /// If `album` is provided, it is used for external metadata lookups.
    #[inline]
    async fn get_cover_internal(
        self: Rc<Self>,
        folder_key: &str,
        embedded_key: &str,
        thumbnail: bool,
        album: Option<AlbumInfo>,
    ) -> Result<Option<Texture>> {
        let mut failed_before = false;

        // 1. Check if we have it cached locally. This is parallelisable so we'll use the threadpool.
        // Covers are always keyed by example_uri while in cache.
        let cache_key = embedded_key.to_owned();
        match self
            .pool
            .push_future(move || get_image_internal(&cache_key, None, thumbnail))
            .expect("get_cover_internal: cache threadpool error")
            .await
            .expect("get_cover_internal: cache threadpool error")
        {
            Ok(Some(tex)) => return Ok(Some(tex)),
            Ok(None) => {}
            Err(Error::PriorFailure) => failed_before = true,
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
                    .get_folder_cover(embedded_key.to_owned())
                    .map_err(Error::Client)
                    .await?
                {
                    return Ok(bundle.take_texture(thumbnail).map(Some)?);
                }
                // 2b. MPD embedded cover. When caching we'll key by example_uri, or folder_uri if optimize-embedded-cover-loading is enabled.
                if let Some(bundle) = self
                    .mpd_client
                    .get_embedded_cover(embedded_key.to_owned())
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
                            &meta.0.image,
                            thumbnail,
                        )
                    })
                    .await;
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

    /// Function for getting & caching the latest album meta from local sources.
    /// Step 1: get both local and MPD-side last-updated timestamps.
    /// Step 2:
    ///   2a: if local is newer or MPD side is not available, return local version. Call site should show a sync button in this case.
    ///   2b: if MPD is newer or local side is not available, pull from MPD, save to local, then return with MetaSource::Mpd. Call site needs not do anything here.
    ///   2c: if both are available and equal (in sync), we'll read from local BUT return with MetaSource::Mpd, such that call site checks won't ask user to sync.
    async fn get_local_album_meta(
        &self,
        album: &AlbumInfo,
    ) -> Result<Option<(models::AlbumMeta, OffsetDateTime, models::MetaSource)>> {
        let title = album.title.to_owned();
        let mbid = album.mbid.clone();
        let artist = album.get_artist_tag().map(String::from);
        let filter_expr = album.get_filter_expression();

        let (mpd_ts, local_ts) = futures::join!(
            self.mpd_client
                .get_meta_last_modified("filter", filter_expr.clone()),
            // For reads we'll use threadpool instead of the queued asyncified (concurrent reads are okay)
            self.pool
                .push_future(move || {
                    sqlite::get_album_meta_last_modified(&title, mbid.as_deref(), artist.as_deref())
                })
                .expect("get_local_album_meta: threadpool error")
        );
        let mpd_ts = mpd_ts.ok().flatten();
        let local_ts = local_ts
            .expect("get_local_album_meta: threadpool error")
            .map_err(Error::Sqlite)?;

        // Handle case when both are None first; afterwards at least one side will be non-None and we can compare them directly.
        if local_ts.is_none() && mpd_ts.is_none() {
            Ok(None)
        } else {
            match local_ts.cmp(&mpd_ts) {
                cmp::Ordering::Greater => {
                    // 2a
                    let title = album.title.to_owned();
                    let mbid = album.mbid.clone();
                    let artist = album.get_artist_tag().map(String::from);

                    self.pool
                        .push_future(move || {
                            sqlite::get_album_meta(&title, mbid.as_deref(), artist.as_deref())
                        })
                        .expect("get_local_album_meta: threadpool error")
                        .await
                        .expect("get_local_album_meta: threadpool error")
                        .map(|om| om.map(|m| (m, local_ts.unwrap(), models::MetaSource::Local)))
                        .map_err(Error::Sqlite)
                }
                cmp::Ordering::Less => {
                    // 2b
                    let from_mpd = self
                        .mpd_client
                        .get_meta::<AlbumMeta>("filter", filter_expr)
                        .await
                        .map_err(Error::Client)?;

                    if let Some(meta) = from_mpd.as_ref() {
                        let uri = album.folder_uri.to_owned();
                        let title = album.title.to_owned();
                        let mbid = album.mbid.clone();
                        let artist = album.get_artist_tag().map(String::from);
                        let to_local = meta.clone();
                        // Store locally (skip on error)
                        if let Err(e) = self
                            .local
                            .call(move |_| {
                                let mut info = AlbumInfo::default();
                                info.folder_uri = uri;
                                info.title = title;
                                info.albumartist = artist;
                                info.mbid = mbid;
                                // Use mpd_ts such that future comparisons between this local copy and the MPD sticker version
                                // will be exactly equal (2c).
                                sqlite::write_album_meta(&info, &to_local, mpd_ts, true)
                            })
                            .await
                        {
                            dbg!(e);
                        }
                    }
                    Ok(from_mpd.map(|m| (m, mpd_ts.unwrap(), models::MetaSource::Mpd)))
                }
                cmp::Ordering::Equal => {
                    // 2c
                    let title = album.title.to_owned();
                    let mbid = album.mbid.clone();
                    let artist = album.get_artist_tag().map(String::from);
                    self.pool
                        .push_future(move || {
                            sqlite::get_album_meta(&title, mbid.as_deref(), artist.as_deref())
                        })
                        .expect("get_local_album_meta: threadpool error")
                        .await
                        .expect("get_local_album_meta: threadpool error")
                        // Use MetaSource::Mpd to make it look like it's already backed up to MPD (well, it is)
                        .map(|om| om.map(|m| (m, local_ts.unwrap(), models::MetaSource::Mpd)))
                        .map_err(Error::Sqlite)
                }
            }
        }
    }

    pub async fn get_album_meta(
        &self,
        album: &AlbumInfo,
        external: bool,  // allow external fetching
        overwrite: bool, // overwrite existing with external if any (will also skip the exists check)
        window: Option<&EuphonicaWindow>,
    ) -> Result<Option<(models::AlbumMeta, OffsetDateTime, models::MetaSource)>> {
        if !(overwrite && external)
            && let Ok(Some(local_res)) = self.get_local_album_meta(album).await
        {
            return Ok(Some(local_res));
        }

        if external && (album.mbid.is_some() || album.albumartist.is_some()) {
            if !overwrite && let Ok(Some(local_res)) = self.get_local_album_meta(album).await {
                return Ok(Some(local_res));
            }
            if let Some(meta) = self
                .meta_providers
                .get_album_meta(album.clone(), None, window)
                .await
            {
                let ts = sqlite::write_album_meta(album, &meta, None, true).map_err(Error::Sqlite)?;
                Ok(Some((meta, ts, models::MetaSource::External)))
            } else {
                // Push an empty AlbumMeta to block further calls for this album.
                println!(
                    "No album meta could be found for {}. Pushing empty document...",
                    &album.folder_uri
                );
                sqlite::write_album_meta(album, &models::AlbumMeta::from_key(album), None, false)
                    .map_err(Error::Sqlite)?;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Back up album metadata document to MPD sticker store.
    /// Uses `typ="filter"` with the album's filter expression as URI.
    /// We use two timestamps to resolve sync conflicts:
    /// - old_last_modified: the last-modified timestamp BEFORE local edits. This is compared against what's currently in the sticker DB.
    ///   Using the pre-edit timestamp facilitates detecting that we're attempting to push an edited version of an outdated copy as backup,
    ///   or other issues that may result in the local copy being generally outdated in itself. If no edit was performed (i.e. a manual sync),
    ///   use the same value as new_last_modified here.
    /// - new_last_modified: time of local edit. This will be the last_updated timestamp written to the sticker store if there is no conflict
    ///   or if overwrite_newer is true. The exact same value must then be stored in our local SQLite DB to keep it and MPD in sync.
    pub async fn backup_album_meta(
        &self,
        album: &AlbumInfo,
        meta: &models::AlbumMeta,
        old_last_modified: OffsetDateTime,
        overwrite_newer: bool,
        new_last_modified: OffsetDateTime,
    ) -> Result<()> {
        let filter_expr = album.get_filter_expression();

        if self
            .mpd_client
            .get_meta_last_modified("filter", filter_expr.clone())
            .await
            .is_ok_and(|maybe_mpdlm| {
                maybe_mpdlm.is_some_and(|mpd_last_modified| mpd_last_modified > old_last_modified)
            })
            && !overwrite_newer
        {
            return Err(Error::AlreadyExists);
        }
        self.mpd_client
            .set_meta::<AlbumMeta>("filter", filter_expr, meta, new_last_modified)
            .await
            .map_err(Error::Client)
    }

    pub fn set_album_meta(
        &self,
        album: &AlbumInfo,
        meta: &models::AlbumMeta,
        update_tags: bool
    ) -> Result<OffsetDateTime> {
        sqlite::write_album_meta(album, meta, None, update_tags).map_err(Error::Sqlite)
    }

    pub fn set_album_tags(&self, folder_uri: &str, tags: &[models::Tag]) -> Result<()> {
        sqlite::write_album_tags(folder_uri, tags, sqlite::TagsInsertMode::Delsert)
            .map_err(Error::Sqlite)
    }

    pub fn get_album_tags(&self, folder_uri: &str) -> Result<Vec<models::Tag>> {
        sqlite::find_album_tags(folder_uri).map_err(Error::Sqlite)
    }

    /// Get the latest artist meta from local sources (with MPD/external fallback).
    /// Step 1: get both local and MPD-side last-updated timestamps via `get_local_artist_meta`.
    /// Step 2:
    ///   2a: if local is newer or MPD side is not available, return local version. Call site should show a sync button.
    ///   2b: if MPD is newer or local side is not available, return with MetaSource::Mpd. Call site needs not do anything.
    ///   2c: if both are available and equal (in sync), return with MetaSource::Mpd so call site checks won't ask user to sync.
    /// Step 3: if no local/MPD copy and `external` is true, fetch from external providers.
    pub async fn get_artist_meta(
        &self,
        artist: &ArtistInfo,
        external: bool,
        overwrite: bool,
        window: Option<&EuphonicaWindow>,
    ) -> Result<Option<(models::ArtistMeta, OffsetDateTime, models::MetaSource)>> {
        if !(overwrite && external)
            && let Ok(Some(local_res)) = self.get_local_artist_meta(artist).await
        {
            return Ok(Some(local_res));
        }

        if external && artist.mbid.is_some() {
            if !overwrite && let Ok(Some(local_res)) = self.get_local_artist_meta(artist).await {
                return Ok(Some(local_res));
            }
            if let Some(meta) = self
                .meta_providers
                .get_artist_meta(artist.clone(), None, window)
                .await
            {
                let ts = sqlite::write_artist_meta(artist, &meta, true).map_err(Error::Sqlite)?;
                Ok(Some((meta, ts, models::MetaSource::External)))
            } else {
                // Push an empty ArtistMeta to block further calls for this artist.
                println!(
                    "No artist meta could be found for {}. Pushing empty document...",
                    &artist.name
                );
                sqlite::write_artist_meta(artist, &models::ArtistMeta::from_key(artist), false)
                    .map_err(Error::Sqlite)?;
                Ok(None)
            }
        } else {
            Ok(None)
        }
    }

    /// Back up artist metadata document to MPD sticker store.
    /// Uses `typ="filter"` with the artist's filter expression as URI.
    /// See `backup_album_meta` for conflict resolution details.
    pub async fn backup_artist_meta(
        &self,
        artist: &ArtistInfo,
        meta: &models::ArtistMeta,
        old_last_modified: OffsetDateTime,
        overwrite_newer: bool,
        new_last_modified: OffsetDateTime,
    ) -> Result<()> {
        let filter_expr = artist.get_filter_expression();

        if self
            .mpd_client
            .get_meta_last_modified("filter", filter_expr.clone())
            .await
            .is_ok_and(|maybe_mpdlm| {
                maybe_mpdlm.is_some_and(|mpd_last_modified| mpd_last_modified > old_last_modified)
            })
            && !overwrite_newer
        {
            return Err(Error::AlreadyExists);
        }
        self.mpd_client
            .set_meta::<models::ArtistMeta>("filter", filter_expr, meta, new_last_modified)
            .await
            .map_err(Error::Client)
    }

    /// Get the latest artist meta from local sources with 3-way timestamp comparison.
    /// Mirrors `get_local_album_meta` but uses `typ="filter"` and filter expression as URI.
    async fn get_local_artist_meta(
        &self,
        artist: &ArtistInfo,
    ) -> Result<Option<(models::ArtistMeta, OffsetDateTime, models::MetaSource)>> {
        let name = artist.name.to_owned();
        let mbid = artist.mbid.clone();
        let filter_expr = artist.get_filter_expression();

        let (mpd_ts, local_ts) = futures::join!(
            self.mpd_client
                .get_meta_last_modified("filter", filter_expr.clone()),
            self.pool
                .push_future(move || {
                    sqlite::get_artist_meta_last_modified(&name, mbid.as_deref())
                })
                .expect("get_local_artist_meta: threadpool error")
        );
        let mpd_ts = mpd_ts.ok().flatten();
        let local_ts = local_ts
            .expect("get_local_artist_meta: threadpool error")
            .map_err(Error::Sqlite)?;

        // Handle case when both are None first; afterwards at least one side will be non-None and we can compare them directly.
        if local_ts.is_none() && mpd_ts.is_none() {
            Ok(None)
        } else {
            match local_ts.cmp(&mpd_ts) {
                cmp::Ordering::Greater => {
                    let name = artist.name.to_owned();
                    let mbid = artist.mbid.clone();
                    self.pool
                        .push_future(move || sqlite::get_artist_meta(&name, mbid.as_deref()))
                        .expect("get_local_artist_meta: threadpool error")
                        .await
                        .expect("get_local_artist_meta: threadpool error")
                        .map(|om| om.map(|m| (m, local_ts.unwrap(), models::MetaSource::Local)))
                        .map_err(Error::Sqlite)
                }
                cmp::Ordering::Less => {
                    // MPD newer => pull from MPD, save to SQLite, return MetaSource::Mpd
                    let filter_expr = artist.get_filter_expression();
                    let from_mpd = self
                        .mpd_client
                        .get_meta::<models::ArtistMeta>("filter", filter_expr)
                        .await
                        .map_err(Error::Client)?;

                    if let Some(meta) = from_mpd.as_ref() {
                        let to_local = meta.clone();
                        let artist = artist.to_owned();
                        if let Err(e) = self
                            .local
                            .call(move |_| sqlite::write_artist_meta(&artist, &to_local, true))
                            .await
                        {
                            dbg!(e);
                        }
                    }
                    Ok(from_mpd.map(|m| (m, mpd_ts.unwrap(), models::MetaSource::Mpd)))
                }
                cmp::Ordering::Equal => {
                    // In sync
                    let name = artist.name.to_owned();
                    let mbid = artist.mbid.clone();
                    self.pool
                        .push_future(move || sqlite::get_artist_meta(&name, mbid.as_deref()))
                        .expect("get_local_artist_meta: threadpool error")
                        .await
                        .expect("get_local_artist_meta: threadpool error")
                        // Use MetaSource::Mpd to make it look like it's already backed up to MPD (well, it is)
                        .map(|om| om.map(|m| (m, local_ts.unwrap(), models::MetaSource::Mpd)))
                        .map_err(Error::Sqlite)
                }
            }
        }
    }

    pub fn set_artist_meta(
        &self,
        artist: &ArtistInfo,
        meta: &models::ArtistMeta,
        update_tags: bool
    ) -> Result<OffsetDateTime> {
        sqlite::write_artist_meta(artist, meta, update_tags).map_err(Error::Sqlite)
    }

    pub fn set_artist_tags(&self, name: &str, tags: &[models::Tag]) -> Result<()> {
        sqlite::write_artist_tags(name, tags, sqlite::TagsInsertMode::Delsert)
            .map_err(Error::Sqlite)
    }

    pub fn get_artist_tags(&self, name: &str) -> Result<Vec<models::Tag>> {
        sqlite::find_artist_tags(name).map_err(Error::Sqlite)
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
            && let Some((meta, _, _)) = self.get_artist_meta(artist, true, false, None).await?
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
        overwrite: bool,   // only effective when external == true
        window: Option<&EuphonicaWindow>,
    ) -> Result<Option<Lyrics>> {
        let uri = song.uri.to_owned();
        if !overwrite & !external {
            match self.local.call(move |_| sqlite::find_lyrics(&uri)).await {
                Ok(Some(lyrics)) => {
                    return Ok(Some(lyrics));
                }
                Ok(None) => {
                    // Nothing found (haven't tried) => allow function to continue
                }
                Err(e) => {
                    // Includes the "do not retry case". Caller of get_lyrics() should display a "re-fetch" button.
                    return Err(Error::Sqlite(e));
                }
            }
        }

        if external {
            let song = song.to_owned();
            println!("Fetching new lyrics...");
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
