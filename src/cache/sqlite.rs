extern crate bson;
extern crate rusqlite;

use std::{io::Cursor, str::FromStr};

use glib::{ThreadHandle, ThreadPool};
use once_cell::sync::Lazy;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Error as SqliteError, OptionalExtension, Result, Row, params};
use time::OffsetDateTime;

use crate::{
    common::{
        AlbumInfo, ArtistInfo, DynamicPlaylist, INodeType, SongInfo,
        dynamic_playlist::{AutoRefresh, Ordering, Rule},
        inode::INodeInfo,
    },
    meta_providers::models::{AlbumMeta, ArtistMeta, Lyrics, LyricsParseError},
    utils::{format_datetime_local_tz, strip_filename_linux},
};

use super::controller::get_doc_cache_path;

// Limit writes to a single thread to avoid DatabaseBusy races.
// Thread will be parked when idle.
static SQLITE_WRITE_THREADPOOL: Lazy<glib::ThreadPool> =
    Lazy::new(|| ThreadPool::shared(Some(1)).expect("Failed to spawn Sqlite write threadpool"));
static SQLITE_POOL: Lazy<r2d2::Pool<SqliteConnectionManager>> = Lazy::new(|| {
    let manager = SqliteConnectionManager::file(get_doc_cache_path());
    let pool = r2d2::Pool::new(manager).unwrap();
    let conn = pool.get().unwrap();
    // Init schema & indices
    // Migrations
    loop {
        let user_version = conn
            .prepare("pragma user_version")
            .unwrap()
            .query_row([], |r| Ok(r.get::<usize, i32>(0)))
            .unwrap()
            .unwrap();

        println!("Local metadata DB version: {user_version}");
        match user_version {
            4 => {
                break;
            }
            3 => {
                conn.execute_batch(
                    "create table if not exists `queries` (
    `name` VARCHAR not null,
    `cover_name` VARCHAR null,
    `last_modified` DATETIME not null,
    `last_queued` DATETIME null,
    `play_count` INTEGER not null,
    `bson` BLOB not null,
    `last_refresh` DATETIME null,
    `auto_refresh` VARCHAR not null,
    `limit` INTEGER null,
    primary key(`name`)
);
create unique index if not exists `query_key` on `queries` (
    `name`
);

create table if not exists `query_results` (
    `query_name` VARCHAR not null,
    `uri` VARCHAR not null
);
create index if not exists `query_results_key` on `query_results` (
    `query_name`
);

pragma user_version = 4;",
                )
                .expect("Unable to migrate DB version 3 to 4");
            }
            2 => {
                conn.execute_batch(
                    "alter table albums_history add column mbid varchar null;
alter table albums_history add column artist varchar null;
pragma user_version = 3;
",
                )
                .expect("Unable to migrate DB version 2 to 3");
            }
            1 => {
                conn.execute_batch(
                    "pragma journal_mode=WAL;
pragma user_version = 2;",
                )
                .expect("Unable to migrate DB version 1 to 2");
            }
            0 => {
                // Check if we're starting from nothing
                match conn.query_row(
                    "select name from sqlite_master where type='table' and name='albums'",
                    [],
                    |row| row.get::<usize, String>(0),
                ) {
                    Ok(_) => {
                        println!("Upgrading local metadata DB to version 1...");
                        // Migrate album table schema: album table now accepts non-unique folder URIs
                        conn.execute_batch(
                            "begin;
-- SQLite doesn't allow dropping a constraint so, so we'll have to recreate the table
alter table albums rename to old_albums;
-- Note: the 'folder_uri' no longer has the primary key or unique constraint here.
create table if not exists `albums` (
    `folder_uri` varchar not null,
    `mbid` varchar null unique,
    `title` varchar not null,
    `artist` varchar null,
    `last_modified` datetime not null,
    `data` blob not null
);

-- Copy
insert into albums (folder_uri, mbid, title, artist, last_modified, data)
select folder_uri, mbid, title, artist, last_modified, data
from old_albums;

-- Drop old table and both indices
drop table old_albums;
drop index if exists album_mbid;
drop index if exists album_name;

-- Reindex
create unique index if not exists `album_mbid` on `albums` (
    `mbid`
);
create unique index if not exists `album_name` on `albums` (
    `title`, `artist`
);

-- Create new tables
create table if not exists `songs_history` (
    `id` INTEGER not null,
    `uri` VARCHAR not null,
    `timestamp` DATETIME not null,
    primary key(`id`)
);
create index if not exists `song_history_last` on `songs_history` (`uri`, `timestamp` desc);

create table if not exists `artists_history` (
    `id` INTEGER not null,
    `name` VARCHAR not null,
    `timestamp` DATETIME not null,
    primary key(`id`)
);
create index if not exists `artists_history_last` on `artists_history` (`name`, `timestamp` desc);

create table if not exists `albums_history` (
    `id` INTEGER not null,
    `title` VARCHAR not null,
    `timestamp` DATETIME not null,
    primary key(`id`)
);
create index if not exists `albums_history_last` on `albums_history` (`title`, `timestamp` desc);

create table if not exists `images` (
    `key` VARCHAR not null,
    `is_thumbnail` INTEGER not null,
    `filename` VARCHAR not null,
    `last_modified` DATETIME not null,
    primary key (`key`, `is_thumbnail`)
);
create unique index if not exists `image_key` on `images` (
    `key`,
    `is_thumbnail`
);

pragma user_version = 1;
end;",
                        )
                        .expect("Unable to migrate DB version 0 to 1");
                    }
                    Err(SqliteError::QueryReturnedNoRows) => {
                        // Starting from scratch
                        println!("Initialising local metadata DB...");
                        conn.execute_batch(
                            "begin;
create table if not exists `albums` (
    `folder_uri` VARCHAR not null,
    `mbid` VARCHAR null unique,
    `title` VARCHAR not null,
    `artist` VARCHAR null,
    `last_modified` DATETIME not null,
    `data` BLOB not null
);
create unique index if not exists `album_mbid` on `albums` (
    `mbid`
);
create unique index if not exists `album_name` on `albums` (
    `title`, `artist`
);

create table if not exists `artists` (
    `name` VARCHAR not null unique,
    `mbid` VARCHAR null unique,
    `last_modified` DATETIME not null,
    `data` BLOB not null,
    primary key (`name`)
);
create unique index if not exists `artist_mbid` on `artists` (
    `mbid`
);
create unique index if not exists `artist_name` on `artists` (`name`);

create table if not exists `songs` (
    `uri` VARCHAR not null unique,
    `lyrics` VARCHAR not null,
    `synced` BOOL not null,
    `last_modified` DATETIME not null,
    primary key(`uri`)
);
create unique index if not exists `song_uri` on `songs` (`uri`);

create table if not exists `songs_history` (
    `id` INTEGER not null,
    `uri` VARCHAR not null,
    `timestamp` DATETIME not null,
    primary key(`id`)
);
create index if not exists `song_history_last` on `songs_history` (`uri`, `timestamp` desc);

create table if not exists `artists_history` (
    `id` INTEGER not null,
    `name` VARCHAR not null,
    `timestamp` DATETIME not null,
    primary key(`id`)
);
create index if not exists `artists_history_last` on `artists_history` (`name`, `timestamp` desc);

create table if not exists `albums_history` (
    `id` INTEGER not null,
    `title` VARCHAR not null,
    `mbid` VARCHAR null,
    `artist` VARCHAR null,
    `timestamp` DATETIME not null,
    primary key(`id`)
);
create index if not exists `albums_history_last` on `albums_history` (`title`, `timestamp` desc);

create table if not exists `images` (
    `key` VARCHAR not null,
    `is_thumbnail` INTEGER not null,
    `filename` VARCHAR not null,
    `last_modified` DATETIME not null,
    primary key (`key`, `is_thumbnail`)
);
create unique index if not exists `image_key` on `images` (
    `key`,
    `is_thumbnail`
);

create table if not exists `queries` (
    `name` VARCHAR not null,
    `cover_name` VARCHAR null,
    `last_modified` DATETIME not null,
    `last_queued` DATETIME null,
    `play_count` INTEGER not null,
    `bson` BLOB not null,
    `last_refresh` DATETIME null,
    `auto_refresh` VARCHAR not null,
    `limit` INTEGER null,
    primary key(`name`)
);
create unique index if not exists `query_key` on `queries` (
    `name`
);

create table if not exists `query_results` (
    `query_name` VARCHAR not null,
    `uri` VARCHAR not null
);
create index if not exists `query_results_key` on `query_results` (
    `query_name`
);

pragma journal_mode=WAL;
pragma user_version = 4;
end;
",
                        )
                        .expect("Unable to init metadata SQLite DB");
                    }
                    e => {
                        panic!("SQLite database error: {e:?}");
                    }
                }
            }
            _ => {}
        }
    }

    pool
});

#[derive(Debug)]
pub enum Error {
    BytesToDocError,
    DocToObjectError,
    ObjectToDocError(bson::error::Error),
    DocToBytesError(bson::error::Error),
    DbError(SqliteError),
    InsufficientKey,
    KeyAlreadyExists,
}

impl From<SqliteError> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::DbError(value)
    }
}

pub struct AlbumMetaRow {
    // folder_uri: String,
    // mbid: Option<String>,
    // title: String,
    // artist: Option<String>,
    // last_modified: OffsetDateTime,
    data: Vec<u8>, // BSON
}

impl TryInto<AlbumMeta> for AlbumMetaRow {
    type Error = Error;
    fn try_into(self) -> Result<AlbumMeta, Self::Error> {
        let mut reader = Cursor::new(self.data);
        bson::deserialize_from_document(
            bson::Document::from_reader(&mut reader).map_err(|_| Error::BytesToDocError)?,
        )
        .map_err(|_| Error::DocToObjectError)
    }
}

impl TryFrom<&Row<'_>> for AlbumMetaRow {
    type Error = SqliteError;
    fn try_from(row: &Row) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            // folder_uri: row.get(0)?,
            // mbid: row.get(1)?,
            // title: row.get(2)?,
            // artist: row.get(3)?,
            // last_modified: row.get(4)?,
            data: row.get(0)?,
        })
    }
}

pub struct ArtistMetaRow {
    // name: String,
    // mbid: Option<String>,
    // last_modified: OffsetDateTime,
    data: Vec<u8>, // BSON
}

impl TryInto<ArtistMeta> for ArtistMetaRow {
    type Error = Error;
    fn try_into(self) -> Result<ArtistMeta, Self::Error> {
        let mut reader = Cursor::new(self.data);
        bson::deserialize_from_document(
            bson::Document::from_reader(&mut reader).map_err(|_| Error::BytesToDocError)?,
        )
        .map_err(|_| Error::DocToObjectError)
    }
}

impl TryFrom<&Row<'_>> for ArtistMetaRow {
    type Error = SqliteError;
    fn try_from(row: &Row) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            // name: row.get(0)?,
            // mbid: row.get(1)?,
            // last_modified: row.get(2)?,
            data: row.get(0)?,
        })
    }
}

pub struct LyricsRow {
    // uri: String,
    lyrics: String,
    synced: bool,
    // last_modified: OffsetDateTime,
}

impl TryInto<Lyrics> for LyricsRow {
    type Error = LyricsParseError;
    fn try_into(self) -> std::result::Result<Lyrics, Self::Error> {
        if self.synced {
            Ok(Lyrics::try_from_synced_lrclib_str(&self.lyrics)?)
        } else {
            Ok(Lyrics::try_from_plain_lrclib_str(&self.lyrics)?)
        }
    }
}

impl TryFrom<&Row<'_>> for LyricsRow {
    type Error = SqliteError;
    fn try_from(row: &Row) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            // uri: row.get(0)?,
            lyrics: row.get(0)?,
            synced: row.get(1)?,
            // last_modified: row.get(3)?,
        })
    }
}

pub fn find_album_meta(album: &AlbumInfo) -> Result<Option<AlbumMeta>, Error> {
    let query: Result<AlbumMetaRow, SqliteError>;
    let conn = SQLITE_POOL.get().unwrap();
    if let Some(mbid) = album.mbid.as_deref() {
        query = conn
            .prepare("select data from albums where mbid = ?1")
            .unwrap()
            .query_row(params![mbid], |r| AlbumMetaRow::try_from(r));
    } else if let (title, Some(artist)) = (&album.title, album.get_artist_tag()) {
        query = conn
            .prepare("select data from albums where title = ?1 and artist = ?2")
            .unwrap()
            .query_row(params![title, artist], |r| AlbumMetaRow::try_from(r));
    } else {
        return Ok(None);
    }
    match query {
        Ok(row) => {
            let res = row.try_into()?;
            Ok(Some(res))
        }
        Err(SqliteError::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::DbError(e)),
    }
}

pub fn find_artist_meta(artist: &ArtistInfo) -> Result<Option<ArtistMeta>, Error> {
    let query: Result<ArtistMetaRow, SqliteError>;
    let conn = SQLITE_POOL.get().unwrap();
    if let Some(mbid) = artist.mbid.as_deref() {
        query = conn
            .prepare("select data from artists where mbid = ?1")
            .unwrap()
            .query_row(params![mbid], |r| ArtistMetaRow::try_from(r));
    } else {
        query = conn
            .prepare("select data from artists where name = ?1")
            .unwrap()
            .query_row(params![&artist.name], |r| ArtistMetaRow::try_from(r));
    }
    match query {
        Ok(row) => {
            let res = row.try_into()?;
            Ok(Some(res))
        }
        Err(SqliteError::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::DbError(e)),
    }
}

pub fn write_album_meta(album: &AlbumInfo, meta: &AlbumMeta) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(Error::DbError)?;
    if let Some(mbid) = album.mbid.as_deref() {
        tx.execute("delete from albums where mbid = ?1", params![mbid])
            .map_err(Error::DbError)?;
    } else if let (title, Some(artist)) = (&album.title, album.get_artist_tag()) {
        tx.execute(
            "delete from albums where title = ?1 and artist = ?2",
            params![title, artist],
        )
        .map_err(Error::DbError)?;
    } else {
        tx.rollback().map_err(Error::DbError)?;
        return Err(Error::InsufficientKey);
    }
    tx.execute(
        "insert into albums (folder_uri, mbid, title, artist, last_modified, data) values (?1,?2,?3,?4,?5,?6)",
        params![
            &album.folder_uri,
            &album.mbid,
            &album.title,
            &album.get_artist_tag(),
            OffsetDateTime::now_utc(),
            bson::serialize_to_vec(
                &bson
                    ::serialize_to_document(meta)
                    .map_err(Error::ObjectToDocError)?
            ).map_err(Error::DocToBytesError)?
        ]
    ).map_err(Error::DbError)?;
    tx.commit().map_err(Error::DbError)?;
    Ok(())
}

pub fn write_artist_meta(artist: &ArtistInfo, meta: &ArtistMeta) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(Error::DbError)?;
    if let Some(mbid) = artist.mbid.as_deref() {
        tx.execute("delete from artists where mbid = ?1", params![mbid])
            .map_err(Error::DbError)?;
    } else {
        tx.execute("delete from artists where name = ?1", params![&artist.name])
            .map_err(Error::DbError)?;
    }
    tx.execute(
        "insert into artists (name, mbid, last_modified, data) values (?1,?2,?3,?4)",
        params![
            &artist.name,
            &artist.mbid,
            OffsetDateTime::now_utc(),
            bson::serialize_to_vec(
                &bson::serialize_to_document(meta).map_err(Error::ObjectToDocError)?
            )
            .map_err(Error::DocToBytesError)?
        ],
    )
    .map_err(Error::DbError)?;
    tx.commit().map_err(Error::DbError)?;
    Ok(())
}

pub fn find_lyrics(song: &SongInfo) -> Result<Option<Lyrics>, Error> {
    let query: Result<LyricsRow, SqliteError>;
    let conn = SQLITE_POOL.get().unwrap();
    query = conn
        .prepare("select lyrics, synced from songs where uri = ?1")
        .unwrap()
        .query_row(params![&song.uri], |r| LyricsRow::try_from(r));
    match query {
        Ok(row) => {
            if !row.lyrics.is_empty() {
                let res = row.try_into().map_err(|_| Error::DocToObjectError)?;
                Ok(Some(res))
            } else {
                Ok(None)
            }
        }
        Err(SqliteError::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::DbError(e)),
    }
}

pub fn write_lyrics(song: &SongInfo, lyrics: Option<&Lyrics>) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(Error::DbError)?;
    tx.execute("delete from songs where uri = ?1", params![&song.uri])
        .map_err(Error::DbError)?;
    if let Some(lyrics) = lyrics {
        tx.execute(
            "insert into songs (uri, lyrics, synced, last_modified) values (?1,?2,?3,?4)",
            params![
                &song.uri,
                &lyrics.to_string(),
                lyrics.synced,
                OffsetDateTime::now_utc()
            ],
        )
        .map_err(Error::DbError)?;
    } else {
        tx.execute(
            "insert into songs (uri, lyrics, synced, last_modified) values (?1,?2,?3,?4)",
            params![&song.uri, "", false, OffsetDateTime::now_utc()],
        )
        .map_err(Error::DbError)?;
    }
    tx.commit().map_err(Error::DbError)?;
    Ok(())
}

pub fn find_image_by_key(
    key: &str,
    prefix: Option<&str>,
    is_thumbnail: bool,
) -> Result<Option<String>, Error> {
    let query: Result<String, SqliteError>;
    let conn = SQLITE_POOL.get().unwrap();
    let final_key = if let Some(prefix) = prefix {
        &format!("{prefix}:{key}")
    } else {
        key
    };
    query = conn
        .prepare("select filename from images where key = ?1 and is_thumbnail = ?2")
        .unwrap()
        .query_row(params![final_key, is_thumbnail as i32], |r| {
            r.get::<usize, String>(0)
        });
    match query {
        Ok(filename) => Ok(Some(filename)),
        Err(SqliteError::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(Error::DbError(e)),
    }
}

/// Convenience wrapper for looking up covers. Automatically falls back to folder-level cover if possible.
pub fn find_cover_by_uri(track_uri: &str, is_thumbnail: bool) -> Result<Option<String>, Error> {
    if let Some(filename) = find_image_by_key(track_uri, None, is_thumbnail)? {
        Ok(Some(filename))
    } else {
        let folder_uri = strip_filename_linux(track_uri);
        if let Some(filename) = find_image_by_key(folder_uri, None, is_thumbnail)? {
            Ok(Some(filename))
        } else {
            Ok(None)
        }
    }
}

pub fn register_image_key(
    key: String,
    prefix: Option<&'static str>,
    filename: Option<String>,
    is_thumbnail: bool,
) -> ThreadHandle<Result<(), Error>> {
    SQLITE_WRITE_THREADPOOL
        .push(move || {
            let mut conn = SQLITE_POOL.get().unwrap();
            let tx = conn.transaction().map_err(Error::DbError)?;

            let final_key = if let Some(prefix) = prefix {
                &format!("{prefix}:{key}")
            } else {
                &key
            };

            tx.execute(
                "delete from images where key = ?1 and is_thumbnail = ?2",
                params![final_key, is_thumbnail as i32],
            )
            .map_err(Error::DbError)?;

            tx.execute(
            "insert into images (key, is_thumbnail, filename, last_modified) values (?1,?2,?3,?4)",
            params![
                final_key,
                is_thumbnail as i32,
                // Callers should interpret empty names as "tried but didn't find anything, don't try again"
                if let Some(filename) = filename {
                    filename
                } else {
                    "".to_owned()
                },
                OffsetDateTime::now_utc()
            ],
        )
          .map_err(Error::DbError)?;
            tx.commit().map_err(Error::DbError)?;
            Ok(())
        })
        .expect("register_image_key: Failed to schedule transaction with threadpool")
}

pub fn unregister_image_key(
    key: String,
    prefix: Option<&'static str>,
    is_thumbnail: bool,
) -> ThreadHandle<Result<(), Error>> {
    SQLITE_WRITE_THREADPOOL
        .push(move || {
            let mut conn = SQLITE_POOL.get().unwrap();
            let tx = conn.transaction().map_err(Error::DbError)?;
            let final_key = if let Some(prefix) = prefix {
                &format!("{prefix}:{key}")
            } else {
                &key
            };
            tx.execute(
                "delete from images where key = ?1 and is_thumbnail = ?2",
                params![final_key, is_thumbnail as i32],
            )
            .map_err(Error::DbError)?;
            tx.commit().map_err(Error::DbError)?;
            Ok(())
        })
        .expect("register_image_key: Failed to schedule transaction with threadpool")
}

pub fn add_to_history(song: &SongInfo) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(Error::DbError)?;
    let ts = OffsetDateTime::now_utc();
    tx.execute(
        "insert into songs_history (uri, timestamp) values (?1, ?2)",
        params![&song.uri, &ts],
    )
    .map_err(Error::DbError)?;
    if let Some(album) = song.album.as_ref() {
        tx.execute(
            "insert into albums_history (title, mbid, artist, timestamp) values (?1, ?2, ?3, ?4)",
            params![
                &album.title,
                album.mbid.as_ref(),
                album.albumartist.as_ref(),
                &ts
            ],
        )
        .map_err(Error::DbError)?;
    }
    for artist in song.artists.iter() {
        tx.execute(
            "insert into artists_history(name, timestamp) values (?1, ?2)",
            params![&artist.name, &ts],
        )
        .map_err(Error::DbError)?;
    }
    tx.commit().map_err(Error::DbError)?;
    Ok(())
}

/// Get URIs of up to N last listened to songs.
pub fn get_last_n_songs(n: u32) -> Result<Vec<(String, OffsetDateTime)>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare(
            "
select uri, max(timestamp) as last_played
from songs_history
group by uri order by last_played desc limit ?1",
        )
        .unwrap();
    let res = query
        .query_map(params![n], |r| {
            Ok((
                r.get::<usize, String>(0)?,
                r.get::<usize, OffsetDateTime>(1)?,
            ))
        })
        .map_err(Error::DbError)?
        .map(|r| r.unwrap());

    Ok(res.collect())
}

/// Get (title, artist, mbid)s of up to N last listened to albums.
pub fn get_last_n_albums(n: u32) -> Result<Vec<(String, Option<String>, Option<String>)>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare(
            "
select title, artist, mbid, max(timestamp) as last_played
from albums_history
group by title order by last_played desc limit ?1",
        )
        .unwrap();
    let res = query
        .query_map(params![n], |r| {
            Ok((
                r.get::<usize, String>(0)?,
                r.get::<usize, Option<String>>(1)?,
                r.get::<usize, Option<String>>(2)?,
            ))
        })
        .map_err(Error::DbError)?
        .map(|r| r.unwrap());

    Ok(res.collect())
}

/// Get names of up to N last listened to artists.
pub fn get_last_n_artists(n: u32) -> Result<Vec<String>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare(
            "
select name, max(timestamp) as last_played
from artists_history
group by name order by last_played desc limit ?1",
        )
        .unwrap();
    let res = query
        .query_map(params![n], |r| r.get::<usize, String>(0))
        .map_err(Error::DbError)?
        .map(|r| r.unwrap());

    Ok(res.collect())
}

pub fn clear_history() -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(Error::DbError)?;
    tx.execute("delete from songs_history", [])
        .map_err(Error::DbError)?;
    tx.execute("delete from albums_history", [])
        .map_err(Error::DbError)?;
    tx.execute("delete from artists_history", [])
        .map_err(Error::DbError)?;
    tx.commit().map_err(Error::DbError)?;
    Ok(())
}

/// Get basic information of each of the dynamic playlists. This returns INodeInfos as
/// lightweight "previews" of full DynamicPlaylist objects.
pub fn get_dynamic_playlists() -> Result<Vec<INodeInfo>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare("select name, last_modified from queries")
        .unwrap();
    Ok(query
        .query_map([], |r| {
            Ok(INodeInfo {
                uri: r.get::<usize, String>(0)?,
                last_modified: Some(format_datetime_local_tz(r.get::<usize, OffsetDateTime>(1)?)),
                inode_type: INodeType::Playlist,
            })
        })
        .map_err(Error::DbError)?
        .map(|r| r.unwrap())
        .collect::<Vec<INodeInfo>>())
}

pub fn exists_dynamic_playlist(name: &str) -> Result<bool, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare("select count(name) from queries where name = ?1")
        .unwrap();
    match query.query_one(params![name], |r| r.get::<usize, usize>(0)) {
        Ok(count) => Ok(count > 0),
        Err(SqliteError::QueryReturnedNoRows) => Ok(false),
        Err(e) => Err(Error::DbError(e)),
    }
}

pub fn insert_dynamic_playlist(
    dp: &DynamicPlaylist,
    overwrite_name: Option<&str>,
) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction()?;

    if let Some(to_overwrite) = overwrite_name {
        // This allows us to both edit and rename an existing DP in one call
        tx.execute(
            "delete from queries where name = ?1",
            params![&to_overwrite],
        )
        .map_err(Error::DbError)?;

        // Migrate image cache entry (if one exists) to new name
        if to_overwrite != dp.name {
            if let Err(db_err) = tx.execute(
                "update images set key = ?1 where key = ?2",
                params![
                    &format!("dynamic_playlist:{to_overwrite}"),
                    &format!("dynamic_playlist:{}", dp.name),
                ],
            ) {
                tx.rollback().map_err(Error::DbError)?;
                return Err(Error::DbError(db_err));
            }
        }
    }

    // Bail out if current name already exists. The overwriting case should have already
    // removed the existing option in the above logic.
    // We can't use exists_dynamic_playlist() here as the check has to be part of this
    // transaction.
    let count_res = tx.query_one(
        "select count(name) from queries where name = ?1",
        params![&dp.name],
        |r| r.get::<usize, usize>(0),
    );
    match count_res {
        Ok(count) => {
            if count > 0 {
                tx.rollback().map_err(Error::DbError)?;
                return Err(Error::KeyAlreadyExists);
            }
        }
        Err(SqliteError::QueryReturnedNoRows) => {}
        Err(e) => {
            return Err(Error::DbError(e));
        }
    }

    let last_queued = dp
        .last_queued
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok());

    let last_refresh = dp
        .last_refresh
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok());

    tx.execute(
        "insert into queries
(name, last_modified, last_queued, play_count, bson, auto_refresh, last_refresh, `limit`)
values (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            &dp.name,
            OffsetDateTime::now_utc(),
            last_queued,
            &dp.play_count,
            bson::serialize_to_vec(&bson::doc! {
                "rules": bson::serialize_to_bson(&dp.rules).map_err(Error::ObjectToDocError)?,
                "ordering": bson::serialize_to_bson(&dp.ordering).map_err(Error::ObjectToDocError)?
            })
            .map_err(Error::DocToBytesError)?,
            &dp.auto_refresh.to_str(),
            last_refresh,
            &dp.limit
        ],
    )?;

    tx.commit()?;
    Ok(())
}

pub fn cache_dynamic_playlist_results(name: &str, songs: &[SongInfo]) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction()?;

    //
    // Remove the previous result
    tx.execute(
        "delete from query_results where query_name = ?1",
        params![name],
    )?;
    for song in songs.iter() {
        tx.execute(
            "insert into query_results (query_name, uri) values (?1,?2)",
            params![name, song.uri],
        )?;
    }
    // Update last_refresh
    tx.execute(
        "update queries set last_refresh = ?1 where name = ?2",
        params![OffsetDateTime::now_utc(), name],
    )?;

    tx.commit()?;
    Ok(())
}

pub fn get_dynamic_playlist_info(name: &str) -> Result<Option<DynamicPlaylist>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare(
            "select
bson, name, last_queued, play_count, auto_refresh, last_refresh, `limit`
from queries where name = ?1",
        )
        .unwrap();

    query
        .query_one(params![name], |r| {
            let mut reader = Cursor::new(r.get::<usize, Vec<u8>>(0)?);
            let mut rules_and_ordering = bson::Document::from_reader(&mut reader).unwrap();
            Ok(DynamicPlaylist {
                name: r.get::<usize, String>(1)?,
                last_queued: r
                    .get::<usize, Option<OffsetDateTime>>(2)?
                    .map(|ts| ts.unix_timestamp()),
                play_count: r.get::<usize, usize>(3)?,
                rules: bson::deserialize_from_bson::<Vec<Rule>>(std::mem::take(
                    rules_and_ordering.get_mut("rules").unwrap(),
                ))
                .unwrap(),
                ordering: bson::deserialize_from_bson::<Vec<Ordering>>(std::mem::take(
                    rules_and_ordering.get_mut("ordering").unwrap(),
                ))
                .unwrap(),
                auto_refresh: AutoRefresh::from_str(&r.get::<usize, String>(4)?).unwrap(),
                last_refresh: r
                    .get::<usize, Option<OffsetDateTime>>(5)?
                    .map(|ts| ts.unix_timestamp()),
                limit: r.get::<usize, Option<u32>>(6)?,
            })
        })
        .optional()
        .map_err(Error::DbError)
}

pub fn get_cached_dynamic_playlist_results(name: &str) -> Result<Vec<String>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare("select uri from query_results where query_name = ?1")
        .unwrap();
    Ok(query
        .query_map(params![name], |r| r.get::<usize, String>(0))?
        .map(|r| r.unwrap())
        .collect::<Vec<String>>())
}

pub fn delete_dynamic_playlist(name: &str) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction()?;
    tx.execute("delete from queries where name = ?1", params![name])
        .map_err(Error::DbError)?;

    tx.execute(
        "delete from images where key = ?1",
        params![&format!("dynamic_playlist:{name}")],
    )
    .map_err(Error::DbError)?;

    tx.commit()?;
    Ok(())
}
