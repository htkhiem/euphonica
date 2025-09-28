use std::{borrow::Cow, hash::BuildHasherDefault, num::NonZero, ops::Range, sync::Mutex};

use async_channel::{SendError, Sender};
use gio::prelude::SettingsExt;
use gtk::gdk;
use lru::LruCache;
use nohash_hasher::NoHashHasher;
use once_cell::sync::Lazy;
use time::OffsetDateTime;

use mpd::{
    error::{Error as MpdError, ErrorCode},
    search::{Operation as QueryOperation, Query, Term, Window}, Id,
};
use rustc_hash::FxHashSet;

use crate::{cache::{get_new_image_paths, sqlite}, common::{dynamic_playlist::{QueryLhs, Rule, StickerOperation}, SongInfo}, meta_providers::ProviderMessage, utils::{self, strip_filename_linux}};

use super::*;

const BATCH_SIZE: usize = 128;
const FETCH_LIMIT: usize = 10000000; // Fetch at most ten million songs at once (same
// folder, same tag, etc)

// Cache song infos so we can reuse them on queue updates.
// Song IDs are u32s anyway, and I don't think there's any risk of a HashDoS attack
// from a self-hosted music server so we'll just use identity hash for speed.
static QUEUED_SONG_CACHE: Lazy<Mutex<LruCache<u32, SongInfo, BuildHasherDefault<NoHashHasher<u32>>>>> =
    Lazy::new(|| Mutex::new(
        LruCache::with_hasher(NonZero::new(16384).unwrap(), BuildHasherDefault::default())
    ));

pub fn update_mpd_database(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    match client.update() {
        Ok(_) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::DBUpdated);
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    }
}

pub fn get_current_queue(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    // This command is only called upon connection so we should drop the entire cache
    {
        QUEUED_SONG_CACHE.lock().unwrap().clear();
    }
    let mut curr_len: usize = 0;
    let mut more: bool = true;
    while more && (curr_len) < FETCH_LIMIT {
        match client.queue(
            Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
        ) {
            Ok(mut mpd_songs) => {
                let songs: Vec<SongInfo> = mpd_songs
                    .iter_mut()
                    .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                    .collect();
                if !songs.is_empty() {
                    // Cache
                    let mut cache = QUEUED_SONG_CACHE.lock().unwrap();
                    for song in songs.iter() {
                        if let Some(id) = song.queue_id {
                            cache.put(
                                id, song.clone()
                            );
                        }
                    }
                    let _ = sender_to_fg.send_blocking(AsyncClientMessage::QueueSongsDownloaded(
                        songs
                    ));
                    curr_len += BATCH_SIZE;
                } else {
                    more = false;
                }
            }
            Err(MpdError::Server(e)) => {
                if e.code != ErrorCode::Argument {
                    dbg!(e);
                }
                // Else assume it's because we've completely fetched the queue
                more = false;
            }
            Err(mpd_error) => {
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
            }
        }
    }
}

pub fn get_queue_changes(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    curr_version: u32,
    total_len: u32
) {
    let mut curr_len: usize = 0;
    while curr_len < total_len as usize {
        match client.changesposid(
            curr_version,
            Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
        ) {
            Ok(changes) => {
                if !changes.is_empty() {
                    // Map to songs.
                    // Use this background client to fetch cache misses to avoid blocking UI.
                    let mut cache = QUEUED_SONG_CACHE.lock().unwrap();
                    let songs: Vec<SongInfo> = changes
                        .into_iter()
                        .map(|change| {
                            if let Some(cached_song) = cache.get(&change.id.0) {
                                let mut song = cached_song.clone();
                                song.queue_pos = Some(change.pos);
                                song
                            } else {
                                if let Ok(mut songs) = client.songs(change.id) {
                                    let song = SongInfo::from(std::mem::take(&mut songs[0]));
                                    cache.push(change.id.0, song.clone());
                                    song
                                } else {
                                    // Queue has probably changed again. Push empty song &
                                    // wait for next refresh.
                                    let mut song = SongInfo::default();
                                    song.queue_id = Some(change.id.0);
                                    song.queue_pos = Some(change.pos);
                                    song
                                }
                            }
                        })
                        .collect();
                    let _ = sender_to_fg.send_blocking(AsyncClientMessage::QueueChangesReceived(
                        songs
                    ));
                }
            }
            Err(MpdError::Server(e)) => {
                if e.code != ErrorCode::Argument {
                    dbg!(e);
                }
                // Else assume it's because we've completely fetched the changes
            }
            Err(mpd_error) => {
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
            }
        }
        curr_len += BATCH_SIZE;
    }
}

fn download_embedded_cover_inner(
    client: &mut mpd::Client<stream::StreamWrapper>,
    uri: String,
) -> Option<(gdk::Texture, gdk::Texture)> {
    if let Some(dyn_img) = client
        .readpicture(&uri)
        .map_or(None, |bytes| utils::read_image_from_bytes(bytes))
    {
        let (hires, thumb) = utils::resize_convert_image(dyn_img);
        let (path, thumbnail_path) = get_new_image_paths();
        hires
            .save(&path)
            .expect(&format!("Couldn't save downloaded cover to {:?}", &path));
        thumb.save(&thumbnail_path).expect(&format!(
            "Couldn't save downloaded thumbnail cover to {:?}",
            &thumbnail_path
        ));
        sqlite::register_image_key(
            uri.clone(),
            None,
            Some(path.file_name().unwrap().to_str().unwrap().to_string()),
            false,
        )
        .join()
        .unwrap()
        .expect("Sqlite DB error");
        sqlite::register_image_key(
            uri.clone(),
            None,
            Some(thumbnail_path.file_name().unwrap().to_str().unwrap().to_string()),
            true,
        )
        .join()
        .unwrap()
        .expect("Sqlite DB error");
        let hires_tex = gdk::Texture::from_filename(&path).unwrap();
        let thumb_tex = gdk::Texture::from_filename(&thumbnail_path).unwrap();
        Some((hires_tex, thumb_tex))
    } else {
        None
    }
}

fn download_folder_cover_inner(
    client: &mut mpd::Client<stream::StreamWrapper>,
    folder_uri: String,
) -> Option<(gdk::Texture, gdk::Texture)> {
    if let Some(dyn_img) = client
        .albumart(&folder_uri)
        .map_or(None, |bytes| utils::read_image_from_bytes(bytes))
    {
        let (hires, thumb) = utils::resize_convert_image(dyn_img);
        let (path, thumbnail_path) = get_new_image_paths();
        hires
            .save(&path)
            .expect(&format!("Couldn't save downloaded cover to {:?}", &path));
        thumb.save(&thumbnail_path).expect(&format!(
            "Couldn't save downloaded thumbnail cover to {:?}",
            &thumbnail_path
        ));
        sqlite::register_image_key(
            folder_uri.clone(),
            None,
            Some(path.file_name().unwrap().to_str().unwrap().to_string()),
            false,
        )
        .join()
        .unwrap()
        .expect("Sqlite DB error");
        sqlite::register_image_key(
            folder_uri.clone(),
            None,
            Some(thumbnail_path.file_name().unwrap().to_str().unwrap().to_string()),
            true,
        )
        .join()
        .unwrap()
        .expect("Sqlite DB error");
        let hires_tex = gdk::Texture::from_filename(&path).unwrap();
        let thumb_tex = gdk::Texture::from_filename(&thumbnail_path).unwrap();
        Some((hires_tex, thumb_tex))
    } else {
        None
    }
}

pub fn download_embedded_cover(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_cache: &Sender<ProviderMessage>,
    key: SongInfo,
) {
    // Still prioritise folder-level art if allowed to
    let folder_uri = strip_filename_linux(&key.uri).to_owned();
    // Re-check in case previous iterations have already downloaded these.
    // Check using thumbnail = true to quickly refresh cache after a deletion of the entire
    // images folder. This is because upon startup we'll mass-schedule thumbnail fetches, so
    // in case the folder has been deleted, only thumbnail records in the SQLite DB will be
    // dropped. Checking with thumbnail=true will still return a path even though that
    // path has already been deleted, preventing downloading from proceeding.
    let folder_path = sqlite::find_image_by_key(&folder_uri, None, true).expect("Sqlite DB error");
    if folder_path.is_none() {
        if let Some((hires_tex, thumb_tex)) =
            download_folder_cover_inner(client, folder_uri.clone())
        {
            sender_to_cache
                .send_blocking(ProviderMessage::CoverAvailable(
                    folder_uri.clone(),
                    false,
                    hires_tex,
                ))
                .expect("Cannot notify main cache of folder cover download result.");
            sender_to_cache
                .send_blocking(ProviderMessage::CoverAvailable(folder_uri, true, thumb_tex))
                .expect("Cannot notify main cache of folder cover download result.");
            return;
        } // No folder-level art was available. Proceed to actually fetch embedded art.
    } else if folder_path.as_ref().map_or(false, |p| p.len() > 0) {
        // Nothing to do, as there's already a path in the DB.
        return;
    }
    // Re-check in case previous iterations have already downloaded these.
    let uri = key.uri.to_owned();
    if sqlite::find_image_by_key(&uri, None, true)
        .expect("Sqlite DB error")
        .is_none()
    {
        if let Some((hires_tex, thumb_tex)) = download_embedded_cover_inner(client, uri.clone()) {
            sender_to_cache
                .send_blocking(ProviderMessage::CoverAvailable(
                    uri.clone(),
                    false,
                    hires_tex,
                ))
                .expect("Cannot notify main cache of embedded cover download result.");
            sender_to_cache
                .send_blocking(ProviderMessage::CoverAvailable(uri, true, thumb_tex))
                .expect("Cannot notify main cache of embedded cover download result.");
            return;
        }
        if let Some(album) = &key.album {
            // Go straight to external metadata providers since we've already
            // failed to fetch folder-level cover from MPD at this point.
            // Don't schedule again if we've come back empty-handed once before.
            if folder_path.is_none() {
                sender_to_cache
                    .send_blocking(ProviderMessage::FetchFolderCoverExternally(album.clone()))
                    .expect("Cannot signal main cache to run fallback folder cover logic.");
                return;
            }
        }
        sender_to_cache
            .send_blocking(ProviderMessage::CoverNotAvailable(uri))
            .expect("Cannot notify main cache of embedded cover download result.");
    } else {
        // Nothing to do, as there's already a path in the DB
        return;
    }
}

pub fn download_folder_cover(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_cache: &Sender<ProviderMessage>,
    key: AlbumInfo,
) {
    // Re-check in case previous iterations have already downloaded these.
    if sqlite::find_image_by_key(&key.folder_uri, None, true)
        .expect("Sqlite DB error")
        .is_none()
    {
        let folder_uri = key.folder_uri.to_owned();
        if let Some((hires_tex, thumb_tex)) =
            download_folder_cover_inner(client, folder_uri.clone())
        {
            sender_to_cache
                .send_blocking(ProviderMessage::CoverAvailable(
                    key.folder_uri.clone(),
                    false,
                    hires_tex,
                ))
                .expect("Cannot notify main cache of folder cover download result.");
            sender_to_cache
                .send_blocking(ProviderMessage::CoverAvailable(
                    key.folder_uri,
                    true,
                    thumb_tex,
                ))
                .expect("Cannot notify main cache of folder cover download result.");
        } else {
            // Fall back to embedded art.
            let uri = key.example_uri.to_owned();
            let sqlite_path = sqlite::find_image_by_key(&uri, None, true).expect("Sqlite DB error");
            if sqlite_path.is_none() {
                if let Some((hires_tex, thumb_tex)) =
                    download_embedded_cover_inner(client, uri.clone())
                {
                    sender_to_cache
                        .send_blocking(ProviderMessage::CoverAvailable(
                            uri.clone(),
                            false,
                            hires_tex,
                        ))
                        .expect("Cannot notify main cache of embedded fallback download result.");
                    sender_to_cache
                        .send_blocking(ProviderMessage::CoverAvailable(uri, true, thumb_tex))
                        .expect("Cannot notify main cache of embedded fallback download result.");
                    return;
                }
            } else if sqlite_path.as_ref().map_or(false, |p| p.len() > 0) {
                // Nothing to do, as there's already a path in the DB.
                return;
            }
            sender_to_cache
                .send_blocking(ProviderMessage::FetchFolderCoverExternally(key))
                .expect("Cannot signal main cache to fetch cover externally.");
        }
    }
}

// Err is true when a reconnection should be attempted
fn fetch_albums_by_query<F>(
    client: &mut mpd::Client<stream::StreamWrapper>,
    query: &Query,
    respond: F,
) -> Result<(), MpdError>
where
    F: Fn(AlbumInfo) -> Result<(), SendError<AsyncClientMessage>>,
{
    // TODO: batched windowed retrieval
    // Get list of unique album tags, grouped by albumartist
    // Will block child thread until info for all albums have been retrieved.
    match client.list(
        &Term::Tag(Cow::Borrowed("album")),
        query,
        Some("albumartist"),
    ) {
        Ok(grouped_vals) => {
            for (key, tags) in grouped_vals.groups.into_iter() {
                for tag in tags.iter() {
                    match client.find(
                        Query::new()
                            .and(Term::Tag(Cow::Borrowed("album")), tag)
                            .and(Term::Tag(Cow::Borrowed("albumartist")), &key),
                        Window::from((0, 1)),
                    ) {
                        Ok(mut songs) => {
                            if !songs.is_empty() {
                                let info = SongInfo::from(std::mem::take(&mut songs[0]))
                                    .into_album_info()
                                    .unwrap_or_default();
                                let _ = respond(info);
                            }
                        }
                        Err(e) => {
                            dbg!(e);
                        }
                    }
                }
            }
            Ok(())
        }
        Err(mpd_error) => {
            Err(mpd_error)
        }
    }
}

fn fetch_songs_by_query<F>(
    client: &mut mpd::Client<stream::StreamWrapper>,
    query: &Query,
    mut respond: F,
) where
    F: FnMut(Vec<SongInfo>) -> Result<(), SendError<AsyncClientMessage>>,
{
    let mut curr_len: usize = 0;
    let mut more: bool = true;
    while more && (curr_len) < FETCH_LIMIT {
        match client.find(
            query,
            Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32)),
        ) {
            Ok(mut mpd_songs) => {
                let songs: Vec<SongInfo> = mpd_songs
                    .iter_mut()
                    .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                    .collect();
                if !songs.is_empty() {
                    let _ = respond(songs);
                    curr_len += BATCH_SIZE;
                } else {
                    more = false;
                }
            }
            Err(e) => {
                dbg!(e);
            }
        }
    }
}

fn fetch_uris_by_sticker<F>(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sticker: &str,
    op: StickerOperation,
    rhs: &str,
    mut respond: F,
) where
    F: FnMut(Vec<String>) -> Result<(), SendError<AsyncClientMessage>>,
{
    let mut curr_len: usize = 0;
    let mut more: bool = true;
    while more && (curr_len) < FETCH_LIMIT {
        match client.find_sticker_op(
            "song", "", sticker, op.to_mpd_syntax(), rhs,
            Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
        ) {
            Ok(uris) => {
                if !uris.is_empty() {
                    let _ = respond(uris);
                    curr_len += BATCH_SIZE;
                } else {
                    more = false;
                }
            }
            Err(e) => {
                dbg!(e);
            }
        }
    }
}

/// Fetch all albums, using AlbumArtist to further disambiguate same-named ones.
pub fn fetch_all_albums(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    match fetch_albums_by_query(client, &Query::new(), |info| {
        sender_to_fg.send_blocking(AsyncClientMessage::AlbumBasicInfoDownloaded(info))
    }) {
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
        _ => {}
    }
}

pub fn fetch_recent_albums(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    let settings = utils::settings_manager().child("library");
    let recent_albums =
        sqlite::get_last_n_albums(settings.uint("n-recent-albums")).expect("Sqlite DB error");
    for tup in recent_albums.into_iter() {
        let mut query = Query::new();
        query.and(Term::Tag(Cow::Borrowed("album")), tup.0);
        if let Some(artist) = tup.1 {
            query.and(Term::Tag(Cow::Borrowed("albumartist")), artist);
        }
        if let Some(mbid) = tup.2 {
            query.and(Term::Tag(Cow::Borrowed("musicbrainz_albumid")), mbid);
        }
        match fetch_albums_by_query(client, &query, |info| {
            sender_to_fg.send_blocking(AsyncClientMessage::RecentAlbumDownloaded(info))
        }) {
            Err(mpd_error) => {
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
            }
            _ => {}
        }
    }
}

pub fn fetch_albums_of_artist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    artist_name: String,
) {
    match fetch_albums_by_query(
        client,
        Query::new().and_with_op(
            Term::Tag(Cow::Borrowed("artist")),
            QueryOperation::Contains,
            artist_name.clone(),
        ),
        |info| {
            sender_to_fg.send_blocking(AsyncClientMessage::ArtistAlbumBasicInfoDownloaded(
                artist_name.clone(),
                info,
            ))
        },
    ) {
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
        _ => {}
    }
}

pub fn fetch_album_songs(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    tag: String,
) {
    fetch_songs_by_query(
        client,
        Query::new().and(Term::Tag(Cow::Borrowed("album")), tag.clone()),
        |songs| {
            sender_to_fg.send_blocking(AsyncClientMessage::AlbumSongInfoDownloaded(
                tag.clone(),
                songs,
            ))
        },
    );
}

pub fn fetch_artists(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    use_album_artist: bool,
) {
    // Fetching artists is a bit more involved: artist tags usually contain multiple artists.
    // For the same reason, one artist can appear in multiple tags.
    // Here we'll reuse the artist parsing code in our SongInfo struct and put parsed
    // ArtistInfos in a Set to deduplicate them.
    let tag_type: &'static str = if use_album_artist {
        "albumartist"
    } else {
        "artist"
    };
    let mut already_parsed: FxHashSet<String> = FxHashSet::default();
    match client.list(&Term::Tag(Cow::Borrowed(tag_type)), &Query::new(), None) {
        Ok(grouped_vals) => {
            // TODO: Limit tags to only what we need locally
            for tag in &grouped_vals.groups[0].1 {
                if let Ok(mut songs) = client.find(
                    Query::new().and(Term::Tag(Cow::Borrowed(tag_type)), tag),
                    Window::from((0, 1)),
                ) {
                    if !songs.is_empty() {
                        let first_song = SongInfo::from(std::mem::take(&mut songs[0]));
                        let artists = first_song.into_artist_infos();
                        // println!("Got these artists: {artists:?}");
                        for artist in artists.into_iter() {
                            if already_parsed.insert(artist.name.clone()) {
                                // println!("Never seen {artist:?} before, inserting...");
                                let _ = sender_to_fg.send_blocking(
                                    AsyncClientMessage::ArtistBasicInfoDownloaded(artist),
                                );
                            }
                        }
                    }
                }
            }
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    }
}

pub fn fetch_recent_artists(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    let mut already_parsed: FxHashSet<String> = FxHashSet::default();
    let settings = utils::settings_manager().child("library");
    let n = settings.uint("n-recent-artists");
    let mut res: Vec<ArtistInfo> = Vec::with_capacity(n as usize);
    let recent_names = sqlite::get_last_n_artists(n).expect("Sqlite DB error");
    let mut recent_names_set: FxHashSet<String> = FxHashSet::default();
    for name in recent_names.iter() {
        recent_names_set.insert(name.clone());
    }
    for name in recent_names.iter() {
        match client.find(
            Query::new().and_with_op(
                Term::Tag(Cow::Borrowed("artist")),
                QueryOperation::Contains,
                name,
            ),
            Window::from((0, 1)),
        ) {
            Ok(mut songs) => {
                if !songs.is_empty() {
                    let first_song = SongInfo::from(std::mem::take(&mut songs[0]));
                    let artists = first_song.into_artist_infos();
                    for artist in artists.into_iter() {
                        if recent_names_set.contains(&artist.name) {
                            if already_parsed.insert(artist.name.clone()) {
                                res.push(artist);
                            }
                        }
                    }
                }
            }
            Err(MpdError::Io(_)) => {
                // Connection error => attempt to reconnect
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::Connect);
                return;
            }
            _ => {}
        }
    }

    for artist in res.into_iter() {
        let _ = sender_to_fg.send_blocking(AsyncClientMessage::RecentArtistDownloaded(artist));
    }
}

pub fn fetch_songs_of_artist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    name: String,
) {
    fetch_songs_by_query(
        client,
        Query::new().and_with_op(
            Term::Tag(Cow::Borrowed("artist")),
            QueryOperation::Contains,
            name.clone(),
        ),
        |songs| {
            sender_to_fg.send_blocking(AsyncClientMessage::ArtistSongInfoDownloaded(
                name.clone(),
                songs,
            ))
        },
    );
}

pub fn fetch_folder_contents(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    path: String,
) {
    match client.lsinfo(&path) {
        Ok(contents) => {
            let _ = sender_to_fg
                .send_blocking(AsyncClientMessage::FolderContentsDownloaded(path, contents));
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    }
}

pub fn fetch_playlist_songs(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    name: String,
) {
    if client.version.1 < 24 {
        match client.playlist(&name, Option::<Range<u32>>::None) {
            Ok(mut mpd_songs) => {
                let songs: Vec<SongInfo> = mpd_songs
                    .iter_mut()
                    .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                    .collect();
                if !songs.is_empty() {
                    let _ = sender_to_fg.send_blocking(
                        AsyncClientMessage::PlaylistSongInfoDownloaded(name.clone(), songs),
                    );
                }
            }
            Err(mpd_error) => {
                let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
            }
        }
    } else {
        // For MPD 0.24+, use the new paged loading
        let mut curr_len: u32 = 0;
        let mut more: bool = true;
        while more && (curr_len as usize) < FETCH_LIMIT {
            match client.playlist(&name, Some(curr_len..(curr_len + BATCH_SIZE as u32))) {
                Ok(mut mpd_songs) => {
                    let songs: Vec<SongInfo> = mpd_songs
                        .iter_mut()
                        .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                        .collect();
                    more = songs.len() >= BATCH_SIZE as usize;
                    if !songs.is_empty() {
                        curr_len += songs.len() as u32;
                        let _ = sender_to_fg.send_blocking(
                            AsyncClientMessage::PlaylistSongInfoDownloaded(name.clone(), songs),
                        );
                    }
                }
                Err(mpd_error) => {
                    let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
                }
            }
        }
    }
}

pub fn fetch_songs_by_uri(
    client: &mut mpd::Client<stream::StreamWrapper>,
    uris: &[&str],
) -> Result<Vec<SongInfo>, MpdError> {
    let mut res: Vec<mpd::Song> = Vec::with_capacity(uris.len());
    for uri in uris.iter() {
        match client.find(Query::new().and(Term::File, *uri), None) {
            Ok(mut found_songs) => {
                if found_songs.len() > 0 {
                    res.push(found_songs.remove(0));
                }
            }
            Err(mpd_error) => {
                return Err(mpd_error);
            }
        }
    }

    Ok(res
        .into_iter()
        .map(|mut mpd_song| SongInfo::from(std::mem::take(&mut mpd_song)))
        .collect()
    )
}

pub fn fetch_last_n_songs(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    n: u32,
) {
    let to_fetch: Vec<(String, OffsetDateTime)> =
        sqlite::get_last_n_songs(n).expect("Sqlite DB error");
    match fetch_songs_by_uri(
        client,
        &to_fetch
            .iter()
            .map(|tup| tup.0.as_str())
            .collect::<Vec<&str>>(),
    ) {
        Ok(raw_songs) => {
            let songs: Vec<SongInfo> = raw_songs
                .into_iter()
                .zip(
                    to_fetch
                        .iter()
                        .map(|r| r.1)
                        .collect::<Vec<OffsetDateTime>>(),
                )
                .map(|mut tup| {
                    tup.0.last_played = Some(tup.1);
                    std::mem::take(&mut tup.0)
                })
                .collect();

            if !songs.is_empty() {
                let _ =
                    sender_to_fg.send_blocking(AsyncClientMessage::RecentSongInfoDownloaded(songs));
            }
        }
        Err(error) => {
            // Connection error => attempt to reconnect
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(error, None));
        }
    }
}

pub fn play_at(
    client: &mut mpd::Client<stream::StreamWrapper>,
    id_or_pos: u32,
    is_id: bool
) -> Result<(), MpdError> {
    if is_id {
        client.switch(Id(id_or_pos)).map(|_| ())
    } else {
        client.switch(id_or_pos).map(|_| ())
    }
}

pub fn find_add(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    query: Query<'static>,
    start_playing_pos: Option<u32>
) {
    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(true));
    let mut res = client.findadd(&query);

    if let Some(pos) = start_playing_pos {
        res = res.and_then(|_| {
            play_at(client, pos, false)
        });
    }

    match res {
        Ok(()) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(false));
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, Some(ClientError::Queuing)));
        }
    }
}

pub fn add_multi(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    uris: &[String],
    recursive: bool,
    start_playing_pos: Option<u32>,
    insert_pos: Option<u32>
) {
    if uris.is_empty() {
        return;
    }
    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(true));
    let mut res: Result<(), MpdError> = Ok(());
    if uris.len() > 1 {
        // Batch by batch to avoid holding the server up too long (and timing out)
        let mut inserted: usize = 0;
        while inserted < uris.len() {
            let to_insert = (uris.len() - inserted).min(BATCH_SIZE);
            res = if let Some(pos) = insert_pos {
                client.insert_multiple(&uris[inserted..(inserted + to_insert)], pos as usize + inserted).map(|_| ())
            } else {
                client.push_multiple(&uris[inserted..(inserted + to_insert)]).map(|_| ())
            };
            inserted += to_insert;
            if !res.is_ok() {
                break;
            }
        }
    } else {
        res = if recursive {
            // TODO: support inserting at specific location in queue
            client.findadd(Query::new().and(Term::Base, &uris[0])).map(|_| ())
        } else if let Some(pos) = insert_pos {
            client.insert(&uris[0], pos as usize).map(|_| ())
        } else {
            client.push(&uris[0]).map(|_| ())
        };
    }

    if let Some(pos) = start_playing_pos {
        res = res.and_then(|_| {
            play_at(client, pos, false)
        });
    }

    match res {
        Ok(()) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(false));
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, Some(ClientError::Queuing)));
        }
    }
}

pub fn load_playlist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    name: &str,
    start_playing_pos: Option<u32>
) {
    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(true));

    let mut res = client.load(name, ..);
    if let Some(pos) = start_playing_pos {
        res = res.and_then(|_| {
            play_at(client, pos, false)
        });
    }

    match res {
        Ok(()) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(false));
        }
        Err(MpdError::Io(_)) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::Connect);
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, Some(ClientError::Queuing)));
        }
    }
}

fn resolve_dynamic_playlist_rules(
    client: &mut mpd::Client<stream::StreamWrapper>,
    dp: DynamicPlaylist
) ->Vec<String> {
    // Resolve into concrete URIs.
    // First, separate the search query-based conditions from the sticker ones.
    let mut query_clauses: Vec<(QueryLhs, String)> = Vec::new();
    let mut sticker_clauses: Vec<(String, StickerOperation, String)> = Vec::new();
    for rule in dp.rules.into_iter() {
        match rule {
            Rule::Sticker(key, op, rhs) => {
                sticker_clauses.push((key, op, rhs));
            }
            Rule::Query(lhs, rhs) => {
                query_clauses.push((lhs, rhs));
            }
        }
    }
    let mut res: Option<FxHashSet<String>> = None;
    // Query first 'cuz I feel like it.
    if !query_clauses.is_empty() {
        let mut mpd_query = Query::new();
        for (lhs, rhs) in query_clauses.into_iter() {
            lhs.add_to_query(&mut mpd_query, rhs);
        }
        let mut set = FxHashSet::default();
        fetch_songs_by_query(client, &mpd_query, |batch| {
            for song in batch.into_iter() {
                set.insert(song.uri);
            }
            Ok(())
        });
        res = Some(set);
    }

    // Get matching URIs for each sticker condition
    for clause in sticker_clauses.into_iter() {
        let mut set = FxHashSet::default();
        fetch_uris_by_sticker(
            client, &clause.0, clause.1, &clause.2,
            |batch| {
                for uri in batch.into_iter() {
                    set.insert(uri);
                }
                Ok(())
            }
        );
        if let Some(ref prev_res) = res {
            // TODO: reduce cloning
            let intersection: FxHashSet<String> = prev_res.intersection(&set).map(|s| s.to_owned()).collect();
            if intersection.len() == 0 {
                // Return early
                return Vec::with_capacity(0);
            }
            res.replace(intersection);
        }
    }

    if let Some(res) = res {
        res.into_iter().collect()
    } else {
        Vec::with_capacity(0)
    }
}

pub fn fetch_dynamic_playlist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    dp: DynamicPlaylist,
    fetch_limit: Option<usize>,  // disregarded when queuing
    queue: bool,  // If true, will queue instead of replying with SongInfos
    play: bool
) {
    if queue {
        let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(true));
    }

    // To reduce server & connection burden, temporarily turn off all tags in responses.
    if client.tagtypes_clear().is_ok() {
        let name = dp.name.to_owned();
        let uris = resolve_dynamic_playlist_rules(client, dp);
        client.tagtypes_all().expect("Cannot restore tagtypes");
        if queue {
            add_multi(client, sender_to_fg, &uris, false, if play {Some(0)} else {None}, None);
        } else {
            let mut curr_len: usize = 0;
            match fetch_songs_by_uri(
                client,
                uris[..(if let Some(limit) = fetch_limit {limit.min(uris.len())} else {uris.len()})]
                    .iter().map(AsRef::as_ref).collect::<Vec<&str>>().as_slice()
            ) {
                Ok(songs) => {
                    let songs_len = songs.len();
                    while curr_len < songs_len {
                        let next_len = (curr_len + BATCH_SIZE).min(songs_len);
                        let _ = sender_to_fg.send_blocking(
                            AsyncClientMessage::DynamicPlaylistSongInfoDownloaded(name.clone(), songs[curr_len..next_len].to_vec())
                        );
                        curr_len = next_len;
                    }
                }
                Err(e) => {
                    dbg!(e);
                }
            }
        }
    }
}
