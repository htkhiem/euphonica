use std::time::SystemTime;

use crate::{
    common::{AlbumInfo, ArtistInfo},
    config::APPLICATION_USER_AGENT,
    utils::meta_provider_settings,
};
use gtk::prelude::*;
use reqwest::{
    Error,
    blocking::{Client, RequestBuilder},
    header::USER_AGENT,
};
use urlencoding::encode as uenc;

use super::models::{
    TheAudioDbAlbum, TheAudioDbAlbumResponse, TheAudioDbArtist, TheAudioDbArtistResponse,
};
use super::{
    super::{MetadataProvider, models, prelude::*},
    PROVIDER_KEY,
};

const API_ROOT_V1: &str = "https://www.theaudiodb.com/api/v1/json";
const API_ROOT_V2: &str = "https://www.theaudiodb.com/api/v2/json";

#[derive(Clone, Copy)]
enum Action<'a> {
    /// Artist name string
    SearchArtist(&'a str),
    /// Artist name string, album name string
    SearchAlbum(&'a str, &'a str),
    /// Artist MBID
    LookupArtist(&'a str),
    /// Album MBID
    LookupAlbum(&'a str),
}

// For simplicity in handling both v1 (query params-based) and v2 (part of path)
// we take matters into our own hands here instead of using Reqwest's .query(),
// heh.
impl<'a> Action<'a> {
    fn url(&self, version: i32, api_key: &str) -> String {
        match version {
            // v1 uses query parameters, with the API key in the URL path.
            1 => {
                let key = uenc(api_key);
                match self {
                    Self::SearchArtist(name) => {
                        format!("{API_ROOT_V1}/{key}/search.php?s={}", uenc(name))
                    }
                    Self::SearchAlbum(artist, title) => format!(
                        "{API_ROOT_V1}/{key}/searchalbum.php?s={}&a={}",
                        uenc(artist), // artist name
                        uenc(title)   // album name
                    ),
                    Self::LookupArtist(mbid) => {
                        format!("{API_ROOT_V1}/{key}/artist-mb.php?i={}", uenc(mbid))
                    }
                    Self::LookupAlbum(mbid) => {
                        format!("{API_ROOT_V1}/{key}/album-mb.php?i={}", uenc(mbid))
                    }
                }
            }
            // v2 uses path segments, with the API key in a header.
            _ => match self {
                Self::SearchArtist(name) => {
                    format!("{API_ROOT_V2}/search/artist/{}", uenc(name))
                }
                Self::SearchAlbum(_artist, title) => {
                    format!("{API_ROOT_V2}/search/album/{}", uenc(title))
                }
                Self::LookupArtist(mbid) => {
                    format!("{API_ROOT_V2}/lookup/artist_mb/{}", uenc(mbid))
                }
                Self::LookupAlbum(mbid) => {
                    format!("{API_ROOT_V2}/lookup/album_mb/{}", uenc(mbid))
                }
            },
        }
    }
}

pub struct TheAudioDbWrapper {
    client: Client,
    last_request_time: SystemTime,
}

impl TheAudioDbWrapper {
    /// Returns a RequestBuilder with the necessary headers, or None if the
    /// user hasn't specified an API key.
    fn prepare_request(&self, action: Action) -> Option<RequestBuilder> {
        let settings = meta_provider_settings(PROVIDER_KEY);
        let api_key = settings.string("api-key").to_string();
        let version = settings.enum_("api-version");
        if api_key.is_empty() {
            return None;
        }
        let mut res = self
            .client
            .get(action.url(version, &api_key))
            .header(USER_AGENT, APPLICATION_USER_AGENT);
        if version != 1 {
            res = res.header("X-API-KEY", api_key);
        }
        Some(res)
    }
}

/// Map TheAudioDB HTTP status codes to errors.
/// TheAudioDB v1 returns 404 for bad API keys as well as missing entities,
/// and 429 for rate limiting.
fn map_http_error<T>(e: Error, existing: T) -> MetadataError<T> {
    if let Some(code) = e.status() {
        if code == reqwest::StatusCode::NOT_FOUND {
            return MetadataError::NotFound(existing);
        }
        if code == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return MetadataError::RateLimit(existing);
        }
    }
    MetadataError::Reqwest(existing, e)
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
            let action = if let Some(id) = key.mbid.as_ref() {
                Action::LookupAlbum(id)
            } else if let Some(artist) = key.albumartist.as_deref() {
                Action::SearchAlbum(artist, &key.title)
            } else {
                return Err(MetadataError::InsufficientKey(existing));
            };
            let req = match self.prepare_request(action) {
                Some(req) => req,
                None => return Err(MetadataError::Credential(existing)),
            };
            sleep_between_requests(self.last_request_time);
            self.last_request_time = SystemTime::now();
            let res = req.send().and_then(|r| r.error_for_status());
            let response = match res {
                Ok(response) => response,
                Err(e) => return Err(map_http_error(e, existing)),
            };
            let text = match response.text() {
                Ok(text) => text,
                Err(e) => {
                    println!("[TheAudioDB] Failed to read album response as text: {e}");
                    return Err(MetadataError::Parse(existing));
                }
            };
            match serde_json::from_str::<TheAudioDbAlbumResponse>(&text) {
                Ok(resp) => {
                    let mut albums: Vec<TheAudioDbAlbum> = resp.album.unwrap_or_default();
                    let album: Option<TheAudioDbAlbum> = if albums.is_empty() {
                        None
                    } else if key.mbid.is_some() {
                        // MBID lookup should return exactly one album.
                        if !albums.is_empty() {
                            Some(std::mem::take(&mut albums[0]))
                        } else {
                            None
                        }
                    } else {
                        // Look for the first album whose artist string matches,
                        // case-insensitive
                        let raw_target = key.albumartist.as_deref().unwrap_or("").to_lowercase();
                        let target = raw_target.trim();
                        albums.into_iter().find(|a| {
                            a.artist
                                .as_deref()
                                .map(|s| s.to_lowercase() == target)
                                .unwrap_or(false)
                                || a.artist_raw
                                    .as_deref()
                                    .map(|s| s.to_lowercase().trim() == target)
                                    .unwrap_or(false)
                        })
                    };
                    match album {
                        Some(parsed) => {
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
                        None => Ok(existing),
                    }
                }
                Err(e) => {
                    println!("[TheAudioDB] Failed to parse album response: {e}");
                    Err(MetadataError::Parse(existing))
                }
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
            let action = if let Some(id) = key.mbid.as_ref() {
                Action::LookupArtist(id)
            } else {
                Action::SearchArtist(&key.name)
            };
            let req = match self.prepare_request(action) {
                Some(req) => req,
                None => return Err(MetadataError::Credential(existing)),
            };
            sleep_between_requests(self.last_request_time);
            self.last_request_time = SystemTime::now();
            let res = req.send().and_then(|r| r.error_for_status());
            let response = match res {
                Ok(response) => response,
                Err(e) => return Err(map_http_error(e, existing)),
            };
            let text = match response.text() {
                Ok(text) => text,
                Err(e) => {
                    println!("[TheAudioDB] Failed to read artist response as text: {e}");
                    return Err(MetadataError::Parse(existing));
                }
            };
            match serde_json::from_str::<TheAudioDbArtistResponse>(&text) {
                Ok(resp) => {
                    let artist: Option<TheAudioDbArtist> = if let Some(mut artists) = resp.artists {
                        if artists.is_empty() {
                            None
                        } else {
                            // Take the first result. The name is overridden with the
                            // tag value later, so a fuzzy search miss is acceptable.
                            Some(std::mem::take(&mut artists[0]))
                        }
                    } else {
                        None
                    };
                    match artist {
                        Some(parsed) => {
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
                        None => Ok(existing),
                    }
                }
                Err(e) => {
                    println!("[TheAudioDB] Failed to parse artist response: {e}");
                    Err(MetadataError::Parse(existing))
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common::{AlbumInfo, ArtistInfo, QualityGrade};
    use rustc_hash::FxHashSet;

    #[test]
    fn v1_url_search_artist() {
        let action = Action::SearchArtist("Coldplay");
        assert_eq!(
            action.url(1, "test-key"),
            "https://www.theaudiodb.com/api/v1/json/test-key/search.php?s=Coldplay"
        );
    }

    #[test]
    fn v1_url_search_album() {
        let action = Action::SearchAlbum("Coldplay", "Parachutes");
        assert_eq!(
            action.url(1, "test-key"),
            "https://www.theaudiodb.com/api/v1/json/test-key/searchalbum.php?s=Coldplay&a=Parachutes"
        );
    }

    #[test]
    fn v1_url_lookup_artist() {
        let action = Action::LookupArtist("cc197bad-dc9c-440d-a5b5-d52ba2e14234");
        assert_eq!(
            action.url(1, "test-key"),
            "https://www.theaudiodb.com/api/v1/json/test-key/artist-mb.php?i=cc197bad-dc9c-440d-a5b5-d52ba2e14234"
        );
    }

    #[test]
    fn v1_url_lookup_album() {
        let action = Action::LookupAlbum("1dc4c347-a1db-32aa-b14f-bc9cc507b843");
        assert_eq!(
            action.url(1, "test-key"),
            "https://www.theaudiodb.com/api/v1/json/test-key/album-mb.php?i=1dc4c347-a1db-32aa-b14f-bc9cc507b843"
        );
    }

    #[test]
    fn v1_url_encodes_special_chars() {
        let action = Action::SearchArtist("Adele ft. Sam Smith");
        let url = action.url(1, "test-key");
        assert!(
            url.ends_with("search.php?s=Adele%20ft.%20Sam%20Smith"),
            "{url}"
        );
    }

    #[test]
    fn v2_url_search_artist() {
        let action = Action::SearchArtist("Coldplay");
        assert_eq!(
            action.url(2, "test-key"),
            "https://www.theaudiodb.com/api/v2/json/search/artist/Coldplay"
        );
    }

    #[test]
    fn v2_url_search_album() {
        let action = Action::SearchAlbum("Coldplay", "Parachutes");
        assert_eq!(
            action.url(2, "test-key"),
            "https://www.theaudiodb.com/api/v2/json/search/album/Parachutes"
        );
    }

    #[test]
    fn v2_url_lookup_artist() {
        let action = Action::LookupArtist("cc197bad-dc9c-440d-a5b5-d52ba2e14234");
        assert_eq!(
            action.url(2, "test-key"),
            "https://www.theaudiodb.com/api/v2/json/lookup/artist_mb/cc197bad-dc9c-440d-a5b5-d52ba2e14234"
        );
    }

    #[test]
    fn v2_url_lookup_album() {
        let action = Action::LookupAlbum("1dc4c347-a1db-32aa-b14f-bc9cc507b843");
        assert_eq!(
            action.url(2, "test-key"),
            "https://www.theaudiodb.com/api/v2/json/lookup/album_mb/1dc4c347-a1db-32aa-b14f-bc9cc507b843"
        );
    }

    /// Returns the manually-provided TheAudioDB API key for live tests, or None
    /// if the `TADB_API_KEY` environment variable is not set. Provide the key yourself
    /// when you wanna run these tests.
    /// Hard dependency for all tests defined after this function.
    fn live_api_key() -> Option<String> {
        std::env::var("TADB_API_KEY").ok().filter(|k| !k.is_empty())
    }

    #[test]
    #[ignore]
    fn live_v1_artist_lookup() {
        let api_key = match live_api_key() {
            Some(key) => key,
            None => {
                println!("TADB_API_KEY not set, skipping live test");
                return;
            }
        };
        let settings = meta_provider_settings(PROVIDER_KEY);
        settings.set_boolean("enabled", true).unwrap();
        settings.set_string("api-key", &api_key).unwrap();
        settings.set_enum("api-version", 1).unwrap();
        let mut provider = TheAudioDbWrapper::new();
        let mut key = ArtistInfo {
            name: "Coldplay".to_owned(),
            sort_tag: None,
            mbid: Some("cc197bad-dc9c-440d-a5b5-d52ba2e14234".to_owned()),
            genres: FxHashSet::default(),
            example_uris: Vec::new(),
            is_composer: false,
            is_albumartist: true,
        };
        let res = provider.get_artist_meta(&mut key, None).unwrap();
        let meta = res.expect("expected artist metadata");
        assert_eq!(meta.name, "Coldplay");
        assert_eq!(
            meta.mbid,
            Some("cc197bad-dc9c-440d-a5b5-d52ba2e14234".to_owned())
        );
    }

    #[test]
    #[ignore]
    fn live_v1_album_search() {
        let api_key = match live_api_key() {
            Some(key) => key,
            None => {
                println!("TADB_API_KEY not set, skipping live test");
                return;
            }
        };
        let settings = meta_provider_settings(PROVIDER_KEY);
        settings.set_boolean("enabled", true).unwrap();
        settings.set_string("api-key", &api_key).unwrap();
        settings.set_enum("api-version", 1).unwrap();
        let mut provider = TheAudioDbWrapper::new();
        let mut key = AlbumInfo {
            title: "Parachutes".to_owned(),
            albumsort: None,
            example_uri: String::new(),
            folder_uri: String::new(),
            artists: Vec::new(),
            albumartist: Some("Coldplay".to_owned()),
            albumartistsort: None,
            genres: FxHashSet::default(),
            cover: None,
            release_date: None,
            date_tag: None,
            quality_grade: QualityGrade::default(),
            mbid: None,
        };
        let res = provider.get_album_meta(&mut key, None).unwrap();
        let meta = res.expect("expected album metadata");
        assert_eq!(meta.name, "Parachutes");
        assert_eq!(meta.artist.as_deref(), Some("Coldplay"));
    }
}
