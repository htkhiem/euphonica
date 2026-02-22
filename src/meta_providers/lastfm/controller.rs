extern crate bson;

use std::time::SystemTime;

use gtk::prelude::*;
use reqwest::{blocking::Client, header::USER_AGENT};

use crate::{
    common::{AlbumInfo, ArtistInfo},
    config::APPLICATION_USER_AGENT,
    meta_providers::lastfm::models::LastfmErrorResponse,
    utils::meta_provider_settings,
};

use super::models::{LastfmAlbumResponse, LastfmArtistResponse};
use super::{
    super::{MetadataProvider, models, prelude::*},
    PROVIDER_KEY,
};

pub const API_ROOT: &str = "http://ws.audioscrobbler.com/2.0";

pub struct LastfmWrapper {
    client: Client,
    last_request_time: SystemTime,
}

impl MetadataProvider for LastfmWrapper {
    fn new() -> Self {
        Self {
            client: Client::new(),
            last_request_time: SystemTime::now(),
        }
    }

    /// Schedule getting album metadata from Last.fm.
    /// A signal will be emitted to notify the caller when the result arrives.
    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> MetadataResult<Option<models::AlbumMeta>> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            // get_lastfm will only return Some(resp) if status code is 200
            let settings = meta_provider_settings(PROVIDER_KEY);
            let api_key = settings.string("api-key").to_string();
            // Return None if there is no API key specified.
            if !api_key.is_empty() {
                let mut params: Vec<(&str, &str)> = Vec::new();
                if let Some(id) = key.mbid.as_ref() {
                    params.push(("mbid", id));
                } else if let (name, Some(artist)) = (&key.title, key.get_artist_tag().as_ref()) {
                    params.push(("album", name));
                    params.push(("artist", artist));
                } else {
                    return Err(MetadataError::InsufficientKey(existing));
                }
                sleep_between_requests(self.last_request_time);
                println!("[Last.fm] Calling `album.getInfo` with query {params:?}");
                self.last_request_time = SystemTime::now();
                let mut res = self
                    .client
                    .get(API_ROOT)
                    .query(&[
                        ("format", "json"),
                        ("method", "album.getInfo"),
                        ("api_key", api_key.as_ref()),
                    ])
                    .query(&params)
                    .header(USER_AGENT, APPLICATION_USER_AGENT)
                    .send();
                if res.is_ok() {
                    res = res.unwrap().error_for_status();
                }
                match res {
                    Ok(response) => {
                        // Might still be a Last.fm error
                        if let Ok(text) = response.text() {
                            match serde_json::from_str::<LastfmAlbumResponse>(&text) {
                                Ok(parsed) => {
                                    // Some preprocessing is needed.
                                    // Might have to put the mbid back in, as Last.fm won't return
                                    // it if we queried using it in the first place.
                                    let mut new: models::AlbumMeta = parsed.album.into();
                                    if let Some(id) = key.mbid.as_ref() {
                                        new.mbid = Some(id.to_owned());
                                    }
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
                                Err(_) => {
                                    match serde_json::from_str::<LastfmErrorResponse>(&text) {
                                        Ok(err_resp) => match err_resp.error {
                                            29 => Err(MetadataError::RateLimit(existing)),
                                            4 | 9 | 10 | 26 => {
                                                Err(MetadataError::Credential(existing))
                                            }
                                            11 | 16 => Err(MetadataError::Temporary(existing)),
                                            _ => Err(MetadataError::Other(existing)),
                                        },
                                        Err(_) => Err(MetadataError::Parse(existing)),
                                    }
                                }
                            }
                        } else {
                            Err(MetadataError::Parse(existing))
                        }
                    }
                    Err(e) => {
                        return Err(MetadataError::Reqwest(existing, e));
                    }
                }
            } else {
                return Err(MetadataError::Credential(existing));
            }
        } else {
            Ok(existing)
        }
    }

    /// Schedule getting artist metadata from Last.fm.
    /// A signal will be emitted to notify the caller when the result arrives.
    /// Note that Last.fm no longer supports fetching artist images (URLs will always return
    /// a white star on grey background placeholder). For this reason, we will not parse
    /// artist image URLs.
    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        existing: std::option::Option<models::ArtistMeta>,
    ) -> MetadataResult<Option<models::ArtistMeta>> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            // get_lastfm will only return Some(resp) if status code is 200
            let settings = meta_provider_settings(PROVIDER_KEY);
            let api_key = settings.string("api-key").to_string();
            // Return None if there is no API key specified.
            if !api_key.is_empty() {
                let mut params: Vec<(&str, &str)> = Vec::new();
                if let Some(id) = key.mbid.as_ref() {
                    params.push(("mbid", id));
                } else {
                    params.push(("artist", &key.name))
                }
                sleep_between_requests(self.last_request_time);
                println!("[Last.fm] Calling `artist.getInfo` with query {params:?}");
                self.last_request_time = SystemTime::now();
                let mut res = self
                    .client
                    .get(API_ROOT)
                    .query(&[
                        ("format", "json"),
                        ("method", "artist.getinfo"),
                        ("api_key", api_key.as_ref()),
                    ])
                    .query(&params)
                    .header(USER_AGENT, APPLICATION_USER_AGENT)
                    .send();

                if res.is_ok() {
                    res = res.unwrap().error_for_status();
                }
                match res {
                    Ok(response) => {
                        // Might still be a Last.fm error
                        if let Ok(text) = response.text() {
                            match serde_json::from_str::<LastfmArtistResponse>(&text) {
                                Ok(parsed) => {
                                    // Some preprocessing is needed.
                                    // Might have to put the mbid back in, as Last.fm won't return
                                    // it if we queried using it in the first place.
                                    let mut new: models::ArtistMeta = parsed.artist.into();
                                    if let Some(id) = key.mbid.as_ref() {
                                        new.mbid = Some(id.to_owned());
                                    }
                                    // Override artist name in case the returned values
                                    // are slightly different (casing, apostrophes, etc.), else
                                    // we won't be able to query it back using our own tags.
                                    if new.name != key.name {
                                        new.name = key.name.to_owned();
                                    }
                                    Ok(Some(if let Some(old) = existing {
                                        old.merge(new)
                                    } else {
                                        new
                                    }))
                                }
                                Err(_) => {
                                    match serde_json::from_str::<LastfmErrorResponse>(&text) {
                                        Ok(err_resp) => match err_resp.error {
                                            29 => Err(MetadataError::RateLimit(existing)),
                                            4 | 9 | 10 | 26 => {
                                                Err(MetadataError::Credential(existing))
                                            }
                                            11 | 16 => Err(MetadataError::Temporary(existing)),
                                            _ => Err(MetadataError::Other(existing)),
                                        },
                                        Err(_) => Err(MetadataError::Parse(existing)),
                                    }
                                }
                            }
                        } else {
                            Err(MetadataError::Parse(existing))
                        }
                    }
                    Err(e) => {
                        return Err(MetadataError::Reqwest(existing, e));
                    }
                }
            } else {
                return Err(MetadataError::Credential(existing));
            }
        } else {
            Ok(existing)
        }
    }

    /// Last.fm does not provide lyrics.
    fn get_lyrics(
        &mut self,
        _key: &crate::common::SongInfo,
    ) -> MetadataResult<Option<models::Lyrics>> {
        Ok(None)
    }
}
