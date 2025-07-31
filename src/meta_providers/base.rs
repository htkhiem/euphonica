extern crate bson;
use gtk::{prelude::*, gdk};
use std::{thread, time::Duration};

use crate::{common::{AlbumInfo, ArtistInfo, SongInfo}, utils::settings_manager};

use super::models;

pub fn sleep_after_request() {
    let settings = settings_manager().child("metaprovider");
    thread::sleep(Duration::from_millis(
        (settings.double("delay-between-requests-s") * 1000.0) as u64,
    ));
}

/// Enum for communication with provider threads from the cache controller living on the main thread.
/// Can be used for both request and response.
pub enum ProviderMessage {
    ClearFolderCover(String),
    // EmbeddedCover(SongInfo),
    FolderCover(AlbumInfo), // Pass through the fallback parameter
    CoverAvailable(String, bool, gdk::Texture), // URI (can be track or folder), is_thumbnail, the texture itself
    /// Negative responses (currently only used by MpdWrapper)
    CoverNotAvailable(String), // URI can be track or folder
    FallbackToFolderCover(AlbumInfo),
    FallbackToEmbeddedCover(AlbumInfo),
    FetchFolderCoverExternally(AlbumInfo), // Pass through the fallback parameter
    AlbumMeta(AlbumInfo),
    AlbumMetaAvailable(String), // Only return URI
    ClearArtistAvatar(String), // Only need name
    /// Both request and positive response
    ArtistAvatar(ArtistInfo), // With cache basepath
    ArtistAvatarAvailable(String, bool, gdk::Texture), // Name, is_thumbnail, the texture itself
    /// Both request and positive response. Includes downloading artist avatar.
    ArtistMeta(ArtistInfo), // With cache basepath (for passthrough to artist avatar)
    ArtistMetaAvailable(String), // Only return name
    Lyrics(SongInfo),
    LyricsAvailable(String) // Only return full URI
}

/// Common provider-agnostic utilities.
pub mod utils {
    use super::*;
    use crate::utils;
    use image::DynamicImage;

    /// Get a file from the given URL as bytes. Useful for downloading images.
    fn get_file(url: &str) -> Option<Vec<u8>> {
        let response = reqwest::blocking::get(url);
        // This empty check comes in handy for certain metadata providers who, instead of
        // skipping the URL fields, opt to return an empty string instead.
        if !url.is_empty() {
            if let Ok(res) = response {
                if let Ok(bytes) = res.bytes() {
                    return Some(bytes.to_vec());
                } else {
                    println!("get_file: Failed to read response as bytes!");
                    return None;
                }
            }
            println!("get_file: {:?}", response.err());
            None
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
        for image in images.iter().rev() {
            if let Some(bytes) = get_file(image.url.as_ref()) {
                println!("Downloaded image from: {:?}", &image.url);
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
    /// Create a new instance of this metadata provider with the given priority. A priority of 0 is the highest
    /// & indicates the first provider to be called.
    fn new(prio: u32) -> Self
    where
        Self: Sized;

    /// Get an identifier of this metadata provider. This name must be unique & also used to name the corresponding
    /// child GSettings schema. For this reason, it must be all lowercase alphabetical letters.
    fn key(&self) -> &'static str;

    /// Get priority of this provider.
    fn priority(&self) -> u32;

    /// Set priority of this provider.
    fn set_priority(&self, prio: u32);

    /// Get textual metadata that wouldn't be available as song tags, such as wiki, producer name,
    /// etc. A new AlbumMeta object containing data from both the existing AlbumMeta and newly fetched data. New
    /// data will always overwrite existing fields.
    fn get_album_meta(
        &self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta>;

    /// Get textual metadata about an artist, such as biography, DoB, etc.
    /// A new ArtistMeta object containing data from both the existing ArtistMeta and newly fetched data. New
    /// data will always overwrite existing fields.
    fn get_artist_meta(
        &self,
        key: &mut ArtistInfo,
        existing: Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta>;

    /// Get lyrics for a song. Synced lyrics take precedence over plain ones. The lyrics with the most similar
    /// duration to the song is returned.
    ///
    /// Unlike with album and artist metadata, we stop when one metadata provider returns lyrics.
    fn get_lyrics(
        &self,
        key: &SongInfo
    ) -> Option<models::Lyrics>; 
}
