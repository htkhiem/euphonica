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
    super::{models, MetadataProvider},
    LrcLibResponse, PROVIDER_KEY,
};

pub const API_ROOT: &str = "https://lrclib.net/api/";

pub struct LrcLibWrapper {
    client: Client
}

impl LrcLibWrapper {
    fn get_lrclib(&self, params: &[(&str, &str)]) -> Option<Response> {
        let resp = self
            .client
            .get(format!("{API_ROOT}search"))
            .query(params)
            .header(USER_AGENT, APPLICATION_USER_AGENT)
            .send();
        if let Ok(res) = resp {
            return Some(res);
        }
        return None;
    }
}

impl MetadataProvider for LrcLibWrapper {
    fn new() -> Self {
        Self {
            client: Client::new()
        }
    }

    /// LRCLIB only provides song lyrics.
    fn get_album_meta(
        &self,
        _key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta> {
        existing
    }

    /// LRCLIB only provides song lyrics.
    fn get_artist_meta(
        &self,
        _key: &mut ArtistInfo,
        existing: Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta> {
        existing
    }

    fn get_lyrics(&self, key: &SongInfo) -> Option<models::Lyrics> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            let mut params: Vec<(&str, &str)> = Vec::new();
            params.push(("track_name", &key.title));
            if let Some(artists) = &key.artist_tag {
                params.push(("artist_name", artists));
            }
            if let Some(album) = &key.album {
                params.push(("album_name", &album.title));
            }

            if let Some(resp) = self.get_lrclib(&params) {
                match resp.status() {
                    reqwest::StatusCode::OK => {
                        match resp.json::<Vec<LrcLibResponse>>() {
                            Ok(parsed) => {
                                if parsed.len() > 0 {
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
                                        if let Ok(lyrics) =
                                            models::Lyrics::try_from_plain_lrclib_str(
                                                &parsed[best_idx].plain,
                                            )
                                        {
                                            res = Some(lyrics);
                                        }
                                    }
                                    res
                                } else {
                                    None
                                }
                            }
                            Err(e) => {
                                dbg!(e);
                                None
                            },
                        }
                    }
                    code => {
                        dbg!(code);
                        None
                    },
                }
                // Pick the one with the closest duration to our song
            } else {
                None
            }
        }
        else {
            None
        }
    }
}
