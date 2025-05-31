extern crate bson;
extern crate rusqlite;

use std::{io::Cursor, path::Path};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{params, Error as SqliteError, Result, Row};
use time::OffsetDateTime;

use crate::{common::{AlbumInfo, ArtistInfo}, meta_providers::models::{AlbumMeta, ArtistMeta}};

#[derive(Debug)]
pub enum Error {
    BytesToDocError,
    DocToMetaError,
    MetaToDocError,
    DocToBytesError,
    DbError(SqliteError),
    InsufficientKey
}

pub struct AlbumMetaRow {
    folder_uri: String,
    mbid: Option<String>,
    title: String,
    artist: Option<String>,
    last_modified: OffsetDateTime,
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
            folder_uri: row.get(0)?,
            mbid: row.get(1)?,
            title: row.get(2)?,
            artist: row.get(3)?,
            last_modified: row.get(4)?,
            data: row.get(5)?
        })
    }
}

impl AlbumMetaRow {
    pub fn new(
        folder_uri: String,
        mbid: Option<String>,
        title: String,
        artist: Option<String>,
        last_modified: OffsetDateTime,
        meta: &AlbumMeta,
    ) -> Result<Self, Error> {
        let res = Self {
            folder_uri,
            mbid,
            title,
            artist,
            last_modified,
            data: bson::to_vec(&bson::to_document(meta).map_err(|_| Error::MetaToDocError)?)
                .map_err(|_| Error::DocToBytesError)?,
        };
        Ok(res)
    }
}

pub struct ArtistMetaRow {
    name: String,
    mbid: Option<String>,
    last_modified: OffsetDateTime,
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

impl ArtistMetaRow {
    pub fn new(
        name: String,
        mbid: Option<String>,
        last_modified: OffsetDateTime,
        meta: &ArtistMeta,
    ) -> Result<Self, Error> {
        let res = Self {
            name,
            mbid,
            last_modified,
            data: bson::to_vec(&bson::to_document(meta).map_err(|_| Error::MetaToDocError)?)
                .map_err(|_| Error::DocToBytesError)?,
        };
        Ok(res)
    }
}

impl TryFrom<&Row<'_>> for ArtistMetaRow {
    type Error = SqliteError;
    fn try_from(row: &Row) -> std::result::Result<Self, Self::Error> {
        Ok(Self {
            name: row.get(0)?,
            mbid: row.get(1)?,
            last_modified: row.get(2)?,
            data: row.get(3)?
        })
    }
}

#[derive(Debug, Clone)]
pub struct LocalMetaDb {
    pool: r2d2::Pool<SqliteConnectionManager>
}

impl LocalMetaDb {
    /// Connect to the local metadata database, or create an empty one if one
    /// does not exist yet.
    pub fn new<P: AsRef<Path>>(path: P) -> Result<Self, SqliteError> {
        let manager = SqliteConnectionManager::file(path);
        let pool = r2d2::Pool::new(manager).unwrap();
        // let conn = Connection::open(path)?;
        // Init schema & indices
        pool.get().unwrap().execute_batch(
            "begin;
create table if not exists `albums` (
    `folder_uri` VARCHAR not null unique,
    `mbid` VARCHAR null unique,
    `title` VARCHAR not null,
    `artist` VARCHAR null,
    `last_modified` DATETIME not null,
    `data` BLOB not null,
    primary key (`folder_uri`)
);
create unique index if not exists `album_mbid` on `albums` (
    `mbid`
);
create unique index if not exists `album_name` on `albums` (
    `folder_uri`,
    `title`
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
end;
",
        )?;

        Ok(Self {pool})
    }

    pub fn find_album_meta(&self, album: &AlbumInfo) -> Result<Option<AlbumMeta>, Error> {
        let query: Result<AlbumMetaRow, SqliteError>;
        let conn = self.pool.get().unwrap();
        if let Some(mbid) = album.mbid.as_deref() {
            query = conn
                .prepare("select * from albums where mbid = ?1")
                .unwrap().query_row(params![mbid], |r| { AlbumMetaRow::try_from(r) });
        }
        else if let (title, Some(artist)) = (&album.title, album.get_artist_tag()) {
            query = conn
                .prepare("select * from albums where title = ?1 and artist = ?2")
                .unwrap().query_row(params![title, artist], |r| { AlbumMetaRow::try_from(r) });
        }
        else { return Ok(None); }
        match query {
            Ok(row) => {
                let res = row.try_into()?;
                return Ok(Some(res));
            }
            Err(SqliteError::QueryReturnedNoRows) => {
                println!("Couldn't find anything for {:?} in local DB", album);
                return Ok(None);
            }
            Err(e) => {return Err(Error::DbError(e));}
        }
    }

    pub fn find_artist_meta(&self, artist: &ArtistInfo) -> Result<Option<ArtistMeta>, Error> {
        let query: Result<ArtistMetaRow, SqliteError>;
        let conn = self.pool.get().unwrap();
        if let Some(mbid) = artist.mbid.as_deref() {
            query = conn
                .prepare("select * from artists where mbid = ?1")
                .unwrap().query_row(params![mbid], |r| { ArtistMetaRow::try_from(r) });
        }
        else {
            query = conn
                .prepare("select * from artists where name = ?1")
                .unwrap().query_row(params![&artist.name], |r| { ArtistMetaRow::try_from(r) });
        }
        match query {
            Ok(row) => {
                let res = row.try_into()?;
                return Ok(Some(res));
            }
            Err(SqliteError::QueryReturnedNoRows) => {
                println!("Couldn't find anything for {:?} in local DB", artist);
                return Ok(None);
            }
            Err(e) => {return Err(Error::DbError(e));}
        }
    }

    pub fn write_album_meta(&self, album: &AlbumInfo, meta: &AlbumMeta) -> Result<(), Error> {
        let mut conn = self.pool.get().unwrap();
        let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
        if let Some(mbid) = album.mbid.as_deref() {
            tx.execute(
                "delete from albums where mbid = ?1",
                params![mbid]
            ).map_err(|e| Error::DbError(e))?;
        }
        else if let (title, Some(artist)) = (&album.title, album.get_artist_tag()) {
            tx.execute(
                "delete from albums where title = ?1 and artist = ?2",
                params![title, artist]
            ).map_err(|e| Error::DbError(e))?;
        }
        else {
            tx.rollback().map_err(|e| Error::DbError(e))?;
            return Err(Error::InsufficientKey);
        }
        tx.execute(
            "insert into albums (folder_uri, mbid, title, artist, last_modified, data) values (?1,?2,?3,?4,?5,?6)",
            params![
                &album.uri,
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

    pub fn write_artist_meta(&self, artist: &ArtistInfo, meta: &ArtistMeta) -> Result<(), Error>  {
        let mut conn = self.pool.get().unwrap();
        let tx = conn.transaction().map_err(|e| Error::DbError(e))?;
        if let Some(mbid) = artist.mbid.as_deref() {
            tx.execute(
                "delete from artists where mbid = ?1",
                params![mbid]
            ).map_err(|e| Error::DbError(e))?;
        }
        else {
            tx.execute(
                "delete from artists where name = ?1",
                params![&artist.name]
            ).map_err(|e| Error::DbError(e))?;
        }
        tx.execute(
            "insert into artists (name, mbid, last_modified, data) values (?1,?2,?3,?4)",
            params![
                &artist.name,
                &artist.mbid,
                OffsetDateTime::now_utc(),
                bson::to_vec(&bson::to_document(meta).map_err(|_| Error::MetaToDocError)?).map_err(|_| Error::DocToBytesError)?
            ]
        ).map_err(|e| Error::DbError(e))?;
        tx.commit().map_err(|e| Error::DbError(e))?;
        Ok(())
    }
}
