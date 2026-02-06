use crate::{
    meta_providers::models::{ImageMeta, ImageSize},
    utils::meta_provider_settings,
};
use chrono::NaiveDate;
use gtk::prelude::SettingsExt;
use musicbrainz_rs::entity::{
    artist::{Artist, ArtistType, Gender},
    relations::RelationContent,
    release::Release,
    tag::Tag,
};
use serde::Deserialize;

use super::{
    super::{models},
    PROVIDER_KEY,
};

// There are more details in the response, but we don't really care about them
#[derive(Deserialize)]
pub struct CoverArtResponse {
    pub images: Vec<CoverArtImage>
}

#[derive(Deserialize)]
pub struct CoverArtImage {
    pub thumbnails: CoverArtThumbnails
}

#[derive(Deserialize)]
pub struct CoverArtThumbnails {
    #[serde(rename = "500")]
    pub res_500: Option<String>,
}

fn transform_wikimedia_url(url: &str) -> Option<String> {
    // MusicBrainz relations cannot contain direct links, so we'll have to extract one ourselves.
    // For now hardcoding downloaded size to 256px wide. Images are usually in portrait orientation
    // so this should work fine.
    if let Some(file_name) = url.strip_prefix("https://commons.wikimedia.org/wiki/File:") {
        let transformed_url = format!(
            "https://commons.wikimedia.org/w/thumb.php?f={file_name}&w=256"
        );
        return Some(transformed_url);
    }
    None
}

impl From<Tag> for models::Tag {
    fn from(mbtag: Tag) -> Self {
        Self {
            name: mbtag.name,
            count: Some(mbtag.count),
            url: None,
        }
    }
}

// TODO: l10n
pub fn mb_gender_to_str(g: Gender) -> Option<String> {
    match g {
        Gender::Male => Some(String::from("male")),
        Gender::Female => Some(String::from("female")),
        Gender::Other => Some(String::from("other")),
        _ => None, // Company/legal entity, etc.
    }
}

impl From<Release> for models::AlbumMeta {
    fn from(rel: Release) -> Self {
        // TODO: Keep more of the data in Release.
        let artist_tag: Option<String>;
        if let Some(artists) = rel.artist_credit {
            let mut content = String::new();
            for artist in artists.iter() {
                content.push_str(&artist.name);
                // Spaces should already be included.
                // Last artist should not have one.
                if let Some(delim) = &artist.joinphrase {
                    content.push_str(delim);
                }
            }
            artist_tag = Some(content);
        } else {
            artist_tag = None;
        }
        let tags: Vec<models::Tag>;
        if let Some(mbtags) = rel.tags {
            tags = mbtags.into_iter().map(models::Tag::from).collect();
        } else {
            tags = Vec::new();
        }

        Self {
            name: rel.title,
            artist: artist_tag,
            mbid: Some(rel.id.clone()),
            tags,
            image: Vec::new(), // acquired separately
            url: Some(format!("https://musicbrainz.org/release/{}", rel.id)),
            wiki: None, // not provided
        }
    }
}

impl From<Artist> for models::ArtistMeta {
    fn from(artist: Artist) -> Self {
        // TODO: Keep more of the data in Artist.
        let tags: Vec<models::Tag>;
        if let Some(mbtags) = artist.tags {
            tags = mbtags.into_iter().map(models::Tag::from).collect();
        } else {
            tags = Vec::new();
        }
        let begin_date: Option<NaiveDate>;
        let end_date: Option<NaiveDate>;
        if let Some(lifespan) = artist.life_span {
            begin_date = lifespan.begin;
            if lifespan.ended.unwrap_or(false) {
                end_date = lifespan.end; // Might still be None
            } else {
                end_date = None;
            }
        } else {
            begin_date = None;
            end_date = None;
        }
        // Currently we only support downloading images via the "image" relation type.
        let mut image: Vec<ImageMeta>;
        if meta_provider_settings(PROVIDER_KEY).boolean("download-artist-avatar") {
            image = Vec::new();
            if let Some(relations) = artist.relations {
                for relation in relations.into_iter() {
                    if relation.relation_type == "image" || relation.relation_type == "picture" {
                        if let RelationContent::Url(url) = relation.content {
                            if let Some(direct) = transform_wikimedia_url(&url.resource) {
                                image.push(ImageMeta {
                                    size: ImageSize::Large,
                                    url: direct,
                                });
                            }
                        }
                    }
                }
            }
        } else {
            image = Vec::with_capacity(0);
        }

        Self {
            name: artist.name,
            tags,
            mbid: Some(artist.id.clone()),
            similar: Vec::new(),
            image,
            url: Some(format!("https://musicbrainz.org/artist/{}", artist.id)),
            bio: None,
            artist_type: artist.artist_type.unwrap_or(ArtistType::Other),
            gender: mb_gender_to_str(artist.gender.unwrap_or(Gender::NotApplicable)),
            begin_date,
            end_date,
            country: artist.country,
        }
    }
}

impl From<CoverArtImage> for models::ImageMeta {
    fn from(cover: CoverArtImage) -> Self {
        return models::ImageMeta {
            // This is kind of a decision, but we use the 5xx size & not the original
            // We only really need the 5xx size, so this isn't really that big of a problem
            size: models::ImageSize::ExtraLarge,
            // docs say that they are always present
            url: cover.thumbnails.res_500.unwrap(),
        }
    }
}
