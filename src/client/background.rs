use chrono::{DateTime, Duration, Local};
use std::{
    borrow::Cow, cmp::Ordering as StdOrdering, hash::BuildHasherDefault, i64, num::NonZero,
    ops::Range, sync::Mutex,
};

use async_channel::{SendError, Sender};
use gio::prelude::SettingsExt;
use gtk::gdk;
use lru::LruCache;
use nohash_hasher::NoHashHasher;
use once_cell::sync::Lazy;
use rand::seq::SliceRandom;
use time::OffsetDateTime;

use mpd::{
    Id,
    error::{Error as MpdError, ErrorCode},
    search::{Operation as QueryOperation, Query, Term, Window},
};
use rustc_hash::FxHashSet;

use crate::{
    cache::{get_new_image_paths, sqlite},
    common::{
        SongInfo,
        dynamic_playlist::{Ordering, QueryLhs, Rule, StickerObjectType, StickerOperation},
    },
    meta_providers::ProviderMessage,
    utils::{self, strip_filename_linux},
};

use super::*;



pub fn update_mpd_database(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
) {
    match client.update() {
        Ok(_) => {
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::DBUpdated);
        }
        Err(mpd_error) => {
            let _ =
                sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
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
        thumb.save(&thumbnail_path).unwrap_or_else(|_| {
            panic!(
                "Couldn't save downloaded thumbnail cover to {:?}",
                &thumbnail_path
            )
        });
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
            Some(
                thumbnail_path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
            ),
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
        thumb.save(&thumbnail_path).unwrap_or_else(|_| {
            panic!(
                "Couldn't save downloaded thumbnail cover to {:?}",
                &thumbnail_path
            )
        });
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
            Some(
                thumbnail_path
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string(),
            ),
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
        Err(mpd_error) => Err(mpd_error),
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
            let _ =
                sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    }
}

pub fn fetch_albums_of_artist(
    client: &mut mpd::Client<stream::StreamWrapper>,
    sender_to_fg: &Sender<AsyncClientMessage>,
    artist_name: String,
) {
    if let Err(mpd_error) = fetch_albums_by_query(
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
        let _ = sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
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
                            && already_parsed.insert(artist.name.clone())
                        {
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
            let _ =
                sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        }
    }
}

fn fetch_playlist_songs_internal<G: Fn(MpdError), F: FnMut(Vec<SongInfo>)>(
    client: &mut mpd::Client<stream::StreamWrapper>,
    name: &str,
    mut respond: F,
    on_error: G,
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
            let _ = sender_to_fg.send_blocking(AsyncClientMessage::PlaylistSongInfoDownloaded(
                name.clone(),
                songs,
            ));
        },
        |mpd_error| {
            let _ =
                sender_to_fg.send_blocking(AsyncClientMessage::BackgroundError(mpd_error, None));
        },
    );
}

pub fn fetch_songs_by_uri(
    client: &mut mpd::Client<stream::StreamWrapper>,
    uris: &[&str],
    fetch_stickers: bool,
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
                            song,
                            client.stickers("song", uri).ok().map(Stickers::from_mpd_kv),
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
        false,
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
    is_id: bool,
) -> Result<(), MpdError> {
    if is_id {
        client.switch(Id(id_or_pos)).map(|_| ())
    } else {
        client.switch(id_or_pos).map(|_| ())
    }
}

fn get_past_unix_timestamp(backoff: i64) -> i64 {
    let current_local_dt: DateTime<Local> = Local::now();
    let backoff_dur: Duration = Duration::seconds(backoff);
    current_local_dt
        .checked_sub_signed(backoff_dur)
        .unwrap()
        .timestamp()
}

fn resolve_dynamic_playlist_rules(
    client: &mut mpd::Client<stream::StreamWrapper>,
    rules: Vec<Rule>,
) -> Vec<String> {
    // Resolve into concrete URIs.
    // First, separate the search query-based conditions from the sticker ones.
    let mut query_clauses: Vec<(QueryLhs, String)> = Vec::new();
    let mut sticker_clauses: Vec<(StickerObjectType, String, StickerOperation, String)> =
        Vec::new();
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
                    },
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
                    },
                );
            }
        }

        println!("Length of matches of sticker_clause: {}", set.len());
        res.retain(move |elem| set.contains(elem));
        if res.is_empty() {
            // Return early
            return Vec::with_capacity(0);
        }
        println!("Length afterwards: {}", res.len());
    }

    res.into_iter().collect()
}
