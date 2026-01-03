extern crate bson;
use crate::{
    common::{AlbumInfo, ArtistInfo, SongInfo},
    utils::settings_manager,
};
use gtk::prelude::*;
use reqwest::blocking::Client;
use std::{thread, time::{Duration, SystemTime}};

use super::models;

pub fn sleep_between_requests(request_time: SystemTime) {
    let settings = settings_manager().child("metaprovider");
    let wake_time = request_time + Duration::from_secs_f64(settings.double("delay-between-requests-s"));
    let now = SystemTime::now();
    // .duration_since returns an Err if the target_time is in the past
    if let Ok(remaining) = wake_time.duration_since(now) {
        println!("Sleeping for {remaining:?}");
        thread::sleep(remaining);
    }
}

/// Common provider-agnostic utilities.
pub mod utils {
    use super::*;
    use crate::{config::APPLICATION_USER_AGENT, utils};
    use image::DynamicImage;
    use reqwest::header::USER_AGENT;

    /// Get a file from the given URL as bytes. Useful for downloading images.
    fn get_file(url: &str) -> Option<Vec<u8>> {
        let client = Client::default();
        // This empty check comes in handy for certain metadata providers who, instead of
        // skipping the URL fields, opt to return an empty string instead.
        if !url.is_empty() {
            match client
                .get(url)
                .header(USER_AGENT, APPLICATION_USER_AGENT)
                .send()
            {
                Ok(res) => {
                    if let Ok(bytes) = res.bytes() {
                        if let Ok(s) = str::from_utf8(&bytes) {
                            println!("Received UTF8 instead: {s}");
                        }
                        Some(bytes.to_vec())
                    } else {
                        println!("get_file: Failed to read response as bytes!");
                        None
                    }
                }
                Err(e) => {
                    println!("get_file: {e:?}");
                    None
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
    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta>;

    /// Get textual metadata about an artist, such as biography, DoB, etc.
    /// A new ArtistMeta object containing data from both the existing ArtistMeta and newly fetched data. New
    /// data will always overwrite existing fields.
    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        existing: Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta>;

    /// Get lyrics for a song. Synced lyrics take precedence over plain ones. The lyrics with the most similar
    /// duration to the song is returned.
    ///
    /// Unlike with album and artist metadata, we stop when one metadata provider returns lyrics.
    fn get_lyrics(&mut self, key: &SongInfo) -> Option<models::Lyrics>;
}
