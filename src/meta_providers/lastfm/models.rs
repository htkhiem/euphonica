use crate::utils::meta_provider_settings;
use gtk::prelude::SettingsExt;
use musicbrainz_rs::entity::artist::ArtistType;
use serde::Deserialize;

use super::{
    super::models::{AlbumMeta, ArtistMeta, ImageMeta, ImageSize, Tag, Wiki},
    PROVIDER_KEY,
};
// Last.fm JSON structs, for deserialising API responses only.
// Widgets should use the standard structs defined in the supercrate's models.rs.

// For some reason the taglist resides in a nested "tag" object.
// Or, for cases with no tags, Last.fm would return an empty string.
#[derive(Deserialize, Debug)]
pub struct NestedTagList {
    tag: Vec<Tag>,
}
#[derive(Deserialize, Debug)]
#[serde(untagged)]
pub enum TagsHelper {
    String(String),
    Nested(NestedTagList),
}

// fn deserialize_tags<'de, D>(deserializer: D) -> Result<Vec<Tag>, D::Error>
// where
//     D: Deserializer<'de>,
// {
//     let helper: TagsHelper = Deserialize::deserialize(deserializer)?;
//     match helper {
//         TagsHelper::String(_) => Ok(Vec::with_capacity(0)),
//         TagsHelper::Nested(nested) => Ok(nested.tag)
//     }
// }
// fn serialize_tags<S>(tags: &[Tag], serializer: S) -> Result<S::Ok, S::Error>
// where
//     S: Serializer,
// {
//     if tags.is_empty() {
//         let helper = TagsHelper::String("".to_owned());
//         helper.serialize(serializer)
//     }
//     else {
//         let helper = TagsHelper::Nested(
//             NestedTagList {
//                 tag: tags.to_owned()
//             }
//         );
//         helper.serialize(serializer)
//     }
// }

#[derive(Deserialize, Debug, Clone)]
pub struct LastfmImage {
    pub size: String,
    #[serde(rename = "#text")]
    pub url: String,
}

impl From<LastfmImage> for ImageMeta {
    fn from(img: LastfmImage) -> Self {
        let size: ImageSize = match img.size.as_ref() {
            "small" => ImageSize::Small,
            "medium" => ImageSize::Medium,
            "large" => ImageSize::Large,
            "extralarge" => ImageSize::Large, // Last.fm album arts only go up to 300x300
            "mega" => ImageSize::Large,       // Last.fm album arts only go up to 300x300
            _ => ImageSize::Large,            // Last.fm album arts only go up to 300x300
        };
        ImageMeta { size, url: img.url }
    }
}

// Album wiki or artist bio (same structure)
#[derive(Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct LastfmWiki {
    pub content: String,
}

impl Into<Wiki> for LastfmWiki {
    fn into(self) -> Wiki {
        // Last.fm text content are not escaped (i.e. ampersands are kept verbatim)
        // YET they also contain <a> tags.
        // Treat the last a tag as the "Read more" link
        match self.content.rfind("<a href") {
            Some(href_start_idx) => {
                if let Some(href_end_idx) = self.content.rfind("</a>.") {
                    let atag: &str = &self.content[href_start_idx..href_end_idx];
                    Wiki {
                        content: self.content[..href_start_idx].trim().to_owned(),
                        url: Some(atag[(atag.find('"').unwrap() + 1)..atag.rfind('"').unwrap()].trim().to_owned()),
                        attribution: self.content[(href_end_idx + 5)..].trim().to_owned()
                    }
                }
                else {
                    // Invalid format. Only the main content is safe for display.
                    // Hardcode attribution since we cannot parse that bit.
                    Wiki {
                        content: self.content[..href_start_idx].to_owned(),
                        url: None,
                        attribution: "Unable to parse licensing information for this text. Please refer to Last.fm's ToS for licensing terms.".to_owned()
                    }
                }
            }
            None => Wiki {
                content: self.content,
                url: None,
                attribution: "Unable to parse licensing information for this text. Please refer to Last.fm's ToS for licensing terms.".to_owned()
            }
        }
    }
}

// Album
#[derive(Deserialize, Debug)]
#[non_exhaustive]
pub struct LastfmAlbum {
    pub artist: String,
    // If queried using mbid, it won't be returned again
    pub mbid: Option<String>,
    pub tags: TagsHelper,
    pub image: Vec<LastfmImage>,
    pub url: String,
    pub name: String,
    pub wiki: Option<LastfmWiki>,
}

impl From<LastfmAlbum> for AlbumMeta {
    fn from(mut lfm: LastfmAlbum) -> Self {
        let tags: Vec<Tag> = match lfm.tags {
            TagsHelper::String(_) => Vec::with_capacity(0),
            TagsHelper::Nested(obj) => obj.tag,
        };
        let image: Vec<ImageMeta>;
        if meta_provider_settings(PROVIDER_KEY).boolean("download-album-art") {
            image = lfm.image.drain(0..).map(LastfmImage::into).collect();
        } else {
            image = Vec::with_capacity(0);
        }
        let wiki: Option<Wiki> = match lfm.wiki {
            Some(w) => Some(w.into()),
            None => None,
        };

        Self {
            name: lfm.name,
            artist: Some(lfm.artist),
            mbid: lfm.mbid,
            tags,
            image,
            url: Some(lfm.url),
            wiki,
        }
    }
}

// The album struct itself is also nested in a root object with
// a single field "album".
#[derive(Deserialize)]
pub struct LastfmAlbumResponse {
    pub album: LastfmAlbum,
}

// Artist
#[derive(Deserialize, Debug)]
#[non_exhaustive]
pub struct LastfmSimilarArtist {
    pub name: String,
    pub url: String,
    pub image: Vec<LastfmImage>,
}

impl Into<ArtistMeta> for LastfmSimilarArtist {
    fn into(mut self) -> ArtistMeta {
        let image: Vec<ImageMeta> = self.image.drain(..).map(LastfmImage::into).collect();
        ArtistMeta {
            name: self.name,
            mbid: None,
            tags: Vec::with_capacity(0),
            similar: Vec::with_capacity(0),
            image,
            url: Some(self.url),
            bio: None,
            artist_type: ArtistType::UnrecognizedArtistType,
            gender: None,
            begin_date: None,
            end_date: None,
            country: None,
        }
    }
}

#[derive(Deserialize, Debug)]
#[non_exhaustive]
pub struct LastfmSimilar {
    pub artist: Vec<LastfmSimilarArtist>,
}

#[derive(Deserialize, Debug)]
#[non_exhaustive]
pub struct LastfmArtist {
    pub name: String,
    // If queried using mbid, it won't be returned again
    pub mbid: Option<String>,
    pub url: String,
    // Do not parse artist URLs - Last.fm no longer allows artist images to be fetched
    // via their API.
    pub similar: Option<LastfmSimilar>,
    pub tags: TagsHelper,
    pub bio: Option<LastfmWiki>,
}

impl Into<ArtistMeta> for LastfmArtist {
    fn into(self) -> ArtistMeta {
        let tags: Vec<Tag> = match self.tags {
            TagsHelper::String(_) => Vec::with_capacity(0),
            TagsHelper::Nested(obj) => obj.tag,
        };
        let similar: Vec<ArtistMeta>;
        if let Some(mut s) = self.similar {
            similar = s.artist.drain(..).map(LastfmSimilarArtist::into).collect();
        } else {
            similar = Vec::new();
        }
        let bio: Option<Wiki> = match self.bio {
            Some(w) => Some(w.into()),
            None => None,
        };

        ArtistMeta {
            name: self.name,
            mbid: self.mbid,
            tags,
            similar,
            image: Vec::with_capacity(0),
            url: Some(self.url),
            bio,
            artist_type: ArtistType::UnrecognizedArtistType,
            gender: None,
            begin_date: None,
            end_date: None,
            country: None,
        }
    }
}

// The artist struct itself is also nested in a root object with
// a single field "artist".
#[derive(Deserialize)]
pub struct LastfmArtistResponse {
    pub artist: LastfmArtist,
}
