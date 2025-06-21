use chrono::NaiveDate;
use musicbrainz_rs::entity::artist::ArtistType;
use serde::{Deserialize, Serialize};

use crate::common::{AlbumInfo, ArtistInfo};

// Common building blocks that can be shared between different providers
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Tag {
    pub url: Option<String>,
    pub name: String,
    pub count: Option<i32>,
}

pub trait Tagged {
    fn get_tags(&self) -> &[Tag];
}

pub trait Merge {
    /// Merge data from another AlbumMeta instance.
    /// - Vec fields are concatenated.
    /// - Option fields are filled in if our own is None and theirs is Some.
    /// - Normal fields are unchanged.
    fn merge(self, other: Self) -> Self;

    /// Convenience provided function: apply value from other if value from self is None.
    fn merge_option<T>(this: Option<T>, that: Option<T>) -> Option<T> {
        if this.is_none() && that.is_some() {
            that
        } else {
            this
        }
    }
}

pub trait HasImage {
    fn get_images(&self) -> &[ImageMeta];
}

/// Image size enumeration. Note to self: automatic derivation of Ord traits assumes
/// that the variants are declared in increasing order.
#[derive(Serialize, Deserialize, Debug, PartialEq, Eq, PartialOrd, Ord, Copy, Clone)]
pub enum ImageSize {
    Small,      // Around 32x32
    Medium,     // Around 64x64
    Large,      // Around 128x128
    ExtraLarge, // Around 256x256
    Mega,       // 512x512 or more
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ImageMeta {
    pub size: ImageSize,
    #[serde(rename = "#text")]
    pub url: String,
}

// Album
#[derive(Serialize, Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct Wiki {
    pub content: String,
    /// "Read more" URLs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub attribution: String, // Mandatory. If public domain or local-only, specify so explicitly.
}

// Standard (provider-agnostic) metadata structures for use across the app.
// All providers must return these structs instead of their own formats.
#[derive(Serialize, Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct AlbumMeta {
    pub name: String,
    pub artist: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    pub tags: Vec<Tag>,
    pub image: Vec<ImageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wiki: Option<Wiki>,
}

impl AlbumMeta {
    /// Create a minimal AlbumMeta. Useful for blocking further calls to an album
    /// whose information are unavailable on any remote source.
    pub fn from_key(key: &AlbumInfo) -> Self {
        Self {
            name: key.title.to_owned(),
            mbid: key.mbid.clone(),
            artist: None,
            tags: Vec::with_capacity(0),
            image: Vec::with_capacity(0),
            url: None,
            wiki: None,
        }
    }
}

impl Merge for AlbumMeta {
    fn merge(
        mut self,
        AlbumMeta {
            mbid,
            artist,
            mut tags,
            mut image,
            url,
            wiki,
            ..
        }: Self,
    ) -> Self {
        self.tags.append(&mut tags);
        self.image.append(&mut image);
        self.mbid = Self::merge_option(self.mbid, mbid);
        self.artist = Self::merge_option(self.artist, artist);
        self.url = Self::merge_option(self.url, url);
        self.wiki = Self::merge_option(self.wiki, wiki);
        self
    }
}

impl Tagged for AlbumMeta {
    fn get_tags(&self) -> &[Tag] {
        &self.tags
    }
}

impl HasImage for AlbumMeta {
    fn get_images(&self) -> &[ImageMeta] {
        &self.image
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct ArtistMeta {
    pub name: String,
    pub tags: Vec<Tag>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mbid: Option<String>,
    pub similar: Vec<ArtistMeta>,
    pub image: Vec<ImageMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bio: Option<Wiki>,
    // Leave as "Other" if unknown
    pub artist_type: ArtistType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
    // Founded, born, etc. If this is None, end_date shouldn't be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub begin_date: Option<NaiveDate>,
    // Dissolved, died, etc. Leave None for "ongoing" or "alive"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_date: Option<NaiveDate>,
    // Two-letter country code, such as US, AU, UK, JP, etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
}

impl ArtistMeta {
    /// Create a minimal ArtistMeta. Useful for blocking further calls to an artist
    /// whose information are unavailable on any remote source.
    pub fn from_key(key: &ArtistInfo) -> Self {
        Self {
            name: key.name.to_owned(),
            mbid: key.mbid.clone(),
            similar: Vec::with_capacity(0),
            tags: Vec::with_capacity(0),
            image: Vec::with_capacity(0),
            url: None,
            bio: None,
            artist_type: ArtistType::Other,
            gender: None,
            begin_date: None,
            end_date: None,
            country: None,
        }
    }
}

impl Merge for ArtistMeta {
    /// Add information from another ArtistMeta object.
    /// artist_type receives special treatment: Other and UnrecognizedArtistType are treated like None
    /// and will be replaced by what the other ArtistMeta object has, unless that other value is also
    /// one of those two.
    fn merge(
        mut self,
        ArtistMeta {
            mut tags,
            mut similar,
            mut image,
            url,
            bio,
            mbid,
            artist_type,
            gender,
            begin_date,
            end_date,
            country,
            ..
        }: Self,
    ) -> Self {
        self.tags.append(&mut tags);
        self.image.append(&mut image);
        self.similar.append(&mut similar);
        self.mbid = Self::merge_option(self.mbid, mbid);
        self.url = Self::merge_option(self.url, url);
        self.bio = Self::merge_option(self.bio, bio);
        if (self.artist_type == ArtistType::Other
            || self.artist_type == ArtistType::UnrecognizedArtistType)
            && (artist_type != ArtistType::Other
                && artist_type != ArtistType::UnrecognizedArtistType)
        {
            self.artist_type = artist_type;
        }
        self.gender = Self::merge_option(self.gender, gender);
        self.begin_date = Self::merge_option(self.begin_date, begin_date);
        self.end_date = Self::merge_option(self.end_date, end_date);
        self.country = Self::merge_option(self.country, country);
        self
    }
}

impl Tagged for ArtistMeta {
    fn get_tags(&self) -> &[Tag] {
        &self.tags
    }
}

impl HasImage for ArtistMeta {
    fn get_images(&self) -> &[ImageMeta] {
        &self.image
    }
}

pub struct Lyrics {
    pub lines: Vec<(f32, String)>, // timestamp (in seconds) and corresponding line. If not synced, set timestamp to 0.
    pub synced: bool,
}

#[derive(Debug, Clone)]
pub enum LyricsParseError {
    TimestampNotFoundError,
    TimestampFormatError
}

pub type LyricsResult = Result<Lyrics, LyricsParseError>;

impl Lyrics {
    pub fn try_from_plain_lrclib_str(lrclib: &str) -> LyricsResult {
        let lines: Vec<(f32, String)> = lrclib
            .split("\n")
            .map(|line| (0.0, line.to_owned()))
            .collect();
        Ok(Self {
            lines,
            synced: false,
        })
    }

    pub fn try_from_synced_lrclib_str(lrclib: &str) -> LyricsResult {
        let raw_lines: Vec<&str> = lrclib.split('\n').collect();
        let mut lines: Vec<(f32, String)> = Vec::with_capacity(raw_lines.len());
        for line in raw_lines.iter() {
            // Extract timestamp
            let ts_end_pos: usize = line
                .find(']')
                .ok_or(LyricsParseError::TimestampNotFoundError)?;
            let ts_str: &str = &line[1..ts_end_pos];
            let ts_parts: Vec<&str> = ts_str.split(':').collect();
            if ts_parts.len() != 2 {
                Err(LyricsParseError::TimestampFormatError)?;
            }
            let ts: f32 = ts_parts[0]
                .parse::<f32>()
                .map_err(|_| LyricsParseError::TimestampFormatError)? * 60.0
                + ts_parts[1]
                    .parse::<f32>()
                    .map_err(|_| LyricsParseError::TimestampFormatError)?;
            if line.len() <= ts_end_pos + 1 {
                lines.push((ts, "".to_owned()));
            } else {
                lines.push((ts, line[ts_end_pos + 1..].to_owned()));
            }
        }

        Ok(Self {
            lines,
            synced: true,
        })
    }

    pub fn to_string(&self) -> String {
        if self.synced {
            self.lines.iter().map(|line| {
            let total_seconds = line.0.max(0.0);
            let content = &line.1;

            let minutes = (total_seconds / 60.0).floor() as u32;
            let remaining_seconds = total_seconds % 60.0;
            // Extract the integer part of the seconds
            let seconds_integer = remaining_seconds.floor() as u32;

            // Extract the fractional part (hundredths of a second)
            // Multiply by 100, round to the nearest integer, and then cast to u32.
            // Using `round()` to handle potential floating-point inaccuracies
            // and ensure correct rounding for the hundredths.
            let hundredths = (remaining_seconds.fract() * 100.0).round() as u32;

            // Format the components into the desired string
            format!("[{:02}:{:02}.{:02}] {}", minutes, seconds_integer, hundredths, content)
        }).collect::<Vec<String>>().join("\n")
        }
        else {
            self.lines.iter().map(|line| line.1.as_str()).collect::<Vec<&str>>().join("\n")
        }

    }

    pub fn to_plain_string(&self) -> String {
        self.lines.iter().map(|line| line.1.as_ref()).collect::<Vec<&str>>().join("\n")
    }

    pub fn to_plain_lines(&self) -> Vec<&str> {
        self.lines.iter().map(|line| line.1.as_ref()).collect()
    }

    pub fn get_line_at_timestamp(&self, ts: f32) -> usize {
        if !self.synced {
            return 0;
        }
        match self.lines.binary_search_by(|line| {
            line.0.partial_cmp(&ts).unwrap()
        }) {
            Ok(index) => index, // Oh lucky
            Err(index) => {
                // Most of the time we'll hit this case because we're not looking
                // for an exact timestamp match
                if index > 0 {
                    index - 1
                }
                else {
                    0
                }
            }
        }
    }
}
