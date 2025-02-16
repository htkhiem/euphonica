use chrono::NaiveDate;
use musicbrainz_rs::entity::artist::ArtistType;
use serde::{Deserialize, Serialize};

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
    pub fn from_key(key: &bson::Document) -> Self {
        let mbid: Option<String>;
        if let Ok(mbid_str) = key.get_str("mbid") {
            mbid = Some(mbid_str.to_owned());
        } else {
            mbid = None;
        }
        Self {
            name: key.get_str("name").unwrap_or_default().to_string(),
            mbid,
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
    pub fn from_key(key: &bson::Document) -> Self {
        let mbid: Option<String>;
        if let Ok(mbid_str) = key.get_str("mbid") {
            mbid = Some(mbid_str.to_owned());
        } else {
            mbid = None;
        }
        Self {
            name: key.get_str("name").unwrap_or_default().to_string(),
            mbid,
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
