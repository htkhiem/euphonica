extern crate bson;
use crate::{
    common::{AlbumInfo, ArtistInfo, SongInfo},
    utils::settings_manager,
};
use gtk::prelude::*;
use reqwest::{blocking::Client, Error as ReqwestError, StatusCode};
use std::{
    thread,
    time::{Duration, SystemTime},
};

use super::models;

#[inline]
pub fn sleep_between_requests(last_request_time: SystemTime) {
    let settings = settings_manager().child("metaprovider");
    let wake_time =
        last_request_time + Duration::from_secs_f64(settings.double("delay-between-requests-s"));
    let now = SystemTime::now();
    // .duration_since returns an Err if the target_time is in the past
    if let Ok(remaining) = wake_time.duration_since(now) {
        println!("Sleeping for {remaining:?}");
        thread::sleep(remaining);
    }
}

#[inline]
pub fn status_is_retryable(c: StatusCode) -> bool {
    c.is_server_error()
}

/// Determines whether this Reqwest error is something that can be fixed simply by retrying.
#[inline]
pub fn reqwest_error_is_retryable(e: &ReqwestError) -> bool {
    e.status().is_some_and(status_is_retryable) || e.is_connect() || e.is_timeout()
}

/// Common provider-agnostic utilities.
pub mod utils {
    use super::*;
    use crate::{config::APPLICATION_USER_AGENT, utils};
    use image::DynamicImage;
    use reqwest::header::USER_AGENT;

    /// Get a file from the given URL as bytes. Useful for downloading images.
    /// This function will handle its own retries. Callers should NOT loop it
    /// for that purpose.
    fn get_file(url: &str) -> Option<Vec<u8>> {
        let client = Client::default();
        let settings = settings_manager().child("metaprovider");
        // This empty check comes in handy for certain metadata providers who, instead of
        // skipping the URL fields, opt to return an empty string instead.
        if !url.is_empty() {
            let mut tries_left: u32 = settings.uint("n-tries").max(1); // Just in case config is corrupt
            loop {
                let wake_time = SystemTime::now()
                    + Duration::from_secs_f64(settings.double("delay-between-requests-s"));
                let mut res = client
                    .get(url)
                    .header(USER_AGENT, APPLICATION_USER_AGENT)
                    .send();
                if let Ok(response) = res {
                    // Treat non-200 codes as errors too
                    res = response.error_for_status();
                }
                match res {
                    Ok(res) => {
                        if let Ok(bytes) = res.bytes() {
                            // Only for testing
                            // if let Ok(s) = str::from_utf8(&bytes) {
                            //     println!("Received UTF8 instead: {s}");
                            // }
                            return Some(bytes.to_vec());
                        } else {
                            println!("get_file: Failed to read response as bytes!");
                            return None;
                        }
                    }
                    Err(e) => {
                        if reqwest_error_is_retryable(&e) {
                            tries_left -= 1;
                            println!(
                                "get_file: URL {}, got error {:?}. Retries left: {}.",
                                url, &e, tries_left
                            );
                            if tries_left > 0 {
                                if let Ok(remaining) = wake_time.duration_since(SystemTime::now()) {
                                    println!("Sleeping for {remaining:?}");
                                    thread::sleep(remaining);
                                }
                            } else {
                                return None;
                            }
                        } else {
                            println!(
                                "get_file: URL {}, got unrecoverable error {}. Not retrying.",
                                url, &e
                            );
                            return None;
                        }
                    }
                }
            }
        } else {
            None
        }
    }

    pub fn get_best_image(metas: &[models::ImageMeta]) -> Result<DynamicImage, String> {
        // Get all image URLs, sorted by size in reverse.
        // Avoid cloning by sorting a mutable vector of references.
        let mut images: Vec<&models::ImageMeta> = metas.iter().collect();
        if images.is_empty() {
            return Err(String::from(
                "This album's metadata does not provide any image.",
            ));
        }
        images.sort_by_key(|img| img.size);
        for image_meta in images.iter().rev() {
            if let Some(bytes) = get_file(image_meta.url.as_ref()) {
                println!("Downloaded image from: {:?}", &image_meta.url);
                if let Some(image) = utils::read_image_from_bytes(bytes) {
                    return Ok(image);
                }
            }
        }
        Err(String::from(
            "This album's metadata provided image URLs but none of them could be downloaded.",
        ))
    }
}

pub trait MetadataProvider: Send + Sync {
    /// Create a new instance of this metadata provider.
    fn new() -> Self
    where
        Self: Sized;

    /// Get textual metadata that wouldn't be available as song tags, such as wiki, producer name,
    /// etc. A new AlbumMeta object containing data from both the existing AlbumMeta and newly fetched data. New
    /// data will always overwrite existing fields.
    ///
    /// Returns the metadata object possibly enhanced by this provider and a flag indicating whether
    /// the caller should retry (i.e. server/connection error).
    /// Each provider may implement the above retry flag differently but generally retries should only be
    /// attempted on 5xx error codes and connection errors, not content-not-available or permisison ones.
    /// It MUST be false after a successful call.
    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> (Option<models::AlbumMeta>, bool);

    /// Get textual metadata about an artist, such as biography, DoB, etc.
    /// A new ArtistMeta object containing data from both the existing ArtistMeta and newly fetched data. New
    /// data will always overwrite existing fields.
    ///
    /// Returns the metadata object possibly enhanced by this provider and a flag indicating whether
    /// the caller should retry (i.e. server/connection error).
    /// Each provider may implement the above retry flag differently but generally retries should only be
    /// attempted on 5xx error codes and connection errors, not content-not-available or permisison ones.
    /// It MUST be false after a successful call.
    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        existing: Option<models::ArtistMeta>,
    ) -> (Option<models::ArtistMeta>, bool);

    /// Get lyrics for a song. Synced lyrics take precedence over plain ones. The lyrics with the most similar
    /// duration to the song is returned.
    ///
    /// Unlike with album and artist metadata, we stop when one metadata provider returns lyrics.
    ///
    /// Returns lyrics from this provide (if available) and a flag indicating whether
    /// the caller should retry (i.e. server/connection error).
    /// Each provider may implement the above retry flag differently but generally retries should only be
    /// attempted on 5xx error codes and connection errors, not content-not-available or permisison ones.
    /// It MUST be false after a successful call.
    fn get_lyrics(&mut self, key: &SongInfo) -> (Option<models::Lyrics>, bool);
}
