use std::time::SystemTime;

use crate::{
    common::{AlbumInfo, ArtistInfo, SongInfo},
    config::APPLICATION_USER_AGENT,
    utils::meta_provider_settings,
};

use gio::prelude::SettingsExt;
use reqwest::{
    blocking::{Client, Response},
    header::USER_AGENT,
};

use super::{
    super::{models, prelude::*, MetadataProvider},
    LrcLibResponse, PROVIDER_KEY,
};

pub const API_ROOT: &str = "https://lrclib.net/api/";

pub struct LrcLibWrapper {
    client: Client,
    last_request_time: SystemTime,
}

impl LrcLibWrapper {
    fn get_lrclib(&mut self, params: &[(&str, &str)]) -> (Option<Response>, bool) {
        sleep_between_requests(self.last_request_time);
        self.last_request_time = SystemTime::now();
        let mut resp = self
            .client
            .get(format!("{API_ROOT}search"))
            .query(params)
            .header(USER_AGENT, APPLICATION_USER_AGENT)
            .send();
        if resp.is_ok() {
            resp = resp.unwrap().error_for_status();
        }
        match resp {
            Ok(res) => (Some(res), false),
            Err(e) => {
                println!("{:?}", &e);
                (None, reqwest_error_is_retryable(&e))
            }
        }
    }
}

impl MetadataProvider for LrcLibWrapper {
    fn new() -> Self {
        Self {
            client: Client::new(),
            last_request_time: SystemTime::now(),
        }
    }

    /// LRCLIB only provides song lyrics.
    fn get_album_meta(
        &mut self,
        _key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> (Option<models::AlbumMeta>, bool) {
        (existing, false)
    }

    /// LRCLIB only provides song lyrics.
    fn get_artist_meta(
        &mut self,
        _key: &mut ArtistInfo,
        existing: Option<models::ArtistMeta>,
    ) -> (Option<models::ArtistMeta>, bool) {
        (existing, false)
    }

    fn get_lyrics(&mut self, key: &SongInfo) -> (Option<models::Lyrics>, bool) {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            let mut params: Vec<(&str, &str)> = Vec::new();
            params.push(("track_name", &key.title));
            if let Some(artists) = &key.artist_tag {
                params.push(("artist_name", artists));
            }
            if let Some(album) = &key.album {
                params.push(("album_name", &album.title));
            }
            let (resp, retry) = self.get_lrclib(&params);
            if let Some(resp) = resp {
                match resp.json::<Vec<LrcLibResponse>>() {
                    Ok(parsed) => {
                        if !parsed.is_empty() {
                            let mut best_idx: usize = 0;
                            let mut best_diff: f32 = (parsed[0].duration
                                - key.duration.map(|d| d.as_secs_f32()).unwrap_or(0.0))
                            .abs();
                            for i in 1..parsed.len() {
                                // Find the one with the closest duration
                                let diff = (parsed[i].duration
                                    - key.duration.map(|d| d.as_secs_f32()).unwrap_or(0.0))
                                .abs();
                                if diff < best_diff {
                                    best_diff = diff;
                                    best_idx = i;
                                }
                            }
                            let mut res: Option<models::Lyrics> = None;
                            if let Some(synced) = parsed[best_idx].synced.as_ref() {
                                if let Ok(lyrics) =
                                    models::Lyrics::try_from_synced_lrclib_str(synced)
                                {
                                    res = Some(lyrics);
                                }
                            }
                            if res.is_none() {
                                if let Ok(lyrics) = models::Lyrics::try_from_plain_lrclib_str(
                                    &parsed[best_idx].plain,
                                ) {
                                    res = Some(lyrics);
                                }
                            }
                            (res, false)
                        } else {
                            (None, false)
                        }
                    }
                    Err(e) => {
                        dbg!(e);
                        (None, retry)
                    }
                }
            } else {
                (None, retry)
            }
        } else {
            (None, false)
        }
    }
}
