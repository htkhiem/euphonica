use std::{
    borrow::Cow, cmp::Ordering as StdOrdering, hash::BuildHasherDefault, i64, num::NonZero, ops::Range, sync::Mutex
};
use chrono::{DateTime, Duration, Local};

use async_channel::{SendError, Sender};
use gio::prelude::SettingsExt;
use gtk::gdk;
use lru::LruCache;
use nohash_hasher::NoHashHasher;
use once_cell::sync::Lazy;
use rand::seq::SliceRandom;
use time::OffsetDateTime;

use mpd::{
    error::{Error as MpdError, ErrorCode},
    search::{Operation as QueryOperation, Query, Term, Window}, Id,
};
use rustc_hash::FxHashSet;

use crate::{cache::{get_new_image_paths, sqlite}, common::{SongInfo, dynamic_playlist::{Ordering, QueryLhs, Rule, StickerObjectType, StickerOperation}, parse_mb_artist_tag}, meta_providers::ProviderMessage, utils::{self, strip_filename_linux}};

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
                            } else if let Ok(mut songs) = client.songs(change.id) {
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
        .map_or(None, utils::read_image_from_bytes)
    {
        let (hires, thumb) = utils::resize_convert_image(dyn_img);
        let (path, thumbnail_path) = get_new_image_paths();
        hires
            .save(&path)
            .unwrap_or_else(|_| panic!("Couldn't save downloaded cover to {:?}", &path));
        thumb.save(&thumbnail_path).unwrap_or_else(|_| panic!("Couldn't save downloaded thumbnail cover to {:?}",
            &thumbnail_path));
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
        .map_or(None, utils::read_image_from_bytes)
    {
        let (hires, thumb) = utils::resize_convert_image(dyn_img);
        let (path, thumbnail_path) = get_new_image_paths();
        hires
            .save(&path)
            .unwrap_or_else(|_| panic!("Couldn't save downloaded cover to {:?}", &path));
        thumb.save(&thumbnail_path).unwrap_or_else(|_| panic!("Couldn't save downloaded thumbnail cover to {:?}",
            &thumbnail_path));
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
    } else if folder_path.as_ref().is_some_and(|p| !p.is_empty()) {
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
            } else if sqlite_path.as_ref().is_some_and(|p| !p.is_empty()) {
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
    mut respond: F,
) -> Result<(), MpdError>
where
    F: FnMut(AlbumInfo) -> Result<(), SendError<AsyncClientMessage>>,
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
                                let _ = respond(
                                    SongInfo
                                        ::from(std::mem::take(&mut songs[0]))
                                        .into_album_info()
                                        .unwrap_or_default()
                                );
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
                more = false;
            }
        }
    }
}

fn fetch_uris_by_sticker<F>(
    client: &mut mpd::Client<stream::StreamWrapper>,
    obj: StickerObjectType,
    sticker: &str,
    op: StickerOperation,
    rhs: &str,
    only_in: Option<&str>,
    mut respond: F,
) where
    F: FnMut(Vec<String>) -> Result<(), SendError<AsyncClientMessage>>,
{
    let mut curr_len: usize = 0;
    let mut more: bool = true;
    while more && (curr_len) < FETCH_LIMIT {
        match client.find_sticker_op(
            obj.to_str(), only_in.unwrap_or(""), sticker, op.to_mpd_syntax(), rhs,
            Window::from((curr_len as u32, (curr_len + BATCH_SIZE) as u32))
        ) {
            Ok(names) => {
                if !names.is_empty() {
                    // If not searching directly by song (for example by album rating), further resolve to URI.
                    match obj {
                        StickerObjectType::Song => {
                            // In this case the names are the URIs themselves
                            let _ = respond(names);
                            curr_len += BATCH_SIZE;
                        }
                        StickerObjectType::Playlist => {
                            // Fetch playlist contents
                            for playlist_name in names.iter() {
                                fetch_playlist_songs_internal(
                                    client,
                                    playlist_name,
                                    |batch| {
                                        let _ = respond(batch.into_iter().map(|song| song.uri).collect());
                                    },
                                    |_| {}
                                );
                            }
                        }
                        tag_type => {
                            let tag_type_str = tag_type.to_str();
                            // Fetch all songs for each tag
                            for tag_value in names.iter() {
                                let mut query = Query::new();
                                query.and(Term::Tag(Cow::Borrowed(tag_type_str)), tag_value);
                                fetch_songs_by_query(
                                    client,
                                    &query,
                                    |batch| {
                                        respond(batch.into_iter().map(|song| song.uri).collect())
                                    }
                                );
                            }
                        }
                    }
                    curr_len += BATCH_SIZE;
                } else {
                    more = false;
                }
            }
            Err(e) => {
                dbg!(e);
                more = false;
            }
        }
    }
}

/// Fetch all albums, using AlbumArtist to further disambiguate same-named ones.
pub fn fetch_all_albums(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    if let Err(mpd_error) = fetch_albums_by_query(client, &Query::new(), |info| {
        sender_to_fg.send_blocking(AsyncClientMessage::AlbumBasicInfoDownloaded(info))
    }) {
        let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
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
        if let Err(mpd_error) = fetch_albums_by_query(client, &query, |info| {
            sender_to_fg.send_blocking(AsyncClientMessage::RecentAlbumDownloaded(info))
        }) {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    }
}

pub fn fetch_albums_of_artist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    artist_name: String,
) {
    // fetch_albums_by_query uses substring search & may lead to false positives
    // so we need to use a bespoke version here.
    // First use substring search to get unique artist tags, not albumartists
    // as we want to show all albums containing at least one song with this
    // artist tagged.
    match client.list(
        &Term::Tag(Cow::Borrowed("artist")),
        Query::new().and_with_op(
            Term::Tag(Cow::Borrowed("artist")),
            QueryOperation::Contains,
            artist_name.clone(),
        ),
        None
    ) {
        Ok(grouped_vals) => {
            let mut already_parsed: FxHashSet<String> = FxHashSet::default();
            for tag in grouped_vals.groups[0].1.iter() {
                // If this artist tag really contains this artist and not a coincidental
                // substring, then fetch all albums containing songs with this artist tag.
                if parse_mb_artist_tag(tag).iter().any(|a| a == &artist_name) {
                    if let Err(mpd_error) = fetch_albums_by_query(
                        client,
                        Query::new().and(Term::Tag("artist".into()), tag),
                        |info| {
                            if already_parsed.insert(info.title.clone()) {
                                let _ = sender_to_fg.send_blocking(AsyncClientMessage::ArtistAlbumBasicInfoDownloaded(
                                    artist_name.clone(),
                                    info
                                ));
                            }
                            Ok(())
                        },
                    ) {
                        let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
                    }
                }
            }
        }
        Err(mpd_error) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
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
                        if recent_names_set.contains(&artist.name)
                            && already_parsed.insert(artist.name.clone()) {
                                res.push(artist);
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
                // Substring search may lead to false positives so filter once more.
                songs.into_iter().filter(|s| s.artists.iter().any(|a| a.name == name)).collect()
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

fn fetch_playlist_songs_internal<G: Fn(MpdError), F: FnMut(Vec<SongInfo>)> (
    client: &mut mpd::Client<stream::StreamWrapper>,
    name: &str,
    mut respond: F,
    on_error: G
) {
    if client.version.1 < 24 {
        match client.playlist(name, Option::<Range<u32>>::None) {
            Ok(mut mpd_songs) => {
                let songs: Vec<SongInfo> = mpd_songs
                    .iter_mut()
                    .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                    .collect();
                if !songs.is_empty() {
                    respond(songs);
                }
            }
            Err(mpd_error) => {
                on_error(mpd_error);
            }
        }
    } else {
        // For MPD 0.24+, use the new paged loading
        let mut curr_len: u32 = 0;
        let mut more: bool = true;
        while more && (curr_len as usize) < FETCH_LIMIT {
            match client.playlist(name, Some(curr_len..(curr_len + BATCH_SIZE as u32))) {
                Ok(mut mpd_songs) => {
                    let songs: Vec<SongInfo> = mpd_songs
                        .iter_mut()
                        .map(|mpd_song| SongInfo::from(std::mem::take(mpd_song)))
                        .collect();
                    more = songs.len() >= BATCH_SIZE;
                    if !songs.is_empty() {
                        curr_len += songs.len() as u32;
                        respond(songs);
                    }
                }
                Err(mpd_error) => {
                    on_error(mpd_error);
                }
            }
        }
    }
}

pub fn fetch_playlist_songs(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    name: String,
) {
    fetch_playlist_songs_internal(
        client,
        &name,
        |songs| {
            let _ = sender_to_fg.send_blocking(
                AsyncClientMessage::PlaylistSongInfoDownloaded(name.clone(), songs),
            );
        },
        |mpd_error| {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    );
}

pub fn fetch_songs_by_uri(
    client: &mut mpd::Client<stream::StreamWrapper>,
    uris: &[&str],
    fetch_stickers: bool
) -> Result<Vec<(SongInfo, Option<Stickers>)>, MpdError> {
    let mut res: Vec<(SongInfo, Option<Stickers>)> = Vec::with_capacity(uris.len());
    for uri in uris.iter() {
        match client.find(Query::new().and(Term::File, *uri), None) {
            Ok(mut found_songs) => {
                if !found_songs.is_empty() {
                    let song = SongInfo::from(std::mem::take(&mut found_songs[0]));
                    if fetch_stickers {
                        // Assume stickers are supported as all paths that call this function
                        // are only accessible via UI when that's the case.
                        res.push((
                            song, client.stickers("song", uri).ok().map(Stickers::from_mpd_kv)
                        ));
                    } else {
                        res.push((song, None));
                    }
                }
            }
            Err(mpd_error) => {
                return Err(mpd_error);
            }
        }
    }

    Ok(res)
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
        false
    ) {
        Ok(raw_songs) => {
            let songs: Vec<SongInfo> = raw_songs
                .into_iter()
                .map(|pair| pair.0)
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
            if res.is_err() {
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

fn get_past_unix_timestamp(backoff: i64) -> i64 {
    let current_local_dt: DateTime<Local> = Local::now();
    let backoff_dur: Duration = Duration::seconds(backoff);
    current_local_dt.checked_sub_signed(backoff_dur).unwrap().timestamp()
}

fn resolve_dynamic_playlist_rules(
    client: &mut mpd::Client<stream::StreamWrapper>,
    rules: Vec<Rule>
) ->Vec<String> {
    // Resolve into concrete URIs.
    // First, separate the search query-based conditions from the sticker ones.
    let mut query_clauses: Vec<(QueryLhs, String)> = Vec::new();
    let mut sticker_clauses: Vec<(StickerObjectType, String, StickerOperation, String)> = Vec::new();
    for rule in rules.into_iter() {
        println!("{rule:?}");
        match rule {
            Rule::Sticker(obj, key, op, rhs) => {
                sticker_clauses.push((obj, key, op, rhs));
            }
            Rule::Query(lhs, rhs) => {
                query_clauses.push((lhs, rhs));
            }
            Rule::LastModified(secs) => {
                // Special case: query current system datetime
                query_clauses.push((QueryLhs::LastMod, get_past_unix_timestamp(secs).to_string()));
            }
        }
    }
    let mut res: FxHashSet<String> = FxHashSet::default();
    let mut mpd_query = Query::new();
    if !query_clauses.is_empty() {
        for (lhs, rhs) in query_clauses.into_iter() {
            lhs.add_to_query(&mut mpd_query, rhs);
        }
    } else {
        // Dummy term that basically matches everything.
        mpd_query.and(Term::AddedSince, i64::MIN.to_string());
    }
    fetch_songs_by_query(client, &mpd_query, |batch| {
        for song in batch.into_iter() {
            res.insert(song.uri);
        }
        Ok(())
    });
    println!("Length after query_clauses: {}", res.len());

    // Get matching URIs for each sticker condition
    // TODO: Optimise sticker operations by limiting to any found URI query clause.
    for clause in sticker_clauses.into_iter() {
        let mut set = FxHashSet::default();
        match clause.1.as_str() {
            Stickers::LAST_PLAYED_KEY | Stickers::LAST_SKIPPED_KEY => {
                // Special case: treat RHS as relative to current time
                fetch_uris_by_sticker(
                    client,
                    clause.0,
                    &clause.1,
                    clause.2,
                    &get_past_unix_timestamp(clause.3.parse::<i64>().unwrap()).to_string(),
                    None,
                    |batch| {
                        for uri in batch.into_iter() {
                            set.insert(uri);
                        }
                        Ok(())
                    }
                );
            }
            _ => {
                fetch_uris_by_sticker(
                    client,
                    clause.0,
                    &clause.1,
                    clause.2,
                    &clause.3,
                    None,
                    |batch| {
                        for uri in batch.into_iter() {
                            set.insert(uri);
                        }
                        Ok(())
                    }
                );
            }
        }

        println!("Length of matches of sticker_clause: {}", set.len());
        res.retain(move |elem| {set.contains(elem)});
        if res.is_empty() {
            // Return early
            return Vec::with_capacity(0);
        }
        println!("Length afterwards: {}", res.len());
    }

    res.into_iter().collect()
}

fn cmp_options_nulls_last<T: Ord>(
    a: Option<&T>,
    b: Option<&T>
) -> StdOrdering {
    match (a, b) {
        (Some(val_a), Some(val_b)) => {
            val_a.cmp(val_b)
        }
        (Some(_), None) => StdOrdering::Less,
        (None, Some(_)) => StdOrdering::Greater,
        (None, None) => StdOrdering::Equal,
    }
}

// Reverse comparison, but still putting nulls last
fn reverse_cmp_options_nulls_last<T: Ord>(
    a: Option<&T>,
    b: Option<&T>
) -> StdOrdering {
    match (a, b) {
        (Some(val_a), Some(val_b)) => {
            val_a.cmp(val_b).reverse()
        }
        (Some(_), None) => StdOrdering::Less,
        (None, Some(_)) => StdOrdering::Greater,
        (None, None) => StdOrdering::Equal,
    }
}

/// Build and return a dynamic comparator closure.
///
/// This is highly efficient because the logic for choosing which fields to compare
/// is determined *once* when this function is called.
pub fn build_comparator(orderings: &[Ordering]) -> Box<dyn Fn(&(SongInfo, Stickers), &(SongInfo, Stickers)) -> StdOrdering> {
    let orderings = orderings.to_vec();
    Box::new(move |a: &(SongInfo, Stickers), b: &(SongInfo, Stickers)| -> StdOrdering {
        let song_a = &a.0;
        let stickers_a = &a.1;
        let song_b = &b.0;
        let stickers_b = &b.1;
        for ordering in &orderings {
            // Determine the ordering for the current rule's field.
            // Nulls are always sorted last as it wouldn't really make sense otherwise in
            // the dynamic playlist/all songs view cases.
            let res = match *ordering {
                Ordering::AscAlbumTitle => {
                    cmp_options_nulls_last(
                        song_a.album.as_ref().map(|album: &AlbumInfo| &album.title),
                        song_b.album.as_ref().map(|album: &AlbumInfo| &album.title)
                    )
                }
                Ordering::DescAlbumTitle => {
                    reverse_cmp_options_nulls_last(
                        song_a.album.as_ref().map(|album: &AlbumInfo| &album.title),
                        song_b.album.as_ref().map(|album: &AlbumInfo| &album.title)
                    )
                }
                Ordering::Track => {
                    // Since nulls are -1, replace them with i64::MAX instead.
                    let mut track_a = song_a.track.get();
                    if track_a < 0 {
                        track_a = i64::MAX;
                    }
                    let mut track_b = song_b.track.get();
                    if track_b < 0 {
                        track_b = i64::MAX;
                    }
                    track_a.cmp(&track_b)
                }
                Ordering::AscReleaseDate => {
                    cmp_options_nulls_last(
                        song_a.release_date.as_ref(),
                        song_b.release_date.as_ref()
                    )
                }
                Ordering::DescReleaseDate => {
                    reverse_cmp_options_nulls_last(
                        song_a.release_date.as_ref(),
                        song_b.release_date.as_ref()
                    )
                }
                Ordering::AscArtistTag => {
                    cmp_options_nulls_last(
                        song_a.artist_tag.as_ref(),
                        song_b.artist_tag.as_ref(),
                    )
                }
                Ordering::DescArtistTag => {
                    reverse_cmp_options_nulls_last(
                        song_a.artist_tag.as_ref(),
                        song_b.artist_tag.as_ref(),
                    )
                }
                Ordering::AscRating => {
                    cmp_options_nulls_last(
                        stickers_a.rating.as_ref(),
                        stickers_b.rating.as_ref(),
                    )
                }
                Ordering::DescRating => {
                    reverse_cmp_options_nulls_last(
                        stickers_a.rating.as_ref(),
                        stickers_b.rating.as_ref(),
                    )
                }
                Ordering::AscLastModified => {
                    cmp_options_nulls_last(
                        song_a.last_modified.as_ref(),
                        song_b.last_modified.as_ref(),
                    )
                }
                Ordering::DescLastModified => {
                    reverse_cmp_options_nulls_last(
                        song_a.last_modified.as_ref(),
                        song_b.last_modified.as_ref(),
                    )
                }
                Ordering::AscPlayCount => {
                    cmp_options_nulls_last(
                        stickers_a.play_count.as_ref(),
                        stickers_b.play_count.as_ref(),
                    )
                }
                Ordering::DescPlayCount => {
                    reverse_cmp_options_nulls_last(
                        stickers_a.play_count.as_ref(),
                        stickers_b.play_count.as_ref(),
                    )
                }
                Ordering::AscSkipCount => {
                    cmp_options_nulls_last(
                        stickers_a.skip_count.as_ref(),
                        stickers_b.skip_count.as_ref(),
                    )
                }
                Ordering::DescSkipCount => {
                    reverse_cmp_options_nulls_last(
                        stickers_a.skip_count.as_ref(),
                        stickers_b.skip_count.as_ref(),
                    )
                }
                Ordering::Random => unreachable!()
            };

            if res != StdOrdering::Equal {
                return res;
            }
            // If equal, fall through to next rule
        }

        // If all rules resulted in equality, the items are considered equal.
        std::cmp::Ordering::Equal
    })
}


pub fn fetch_dynamic_playlist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    dp: DynamicPlaylist,
    cache: bool  // If true, will cache resolved song URIs locally
) {
    // To reduce server & connection burden, temporarily turn off all tags in responses.
    if client.tagtypes_clear().is_ok() {
        let name = dp.name.to_owned();
        // First, fetch just the URIs, without any sorting
        let uris = resolve_dynamic_playlist_rules(client, dp.rules);

        // Then, fetch the tags and stickers needed for display and sorting.
        // These three are always needed for display.
        let mut tagtypes: Vec<&'static str> = vec!["title", "album", "artist", "albumartist"];
        for ordering in dp.ordering.iter() {
            match ordering {
                Ordering::Track => {tagtypes.push("track");},
                Ordering::AscReleaseDate | Ordering::DescReleaseDate => {tagtypes.push("originaldate");}
                _ => {
                    // the rest are either Random, always included (LastModified), or stickers-based
                }
            }
        }
        client.tagtypes_enable(tagtypes).expect("Unable to enable the needed tag types to order the dynamic playlist");
        if let Ok(mut songs_stickers) = fetch_songs_by_uri(
            client,
            &uris.iter().map(String::as_str).collect::<Vec<&str>>(),
            true
        ).map(|raw| raw.into_iter().map(|t| (t.0, t.1.unwrap())).collect::<Vec<(SongInfo, Stickers)>>()) {
            if !songs_stickers.is_empty() {
                // Sort the song list now
                if dp.ordering.len() == 1 && dp.ordering[0] == Ordering::Random {
                    let mut rng = rand::rng();
                    songs_stickers.shuffle(&mut rng);
                } else {
                    let cmp_func = build_comparator(&dp.ordering);
                    songs_stickers.sort_by(cmp_func);
                }
                if let Some(limit) = dp.limit {
                    songs_stickers.truncate(limit as usize);
                }
                let songs: Vec<SongInfo> = songs_stickers.into_iter().map(|tup| tup.0).collect();
                if cache {
                    if let Err(db_err) = sqlite::cache_dynamic_playlist_results(&dp.name, &songs) {
                        println!("Failed to cache DP query result. Queuing will be incorrect!");
                        dbg!(db_err);
                    }
                }

                let mut curr_len: usize = 0;
                let songs_len = songs.len();
                while curr_len < songs_len {
                    let next_len = (curr_len + BATCH_SIZE).min(songs_len);
                    let _ = sender_to_fg.send_blocking(
                        AsyncClientMessage::DynamicPlaylistSongInfoDownloaded(name.clone(), songs[curr_len..next_len].to_vec())
                    );
                    curr_len = next_len;
                }
            }
            // Send once more w/ an empty list to signal end-of-result
            println!("Sending end-of-response");
            let _ = sender_to_fg.send_blocking(
                AsyncClientMessage::DynamicPlaylistSongInfoDownloaded(name.clone(), Vec::new())
            );
        }
        client.tagtypes_all().expect("Cannot restore tagtypes");
    }
}

pub fn fetch_dynamic_playlist_cached(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    name: &str
) {
    if let Some(songs_stickers) = sqlite::get_cached_dynamic_playlist_results(name).ok().and_then(|uris| fetch_songs_by_uri(
            client,
            &uris.iter().map(String::as_str).collect::<Vec<&str>>(),
            false
        ).ok()) {
        let songs: Vec<SongInfo> = songs_stickers.into_iter().map(|tup| tup.0).collect();
        let mut curr_len: usize = 0;
        let songs_len = songs.len();
        while curr_len < songs_len {
            let next_len = (curr_len + BATCH_SIZE).min(songs_len);
            let _ = sender_to_fg.send_blocking(
                AsyncClientMessage::DynamicPlaylistSongInfoDownloaded(name.to_string(), songs[curr_len..next_len].to_vec())
            );
            curr_len = next_len;
        }

        // Send once more w/ an empty list to signal end-of-result
        println!("Sending end-of-response");
        let _ = sender_to_fg.send_blocking(
            AsyncClientMessage::DynamicPlaylistSongInfoDownloaded(name.to_string(), Vec::new())
        );
    }
}

pub fn queue_cached_dynamic_playlist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    name: &str,
    play: bool
) {
    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(true));
    if let Ok(uris) = sqlite::get_cached_dynamic_playlist_results(name) {
        add_multi(
            client,
            sender_to_fg,
            &uris,
            false,
            if play {Some(0)} else {None},
            None
        );
    }

    let _ = sender_to_fg.send_blocking(AsyncClientMessage::Queuing(false));
}
