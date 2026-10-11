extern crate bson;

use std::time::SystemTime;

use gtk::prelude::*;
use reqwest::{
    Error, Url,
    blocking::{Client, RequestBuilder, Response},
    header::USER_AGENT,
};

use crate::{
    common::{AlbumInfo, ArtistInfo},
    config::APPLICATION_USER_AGENT,
    utils::meta_provider_settings,
};

use super::models::{TheAudioDbAlbumResponse, TheAudioDbArtistResponse};
use super::{
    super::{MetadataProvider, models, prelude::*},
    PROVIDER_KEY,
};

const API_ROOT_V1: &str = "https://www.theaudiodb.com/api/v1/json";
const API_ROOT_V2: &str = "https://www.theaudiodb.com/api/v2/json";

#[derive(Clone, Copy)]
enum Action<'a> {
    /// Artist string
    SearchArtist(&'a str),
    /// Album string, artist string
    SearchAlbum(&'a str, &'a str),
    /// Artist MBID
    LookupArtist(&'a str),
    /// Album MBID
    LookupAlbum(&'a str),
}

// For simplicity in handling both v1 (query params-based) and v2 (part of path) we take matters into our own hands here
// instead of using Reqwest's .query(), heh.
impl<'a> Action<'a> {
    pub fn v1(&self) -> String {
        let mut url = Url::parse(API_ROOT_V1).unwrap();
        {
            let mut segments = url.path_segments_mut().unwrap();
            match self {
                Self::SearchArtist(name) => {
                    segments.push("search.php").push(&format!("s={}", name))
                }
                Self::SearchAlbum(title, artist) => segments
                    .push("searchalbum.php")
                    .push(&format!("a={}&s={}", title, artist)),
                Self::LookupArtist(mbid) => {
                    segments.push("artist-mb.php").push(&format!("i={}", mbid))
                }
                Self::LookupAlbum(mbid) => {
                    segments.push("album-mb.php").push(&format!("i={}", mbid))
                }
            };
        }
        dbg!(url.to_string())
    }

    pub fn v2(&self) -> String {
        let mut url = Url::parse(API_ROOT_V1).unwrap();
        {
            let mut segments = url.path_segments_mut().unwrap();
            match self {
                Self::SearchArtist(name) => segments.push("search/artist").push(name),
                Self::SearchAlbum(title, _artist) => segments.push("search/album").push(title), // v2 doesn't allow filtering by artist?
                Self::LookupArtist(mbid) => segments.push("lookup/artist_mb").push(mbid),
                Self::LookupAlbum(mbid) => segments.push("lookup/album_mb").push(mbid),
            };
        }
        dbg!(url.to_string())
    }
}

pub struct TheAudioDbWrapper {
    client: Client,
    last_request_time: SystemTime,
}

impl TheAudioDbWrapper {
    /// Returns a RequestBuilder with the necessary headers.
    fn prepare_request(&self, action: Action) -> Option<RequestBuilder> {
        let settings = meta_provider_settings(PROVIDER_KEY);
        let api_key = settings.string("api-key").to_string();
        let version = settings.int("api-version");
        if api_key.is_empty() {
            return None;
        }
        let res = self
            .client
            .get(match version {
                1 => action.v1(),
                2 => action.v2(),
                _ => unimplemented!(),
            })
            .header(USER_AGENT, APPLICATION_USER_AGENT);
        Some(match version {
            2 => res.header("X-API-KEY", api_key),
            1 => res,
            _ => unimplemented!(),
        })
    }
}

impl MetadataProvider for TheAudioDbWrapper {
    fn new() -> Self {
        Self {
            client: Client::new(),
            last_request_time: SystemTime::now(),
        }
    }

    /// Get album metadata from TheAudioDB.
    /// If MBID is provided, use lookup, else search.
    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> MetadataResult<Option<models::AlbumMeta>> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            let res: Result<Response, Error>;
            let needs_artist_match: bool;
            if let Some(id) = key.mbid.as_ref() {
                if let Some(req) = self.prepare_request(Action::LookupAlbum(id)) {
                    res = req.send();
                } else {
                    return Err(MetadataError::Credential(existing));
                }
                needs_artist_match = false;
            } else if let Some(artist) = key.albumartist.as_deref() {
                if let Some(req) = self.prepare_request(Action::SearchAlbum(&key.title, artist)) {
                    res = req.send();
                } else {
                    return Err(MetadataError::Credential(existing));
                }
                needs_artist_match = true;
            } else {
                return Err(MetadataError::InsufficientKey(existing));
            }
            if let Err(_) = &res {
                return Err(MetadataError::Other(existing));
            }
            match res.unwrap().error_for_status() {
                Ok(response) => {
                    if let Ok(text) = response.text() {
                        match serde_json::from_str::<TheAudioDbAlbumResponse>(&text).map(
                            |mut resp| {
                                if resp.album.is_empty() {
                                    None
                                } else {
                                    // Look for the first album whose artist string matches, case-insensitive
                                    if needs_artist_match {
                                        let target =
                                            key.albumartist.as_deref().unwrap_or("").to_lowercase();
                                        // Guaranteed to have artist here as we can only arrive here with null MBID AND non-null artist
                                        if let Some(album) = resp
                                            .album
                                            .into_iter()
                                            .find(|a| &a.artist.to_lowercase() == &target)
                                        {
                                            Some(album)
                                        } else {
                                            None
                                        }
                                    } else {
                                        Some(std::mem::take(&mut resp.album[0]))
                                    }
                                }
                            },
                        ) {
                            Ok(Some(parsed)) => {
                                let mut new: models::AlbumMeta = parsed.into();
                                // Override album & artist names in case the returned values
                                // are slightly different (casing, apostrophes, etc.), else
                                // we won't be able to query it back using our own tags.
                                if let Some(artist) = key.get_artist_tag() {
                                    new.artist = Some(artist.to_owned());
                                }
                                if new.name != key.title {
                                    new.name = key.title.to_owned();
                                }
                                // If there is existing data, merge new data to it
                                Ok(Some(if let Some(old) = existing {
                                    old.merge(new)
                                } else {
                                    new
                                }))
                            }
                            Ok(None) => Ok(existing),
                            Err(_) => Err(MetadataError::Other(existing)),
                        }
                    } else {
                        Err(MetadataError::Parse(existing))
                    }
                }
                // yknow what v1 API actually returns 404 on API key errors...
                Err(e) => Err(MetadataError::Reqwest(existing, e)),
            }
        } else {
            Ok(existing)
        }
    }

    /// Get artist metadata.
    /// If MBID is provided, use lookup, else search.
    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        existing: Option<models::ArtistMeta>,
    ) -> MetadataResult<Option<models::ArtistMeta>> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            let res: Result<Response, Error>;
            if let Some(id) = key.mbid.as_ref() {
                if let Some(req) = self.prepare_request(Action::LookupArtist(id)) {
                    res = req.send();
                } else {
                    return Err(MetadataError::Credential(existing));
                }
            } else {
                if let Some(req) = self.prepare_request(Action::SearchArtist(&key.name)) {
                    res = req.send();
                } else {
                    return Err(MetadataError::Credential(existing));
                }
            }
            if let Err(_) = &res {
                return Err(MetadataError::Other(existing));
            }
            match res.unwrap().error_for_status() {
                Ok(response) => {
                    if let Ok(text) = response.text() {
                        match serde_json::from_str::<TheAudioDbArtistResponse>(&text).map(
                            |mut resp| {
                                if resp.artists.is_empty() {
                                    None
                                } else {
                                    Some(std::mem::take(&mut resp.artists[0]))
                                }
                            },
                        ) {
                            Ok(Some(parsed)) => {
                                let mut new: models::ArtistMeta = parsed.into();
                                // Override artist name in case the returned values
                                // are slightly different (casing, apostrophes, etc.), else
                                // we won't be able to query it back using our own tags.
                                if new.name != key.name {
                                    new.name = key.name.to_owned();
                                }
                                // If there is existing data, merge new data to it
                                Ok(Some(if let Some(old) = existing {
                                    old.merge(new)
                                } else {
                                    new
                                }))
                            }
                            Ok(None) => Ok(existing),
                            Err(_) => Err(MetadataError::Other(existing)),
                        }
                    } else {
                        Err(MetadataError::Parse(existing))
                    }
                }
                // yknow what v1 API actually returns 404 on API key errors...
                Err(e) => Err(MetadataError::Reqwest(existing, e)),
            }
        } else {
            Ok(existing)
        }
    }

    /// TheAudioDB does not provide lyrics.
    fn get_lyrics(
        &mut self,
        _key: &crate::common::SongInfo,
    ) -> MetadataResult<Option<models::Lyrics>> {
        Ok(None)
    }
}
