extern crate bson;

use std::sync::RwLock;

use gtk::prelude::*;
use reqwest::{
    blocking::{Client, Response},
    header::USER_AGENT,
};

use crate::{config::APPLICATION_USER_AGENT, utils::meta_provider_settings};

use super::models::{LastfmAlbumResponse, LastfmArtistResponse};
use super::{
    super::{models, prelude::*, MetadataProvider},
    PROVIDER_KEY,
};

pub const API_ROOT: &str = "http://ws.audioscrobbler.com/2.0";

pub struct LastfmWrapper {
    client: Client,
    priority: RwLock<u32>,
}

impl LastfmWrapper {
    fn get_lastfm(&self, method: &str, params: &[(&str, String)]) -> Option<Response> {
        let settings = meta_provider_settings(PROVIDER_KEY);
        let key = settings.string("api-key").to_string();
        // Return None if there is no API key specified.
        if !key.is_empty() {
            println!("Last.fm: calling `{}` with query {:?}", method, params);
            let resp = self
                .client
                .get(API_ROOT)
                .query(&[
                    ("format", "json"),
                    ("method", method),
                    ("api_key", key.as_ref()),
                ])
                .query(params)
                .header(USER_AGENT, APPLICATION_USER_AGENT)
                .send();
            if let Ok(res) = resp {
                return Some(res);
            }
            return None;
        }
        None
    }
}

impl MetadataProvider for LastfmWrapper {
    fn new(prio: u32) -> Self {
        Self {
            client: Client::new(),
            priority: RwLock::new(prio),
        }
    }

    fn key(&self) -> &'static str {
        PROVIDER_KEY
    }

    fn priority(&self) -> u32 {
        *self.priority.read().expect("Poisoned RwLock")
    }

    fn set_priority(&self, prio: u32) {
        let mut this_prio = self.priority.write().expect("Poisoned RwLock");
        *this_prio = prio;
    }

    /// Schedule getting album metadata from Last.fm.
    /// A signal will be emitted to notify the caller when the result arrives.
    fn get_album_meta(
        &self,
        key: bson::Document,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            // Will panic if key document is not a simple map of String to String
            let params: Vec<(&str, String)> = key
                .iter()
                .map(|kv: (&String, &bson::Bson)| {
                    // Last.fm wants "album" in query param but will return "name".
                    // Our bson key follows the returned result schema so it'll have to be renamed here.
                    (
                        if kv.0 == "name" {
                            "album"
                        } else {
                            kv.0.as_ref()
                        },
                        kv.1.as_str().unwrap().to_owned(),
                    )
                })
                .collect();

            if let Some(resp) = self.get_lastfm("album.getinfo", &params) {
                // TODO: Get summary
                match resp.status() {
                    reqwest::StatusCode::OK => {
                        match resp.json::<LastfmAlbumResponse>() {
                            Ok(parsed) => {
                                // Some preprocessing is needed.
                                // Might have to put the mbid back in, as Last.fm won't return
                                // it if we queried using it in the first place.
                                let mut new: models::AlbumMeta = parsed.album.into();
                                if let Some(id) = key.get("mbid") {
                                    new.mbid = Some(id.as_str().unwrap().to_owned());
                                }
                                // Override album & artist names in case the returned values
                                // are slightly different (casing, apostrophes, etc.), else
                                // we won't be able to query it back using our own tags.
                                if let Some(artist) = key.get("artist") {
                                    new.artist = Some(artist.as_str().unwrap().to_owned());
                                }
                                if let Some(name) = key.get("album") {
                                    new.name = name.as_str().unwrap().to_owned();
                                }
                                // If there is existing data, merge new data to it
                                if let Some(old) = existing {
                                    Some(old.merge(new))
                                } else {
                                    Some(new)
                                }
                            }
                            Err(err) => {
                                println!("[Last.fm] get_album_meta: {}", err);
                                existing
                            }
                        }
                    }
                    other => {
                        println!("[Last.fm] get_album_meta: failed with status {:?}", other);
                        existing
                    }
                }
            } else {
                existing
            }
        } else {
            existing
        }
    }

    /// Schedule getting artist metadata from Last.fm.
    /// A signal will be emitted to notify the caller when the result arrives.
    /// Note that Last.fm no longer supports fetching artist images (URLs will always return
    /// a white star on grey background placeholder). For this reason, we will not parse
    /// artist image URLs.
    fn get_artist_meta(
        &self,
        key: bson::Document,
        existing: std::option::Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            // Will panic if key document is not a simple map of String to String
            let params: Vec<(&str, String)> = key
                .iter()
                .map(|kv: (&String, &bson::Bson)| {
                    // Last.fm wants "artist" in query param but will return "name".
                    // Our bson key follows the returned result schema so it'll have to be renamed here.
                    (
                        if kv.0 == "name" {
                            "artist"
                        } else {
                            kv.0.as_ref()
                        },
                        kv.1.as_str().unwrap().to_owned(),
                    )
                })
                .collect();
            if let Some(resp) = self.get_lastfm("artist.getinfo", &params) {
                // TODO: Get summary
                match resp.status() {
                    reqwest::StatusCode::OK => {
                        match resp.json::<LastfmArtistResponse>() {
                            Ok(parsed) => {
                                // Some preprocessing is needed.
                                // Might have to put the mbid back in, as Last.fm won't return
                                // it if we queried using it in the first place.
                                let mut new: models::ArtistMeta = parsed.artist.into();
                                if let Some(id) = key.get("mbid") {
                                    new.mbid = Some(id.as_str().unwrap().to_owned());
                                }
                                // Override artist name in case the returned values
                                // are slightly different (casing, apostrophes, etc.), else
                                // we won't be able to query it back using our own tags.
                                if let Some(name) = key.get("artist") {
                                    new.name = name.as_str().unwrap().to_owned();
                                }
                                if let Some(old) = existing {
                                    Some(old.merge(new))
                                } else {
                                    Some(new)
                                }
                            }
                            Err(err) => {
                                println!("[Last.fm] get_artist_meta: {}", err);
                                existing
                            }
                        }
                    }
                    other => {
                        println!("[Last.fm] get_artist_meta: failed with status {:?}", other);
                        existing
                    }
                }
            } else {
                existing
            }
        } else {
            existing
        }
    }
}
