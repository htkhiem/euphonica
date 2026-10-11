use crate::utils::meta_provider_settings;
use chrono::NaiveDate;
use gtk::prelude::SettingsExt;
use musicbrainz_rs::entity::artist::ArtistType;
use serde::Deserialize;

use super::{
    super::models::{AlbumMeta, ArtistMeta, ImageMeta, ImageSize, Tag, Wiki},
    PROVIDER_KEY,
};

// Album
#[derive(Deserialize, Debug, Default)]
#[non_exhaustive]
pub struct TheAudioDbAlbum {
    // for URL construction
    #[serde(rename = "idAlbum")]
    pub id: String,
    #[serde(rename = "strArtistStripped")]
    pub artist: String,
    #[serde(rename = "strMusicBrainzID")]
    pub mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strAlbumThumb")]
    pub image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strAlbumThumbHQ")]
    pub image_hq: Option<String>,
    #[serde(rename = "strAlbumStripped")]
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strDescription")]
    pub wiki: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strMood")]
    pub mood: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strTheme")]
    pub theme: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strSpeed")]
    pub speed: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strUPCID")]
    pub upc_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strSpotifyID")]
    pub spotify_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strMusicMozID")]
    pub musicmoz_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strGeniusID")]
    pub genius_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strLabel")]
    pub label: Option<String>,
    // TODO: display these too next to the tagged genres
    #[serde(skip_serializing_if = "Option::is_none", rename = "strGenre")]
    pub genre: Option<String>,
}

impl From<TheAudioDbAlbum> for AlbumMeta {
    fn from(tada: TheAudioDbAlbum) -> Self {
        // tadaah :)
        let mut images = Vec::new();
        if let Some(url) = tada.image {
            images.push(ImageMeta {
                size: ImageSize::Medium,
                url,
            });
        }
        if let Some(url) = tada.image_hq {
            images.push(ImageMeta {
                size: ImageSize::ExtraLarge, // TADB's HQ size is 4K
                url,
            });
        }
        let url = format!("www.theaudiodb.com/album/{}", tada.id);
        Self {
            name: tada.name,
            artist: Some(tada.artist),
            mbid: tada.mbid,
            tags: Vec::with_capacity(0),
            image: images,
            url: Some(url.clone()),
            wiki: tada.wiki.map(move |content| Wiki {
                content,
                url: Some(url),
                attribution: String::from(
                    "TheAudioDB Terms of Use (https://www.theaudiodb.com/docs_terms_of_use.php)",
                ),
            }),
            mood: tada.mood,
            theme: tada.theme,
            speed: tada.speed,
            upc_id: tada.upc_id,
            spotify_id: tada.spotify_id,
            musicmoz_id: tada.musicmoz_id,
            genius_id: tada.genius_id,
            label: tada.label,
            genres: if let Some(genre) = tada.genre {
                vec![genre]
            } else {
                Vec::with_capacity(0)
            },
        }
    }
}

// The album struct itself is also nested in a root object with
// a single field "album".
#[derive(Deserialize, Default)]
pub struct TheAudioDbAlbumResponse {
    pub album: Vec<TheAudioDbAlbum>,  // this one is named in singular, bravo
}

#[derive(Deserialize, Debug, Default)]
#[non_exhaustive]
pub struct TheAudioDbArtist {
    // for URL construction
    #[serde(rename = "idArtist")]
    pub id: String,
    #[serde(rename = "strArtistStripped")]
    pub name: String,
    #[serde(rename = "strMusicBrainzID")]
    pub mbid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strWebsite")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strBiography")]
    pub bio: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strFormedYear")]
    pub formed: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strBornYear")]
    pub born: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strDisbanded")]
    pub disbanded: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strDiedYear")]
    pub died: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strGender")]
    pub gender: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strCountryCode")]
    pub country: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strArtistThumb")]
    pub thumb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strArtistCutout")]
    pub cutout: Option<String>,
    // TODO: support displaying this in UI as fancier name too. For now it's the third-preferred image.
    #[serde(skip_serializing_if = "Option::is_none", rename = "strArtistLogo")]
    pub logo: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strMood")]
    pub mood: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "strGenre")]
    pub genre: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "intMembers")]
    pub members: Option<String>,
}

impl From<TheAudioDbArtist> for ArtistMeta {
    fn from(tada: TheAudioDbArtist) -> Self {
        let tadb_url = format!("www.theaudiodb.com/artist/{}", tada.id);
        ArtistMeta {
            name: tada.name,
            mbid: tada.mbid,
            tags: Vec::with_capacity(0),
            similar: Vec::with_capacity(0),
            image: tada
                .thumb
                .or(tada.cutout)
                .or_else(|| tada.logo.clone())
                .map(|url| {
                    vec![ImageMeta {
                        size: ImageSize::Large,
                        url,
                    }]
                })
                .unwrap_or_else(|| Vec::with_capacity(0)),
            url: tada.url.or_else(|| Some(tadb_url.clone())),
            bio: tada.bio.map(move |content| Wiki {
                content,
                url: Some(tadb_url),
                attribution: String::from(
                    "TheAudioDB Terms of Use (https://www.theaudiodb.com/docs_terms_of_use.php)",
                ),
            }),
            artist_type: if let Some(members_str) = tada.members {
                if let Ok(mem_count) = members_str.parse::<i32>() {
                    if mem_count > 1 {
                        ArtistType::Group
                    } else {
                        ArtistType::Person
                    }
                } else {
                    ArtistType::UnrecognizedArtistType
                }
            } else {
                ArtistType::UnrecognizedArtistType
            },
            gender: tada.gender,
            begin_date: tada
                .formed
                .or(tada.born)
                .map(|yr| NaiveDate::from_ymd_opt(yr, 1, 1))
                .flatten(),
            end_date: tada
                .disbanded
                .or(tada.died)
                .map(|yr| NaiveDate::from_ymd_opt(yr, 1, 1))
                .flatten(),
            country: tada.country.map(|code| code.to_uppercase()),
            logo_url: tada.logo,
            mood: tada.mood,
            genres: if let Some(genre) = tada.genre {
                vec![genre]
            } else {
                Vec::with_capacity(0)
            },
        }
    }
}

// The artist struct itself is also nested in a root obtada with
// a single field "artist".
#[derive(Deserialize, Default)]
pub struct TheAudioDbArtistResponse {
    pub artists: Vec<TheAudioDbArtist>,  // this one is plural...
}
