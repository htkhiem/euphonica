extern crate bson;
extern crate rusqlite;

use std::io::Cursor;

use once_cell::sync::Lazy;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Error as SqliteError, Result, Row};
use time::OffsetDateTime;
use glib::{ThreadPool, ThreadHandle};

use crate::{
    common::{AlbumInfo, ArtistInfo, SongInfo},
    meta_providers::models::{AlbumMeta, ArtistMeta, Lyrics, LyricsParseError},
    utils::strip_filename_linux,
};

use super::controller::get_doc_cache_path;

// Limit writes to a single thread to avoid DatabaseBusy races.
// Thread will be parked when idle.
static SQLITE_WRITE_THREADPOOL: Lazy<glib::ThreadPool> = Lazy::new(|| {
    ThreadPool::shared(Some(1)).expect("Failed to spawn Sqlite write threadpool")
});
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
            .unwrap().unwrap();

        println!("Local metadata DB version: {user_version}");
        match user_version {
            2 => {break;},
            1 => {
                conn.execute_batch("pragma journal_mode=WAL;
pragma user_version = 2;"
                ).expect("Unable to migrate DB version 1 to 2");
            },
            0 => {
                // Check if we're starting from nothing
                match conn.query_row(
                    "select name from sqlite_master where type='table' and name='albums'",
                    [], |row| row.get::<usize, String>(0)
                ) {
                    Ok(_) => {
                        println!("Upgrading local metadata DB to version 1...");
                        // Migrate album table schema: album table now accepts non-unique folder URIs
                        conn.execute_batch("begin;
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
end;").expect("Unable to migrate DB version 0 to 1");
                    }
                    Err(SqliteError::QueryReturnedNoRows) => {
                        // Starting from scratch
                        println!("Initialising local metadata DB...");
                        conn.execute_batch("begin;
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

pragma journal_mode=WAL;
pragma user_version = 2;
end;
").expect("Unable to init metadata SQLite DB");
                    }
                    e => {panic!("SQLite database error: {e:?}");}
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
    DocToMetaError,
    MetaToDocError,
    DocToBytesError,
    DbError(SqliteError),
    InsufficientKey,
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
        bson::from_document(
            bson::Document::from_reader(&mut reader).map_err(|_| Error::BytesToDocError)?,
        )
        .map_err(|_| Error::DocToMetaError)
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
        bson::from_document(
            bson::Document::from_reader(&mut reader).map_err(|_| Error::BytesToDocError)?,
        )
        .map_err(|_| Error::DocToMetaError)
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
            .prepare("select meta from albums where mbid = ?1")
            .unwrap()
            .query_row(params![mbid], |r| AlbumMetaRow::try_from(r));
    } else if let (title, Some(artist)) = (&album.title, album.get_artist_tag()) {
        query = conn
            .prepare("select meta from albums where title = ?1 and artist = ?2")
            .unwrap()
            .query_row(params![title, artist], |r| AlbumMetaRow::try_from(r));
    } else {
        return Ok(None);
    }
    match query {
        Ok(row) => {
            let res = row.try_into()?;
            return Ok(Some(res));
        }
        Err(SqliteError::QueryReturnedNoRows) => {
            return Ok(None);
        }
        Err(e) => {
            return Err(Error::DbError(e));
        }
    }
}

pub fn find_artist_meta(artist: &ArtistInfo) -> Result<Option<ArtistMeta>, Error> {
    let query: Result<ArtistMetaRow, SqliteError>;
    let conn = SQLITE_POOL.get().unwrap();
    if let Some(mbid) = artist.mbid.as_deref() {
        query = conn
            .prepare("select meta from artists where mbid = ?1")
            .unwrap()
            .query_row(params![mbid], |r| ArtistMetaRow::try_from(r));
    } else {
        query = conn
            .prepare("select meta from artists where name = ?1")
            .unwrap()
            .query_row(params![&artist.name], |r| ArtistMetaRow::try_from(r));
    }
    match query {
        Ok(row) => {
            let res = row.try_into()?;
            return Ok(Some(res));
        }
        Err(SqliteError::QueryReturnedNoRows) => {
            return Ok(None);
        }
        Err(e) => {
            return Err(Error::DbError(e));
        }
    }
}

pub async fn write_album_meta(album: &AlbumInfo, meta: &AlbumMeta) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
    if let Some(mbid) = album.mbid.as_deref() {
        tx.execute("delete from albums where mbid = ?1", params![mbid])
            .map_err(|e| Error::DbError(e))?;
    } else if let (title, Some(artist)) = (&album.title, album.get_artist_tag()) {
        tx.execute(
            "delete from albums where title = ?1 and artist = ?2",
            params![title, artist],
        )
        .map_err(|e| Error::DbError(e))?;
    } else {
        tx.rollback().map_err(|e| Error::DbError(e))?;
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
            bson::to_vec(&bson::to_document(meta).map_err(|_| Error::MetaToDocError)?).map_err(|_| Error::DocToBytesError)?
        ]
    ).map_err(|e| Error::DbError(e))?;
    tx.commit().map_err(|e| Error::DbError(e))?;
    Ok(())
}

pub fn write_artist_meta(artist: &ArtistInfo, meta: &ArtistMeta) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
    if let Some(mbid) = artist.mbid.as_deref() {
        tx.execute("delete from artists where mbid = ?1", params![mbid])
            .map_err(|e| Error::DbError(e))?;
    } else {
        tx.execute("delete from artists where name = ?1", params![&artist.name])
            .map_err(|e| Error::DbError(e))?;
    }
    tx.execute(
        "insert into artists (name, mbid, last_modified, data) values (?1,?2,?3,?4)",
        params![
            &artist.name,
            &artist.mbid,
            OffsetDateTime::now_utc(),
            bson::to_vec(&bson::to_document(meta).map_err(|_| Error::MetaToDocError)?)
                .map_err(|_| Error::DocToBytesError)?
        ],
    )
    .map_err(|e| Error::DbError(e))?;
    tx.commit().map_err(|e| Error::DbError(e))?;
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
            if row.lyrics.len() > 0 {
                let res = row.try_into().map_err(|_| Error::DocToMetaError)?;
                return Ok(Some(res));
            }
            else {
                return Ok(None);
            }
        }
        Err(SqliteError::QueryReturnedNoRows) => {
            return Ok(None);
        }
        Err(e) => {
            return Err(Error::DbError(e));
        }
    }
}

pub fn write_lyrics(song: &SongInfo, lyrics: Option<&Lyrics>) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
    tx.execute("delete from songs where uri = ?1", params![&song.uri])
        .map_err(|e| Error::DbError(e))?;
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
          .map_err(|e| Error::DbError(e))?;
    }
    else {
        tx.execute(
            "insert into songs (uri, lyrics, synced, last_modified) values (?1,?2,?3,?4)",
            params![
                &song.uri,
                "",
                false,
                OffsetDateTime::now_utc()
            ],
        )
          .map_err(|e| Error::DbError(e))?;
    }
    tx.commit().map_err(|e| Error::DbError(e))?;
    Ok(())
}

fn find_image_by_key(key: &str, prefix: Option<&str>, is_thumbnail: bool) -> Result<Option<String>, Error> {
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
            Ok(r.get::<usize, String>(0)?)
        });
    match query {
        Ok(filename) => {
            return Ok(Some(filename));
        }
        Err(SqliteError::QueryReturnedNoRows) => {
            return Ok(None);
        }
        Err(e) => {
            return Err(Error::DbError(e));
        }
    }
}

pub fn find_cover_by_key(key: &str, is_thumbnail: bool) -> Result<Option<String>, Error> {
    find_image_by_key(key, None, is_thumbnail)
}

pub fn find_avatar_by_key(key: &str, is_thumbnail: bool) -> Result<Option<String>, Error> {
    find_image_by_key(key, Some("avatar"), is_thumbnail)
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

fn register_image_key(
    key: String,
    prefix: Option<&'static str>,
    filename: Option<String>,
    is_thumbnail: bool
) -> ThreadHandle<Result<(), Error>> {
    SQLITE_WRITE_THREADPOOL.push(move || {
        let mut conn = SQLITE_POOL.get().unwrap();
        let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
        tx.execute(
            "delete from images where key = ?1 and is_thumbnail = ?2",
            params![key, is_thumbnail as i32],
        )
          .map_err(|e| Error::DbError(e))?;
        let final_key = if let Some(prefix) = prefix {
            &format!("{prefix}:{key}")
        } else {
            &key
        };
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
          .map_err(|e| Error::DbError(e))?;
        tx.commit().map_err(|e| Error::DbError(e))?;
        Ok(())
    }).expect("register_image_key: Failed to schedule transaction with threadpool")
}

pub fn register_cover_key(
    key: &str,
    filename: Option<&str>,
    is_thumbnail: bool,
) -> ThreadHandle<Result<(), Error>> {
    register_image_key(
        key.to_owned(), None, filename.map(str::to_owned), is_thumbnail
    )
}

pub fn register_avatar_key(
    key: &str,
    filename: Option<&str>,
    is_thumbnail: bool,
) -> ThreadHandle<Result<(), Error>> {
    register_image_key(
        key.to_owned(), Some("avatar"), filename.map(str::to_owned), is_thumbnail
    )
}

fn unregister_image_key(
    key: String,
    prefix: Option<&'static str>,
    is_thumbnail: bool
) -> ThreadHandle<Result<(), Error>> {
    SQLITE_WRITE_THREADPOOL.push(move || {
        let mut conn = SQLITE_POOL.get().unwrap();
        let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
        let final_key = if let Some(prefix) = prefix {
            &format!("{prefix}:{key}")
        } else {
            &key
        };
        tx.execute(
            "delete from images where key = ?1 and is_thumbnail = ?2",
            params![final_key, is_thumbnail as i32],
        )
          .map_err(|e| Error::DbError(e))?;
        tx.commit().map_err(|e| Error::DbError(e))?;
        Ok(())
    }).expect("register_image_key: Failed to schedule transaction with threadpool")
}

pub fn unregister_cover_key(key: &str, is_thumbnail: bool) -> ThreadHandle<Result<(), Error>> {
    unregister_image_key(key.to_owned(), None, is_thumbnail)
}

pub fn unregister_avatar_key(key: &str, is_thumbnail: bool) -> ThreadHandle<Result<(), Error>> {
    unregister_image_key(key.to_owned(), Some("avatar"), is_thumbnail)
}

pub fn add_to_history(song: &SongInfo) -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
    let ts = OffsetDateTime::now_utc();
    tx.execute(
        "insert into songs_history (uri, timestamp) values (?1, ?2)",
        params![&song.uri, &ts],
    )
    .map_err(|e| Error::DbError(e))?;
    if let Some(album) = song.album.as_ref() {
        tx.execute(
            "insert into albums_history (title, timestamp) values (?1, ?2)",
            params![&album.title, &ts],
        )
        .map_err(|e| Error::DbError(e))?;
    }
    for artist in song.artists.iter() {
        tx.execute(
            "insert into artists_history(name, timestamp) values (?1, ?2)",
            params![&artist.name, &ts],
        )
        .map_err(|e| Error::DbError(e))?;
    }
    tx.commit().map_err(|e| Error::DbError(e))?;
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
        .query_map(params![n], |r| Ok((r.get::<usize, String>(0)?, r.get::<usize, OffsetDateTime>(1)?)))
        .map_err(|e| Error::DbError(e))?
        .map(|r| r.unwrap());

    return Ok(res.collect());
}

/// Get titles of up to N last listened to albums.
pub fn get_last_n_albums(n: u32) -> Result<Vec<String>, Error> {
    let conn = SQLITE_POOL.get().unwrap();
    let mut query = conn
        .prepare(
            "
select title, max(timestamp) as last_played
from albums_history
group by title order by last_played desc limit ?1",
        )
        .unwrap();
    let res = query
        .query_map(params![n], |r| Ok(r.get::<usize, String>(0)?))
        .map_err(|e| Error::DbError(e))?
        .map(|r| r.unwrap());

    return Ok(res.collect());
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
        .query_map(params![n], |r| Ok(r.get::<usize, String>(0)?))
        .map_err(|e| Error::DbError(e))?
        .map(|r| r.unwrap());

    return Ok(res.collect());
}

pub fn clear_history() -> Result<(), Error> {
    let mut conn = SQLITE_POOL.get().unwrap();
    let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
    tx.execute("delete from songs_history", []).map_err(|e| Error::DbError(e))?;
    tx.execute("delete from albums_history", []).map_err(|e| Error::DbError(e))?;
    tx.execute("delete from artists_history", []).map_err(|e| Error::DbError(e))?;
    tx.commit().map_err(|e| Error::DbError(e))?;
    Ok(())
}
