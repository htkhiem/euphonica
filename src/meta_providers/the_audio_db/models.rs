use chrono::NaiveDate;
use gtk::prelude::SettingsExt;
use musicbrainz_rs::entity::artist::ArtistType;
use serde::Deserialize;

use super::{
    super::models::{AlbumMeta, ArtistMeta, ImageMeta, ImageSize, Wiki},
    PROVIDER_KEY,
};
use crate::utils::meta_provider_settings;

/// TheAudioDB v1 returns `null` for missing fields, and sometimes empty strings.
/// Normalize both to None.
fn some_nonempty(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_owned())
    }
}

fn some_nonempty_opt(s: &Option<String>) -> Option<String> {
    s.as_deref().and_then(some_nonempty)
}

/// TheAudioDB returns numeric year fields inconsistently as either JSON numbers
/// or strings depending on the endpoint.
fn deserialize_year<'de, D>(deserializer: D) -> Result<Option<i32>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Year {
        Num(Option<i32>),
        Str(Option<String>),
    }
    Ok(match Year::deserialize(deserializer)? {
        Year::Num(n) => n,
        Year::Str(s) => s.and_then(|s| s.parse::<i32>().ok()),
    })
}

// Album
#[derive(Deserialize, Debug, Default)]
#[non_exhaustive]
pub struct TheAudioDbAlbum {
    // for URL construction
    #[serde(rename = "idAlbum")]
    pub id: String,
    #[serde(rename = "strArtistStripped")]
    pub artist: Option<String>,
    #[serde(rename = "strArtist")]
    pub artist_raw: Option<String>,
    #[serde(rename = "strMusicBrainzID")]
    pub mbid: Option<String>,
    #[serde(rename = "strAlbumThumb")]
    pub image: Option<String>,
    #[serde(rename = "strAlbumThumbHQ")]
    pub image_hq: Option<String>,
    #[serde(rename = "strAlbumStripped")]
    pub name: Option<String>,
    #[serde(rename = "strAlbum")]
    pub name_raw: Option<String>,
    #[serde(rename = "strDescription")]
    pub wiki: Option<String>,
    #[serde(rename = "strMood")]
    pub mood: Option<String>,
    #[serde(rename = "strTheme")]
    pub theme: Option<String>,
    #[serde(rename = "strSpeed")]
    pub speed: Option<String>,
    #[serde(rename = "strUPCID")]
    pub upc_id: Option<String>,
    #[serde(rename = "strSpotifyID")]
    pub spotify_id: Option<String>,
    #[serde(rename = "strMusicMozID")]
    pub musicmoz_id: Option<String>,
    #[serde(rename = "strGeniusID")]
    pub genius_id: Option<String>,
    #[serde(rename = "strLabel")]
    pub label: Option<String>,
    // TODO: display these too next to the tagged genres
    #[serde(rename = "strGenre")]
    pub genre: Option<String>,
}

impl From<TheAudioDbAlbum> for AlbumMeta {
    fn from(tada: TheAudioDbAlbum) -> Self {
        let settings = meta_provider_settings(PROVIDER_KEY);
        let mut images = Vec::with_capacity(0);
        if settings.boolean("download-album-art") {
            if let Some(url) = some_nonempty_opt(&tada.image) {
                images.push(ImageMeta {
                    size: ImageSize::Medium,
                    url,
                });
            }
            if let Some(url) = some_nonempty_opt(&tada.image_hq) {
                images.push(ImageMeta {
                    size: ImageSize::ExtraLarge, // TADB's HQ size is 4K
                    url,
                });
            }
        }
        let name = some_nonempty_opt(&tada.name)
            .or_else(|| some_nonempty_opt(&tada.name_raw))
            .unwrap_or_default();
        let artist =
            some_nonempty_opt(&tada.artist).or_else(|| some_nonempty_opt(&tada.artist_raw));
        let url = format!("www.theaudiodb.com/album/{}", tada.id);
        Self {
            name,
            artist,
            mbid: some_nonempty_opt(&tada.mbid),
            tags: Vec::with_capacity(0),
            image: images,
            url: Some(url.clone()),
            wiki: some_nonempty_opt(&tada.wiki).map(move |content| Wiki {
                content,
                url: Some(url),
                attribution: String::from(
                    "TheAudioDB Terms of Use (https://www.theaudiodb.com/docs_terms_of_use.php)",
                ),
            }),
            mood: some_nonempty_opt(&tada.mood),
            theme: some_nonempty_opt(&tada.theme),
            speed: some_nonempty_opt(&tada.speed),
            upc_id: some_nonempty_opt(&tada.upc_id),
            spotify_id: some_nonempty_opt(&tada.spotify_id),
            musicmoz_id: some_nonempty_opt(&tada.musicmoz_id),
            genius_id: some_nonempty_opt(&tada.genius_id),
            label: some_nonempty_opt(&tada.label),
            genres: if let Some(genre) = some_nonempty_opt(&tada.genre) {
                vec![genre]
            } else {
                Vec::with_capacity(0)
            },
        }
    }
}

// The album struct itself is also nested in a root object with
// a single field "album". TheAudioDB returns `null` (rather than an empty
// array) when the search yields no results.
#[derive(Deserialize, Debug, Default)]
pub struct TheAudioDbAlbumResponse {
    pub album: Option<Vec<TheAudioDbAlbum>>, // this one is named in singular, bravo
}

#[derive(Deserialize, Debug, Default)]
#[non_exhaustive]
pub struct TheAudioDbArtist {
    // for URL construction
    #[serde(rename = "idArtist")]
    pub id: String,
    #[serde(rename = "strArtistStripped")]
    pub name: Option<String>,
    #[serde(rename = "strArtist")]
    pub name_raw: Option<String>,
    #[serde(rename = "strMusicBrainzID")]
    pub mbid: Option<String>,
    #[serde(rename = "strWebsite")]
    pub url: Option<String>,
    #[serde(rename = "strBiography")]
    pub bio: Option<String>,
    #[serde(rename = "intFormedYear", deserialize_with = "deserialize_year")]
    pub formed: Option<i32>,
    #[serde(rename = "intBornYear", deserialize_with = "deserialize_year")]
    pub born: Option<i32>,
    #[serde(rename = "strDisbanded")]
    pub disbanded: Option<String>,
    #[serde(rename = "intDiedYear", deserialize_with = "deserialize_year")]
    pub died: Option<i32>,
    #[serde(rename = "strGender")]
    pub gender: Option<String>,
    #[serde(rename = "strCountryCode")]
    pub country: Option<String>,
    #[serde(rename = "strArtistThumb")]
    pub thumb: Option<String>,
    #[serde(rename = "strArtistWideThumb")]
    pub wide_thumb: Option<String>,
    #[serde(rename = "strArtistCutout")]
    pub cutout: Option<String>,
    // TODO: support displaying this in UI as fancier name too. For now it's the third-preferred image.
    #[serde(rename = "strArtistLogo")]
    pub logo: Option<String>,
    #[serde(rename = "strMood")]
    pub mood: Option<String>,
    #[serde(rename = "strGenre")]
    pub genre: Option<String>,
    #[serde(rename = "intMembers")]
    pub members: Option<String>,
}

impl From<TheAudioDbArtist> for ArtistMeta {
    fn from(tada: TheAudioDbArtist) -> Self {
        let settings = meta_provider_settings(PROVIDER_KEY);
        let name = some_nonempty_opt(&tada.name)
            .or_else(|| some_nonempty_opt(&tada.name_raw))
            .unwrap_or_default();
        let mut images = Vec::with_capacity(0);
        if settings.boolean("download-artist-avatar") {
            if let Some(url) = some_nonempty_opt(&tada.thumb) {
                images.push(ImageMeta {
                    size: ImageSize::Medium,
                    url,
                });
            }
            if let Some(url) = some_nonempty_opt(&tada.wide_thumb) {
                images.push(ImageMeta {
                    size: ImageSize::Large,
                    url,
                });
            }
            if let Some(url) = some_nonempty_opt(&tada.cutout) {
                images.push(ImageMeta {
                    size: ImageSize::Large,
                    url,
                });
            }
        }
        let tadb_url = format!("www.theaudiodb.com/artist/{}", tada.id);
        ArtistMeta {
            name,
            mbid: some_nonempty_opt(&tada.mbid),
            tags: Vec::with_capacity(0),
            similar: Vec::with_capacity(0),
            image: images,
            url: some_nonempty_opt(&tada.url).or_else(|| Some(tadb_url.clone())),
            bio: some_nonempty_opt(&tada.bio).map(move |content| Wiki {
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
                    } else if mem_count == 1 {
                        ArtistType::Person
                    } else {
                        ArtistType::UnrecognizedArtistType
                    }
                } else {
                    ArtistType::UnrecognizedArtistType
                }
            } else {
                ArtistType::UnrecognizedArtistType
            },
            gender: some_nonempty_opt(&tada.gender),
            begin_date: tada
                .formed
                .or(tada.born)
                .and_then(|yr| NaiveDate::from_ymd_opt(yr, 1, 1)),
            end_date: tada
                .disbanded
                .and_then(|s| s.parse::<i32>().ok())
                .or(tada.died)
                .and_then(|yr| NaiveDate::from_ymd_opt(yr, 1, 1)),
            country: some_nonempty_opt(&tada.country).map(|code| code.to_uppercase()),
            logo_url: some_nonempty_opt(&tada.logo),
            mood: some_nonempty_opt(&tada.mood),
            genres: if let Some(genre) = some_nonempty_opt(&tada.genre) {
                vec![genre]
            } else {
                Vec::with_capacity(0)
            },
        }
    }
}

// The artist struct itself is also nested in a root object with
// a single field "artists". TheAudioDB returns `null` (rather than an empty
// array) when the search yields no results.
#[derive(Deserialize, Debug, Default)]
pub struct TheAudioDbArtistResponse {
    pub artists: Option<Vec<TheAudioDbArtist>>, // this one is plural...
}

#[cfg(test)]
mod tests {
    use super::*;

    /// TheAudioDB returns `null` rather than an empty array when the search
    /// yields no results.
    #[test]
    fn album_response_null() {
        let resp: TheAudioDbAlbumResponse = serde_json::from_str(r#"{"album": null}"#).unwrap();
        assert!(resp.album.is_none());
    }

    #[test]
    fn album_response_present() {
        // Truncated real response from
        // https://www.theaudiodb.com/api/v1/json/test-key/searchalbum.php?s=Coldplay&a=Parachutes
        let json = r#"{
            "album": [{
                "idAlbum": "2109615",
                "strArtist": "Coldplay",
                "strArtistStripped": "Coldplay",
                "strMusicBrainzID": "1dc4c347-a1db-32aa-b14f-bc9cc507b843",
                "strAlbumThumb": "https://r2.theaudiodb.com/images/media/album/thumb/swxywp1367234202.jpg",
                "strAlbumThumbHQ": null,
                "strAlbum": "Parachutes",
                "strAlbumStripped": "Parachutes",
                "strDescription": "Parachutes is the debut album by Coldplay.",
                "strMood": "Relaxed",
                "strTheme": "In Love",
                "strSpeed": "Slow",
                "strUPCID": "0067003016254",
                "strSpotifyID": "6ZG5lRT77aJ3btmArcykra",
                "strMusicMozID": "Parachutes",
                "strGeniusID": "Parachutes",
                "strLabel": "Parlophone UK",
                "strGenre": "Pop-Rock"
            }]
        }"#;
        let resp: TheAudioDbAlbumResponse = serde_json::from_str(json).unwrap();
        let album = resp.album.unwrap().remove(0);
        assert_eq!(album.id, "2109615");
        assert_eq!(album.artist.as_deref(), Some("Coldplay"));
        assert_eq!(
            album.mbid.as_deref(),
            Some("1dc4c347-a1db-32aa-b14f-bc9cc507b843")
        );
        assert_eq!(album.name.as_deref(), Some("Parachutes"));
        assert!(album.image.is_some());
        assert!(album.image_hq.is_none());
        assert!(album.wiki.is_some());
        assert_eq!(album.genre.as_deref(), Some("Pop-Rock"));
    }

    #[test]
    fn artist_response_null() {
        let resp: TheAudioDbArtistResponse = serde_json::from_str(r#"{"artists": null}"#).unwrap();
        assert!(resp.artists.is_none());
    }

    #[test]
    fn artist_response_present() {
        // Truncated real response from
        // https://www.theaudiodb.com/api/v1/json/test-key/artist-mb.php?i=cc197bad-...
        // Note strArtistStripped is null in the real data.
        let json = r#"{
            "artists": [{
                "idArtist": "111239",
                "strArtist": "Coldplay",
                "strArtistStripped": null,
                "strMusicBrainzID": "cc197bad-dc9c-440d-a5b5-d52ba2e14234",
                "strWebsite": "",
                "strBiography": "Coldplay are a British rock band formed in London in 1997.",
                "intFormedYear": 1996,
                "intBornYear": 1996,
                "strDisbanded": null,
                "intDiedYear": null,
                "strGender": "Male",
                "strCountryCode": "GB",
                "strArtistThumb": "https://r2.theaudiodb.com/images/media/artist/thumb/uxrqxy13.jpg",
                "strArtistWideThumb": "https://r2.theaudiodb.com/images/media/artist/widethumb/sxqs.jpg",
                "strArtistCutout": "https://r2.theaudiodb.com/images/media/artist/cutout/ggq5ap1.jpg",
                "strArtistLogo": "https://r2.theaudiodb.com/images/media/artist/logo/q094e2166.jpg",
                "strMood": "Happy",
                "strGenre": "Alternative Rock",
                "intMembers": "4"
            }]
        }"#;
        let resp: TheAudioDbArtistResponse = serde_json::from_str(json).unwrap();
        let artist = resp.artists.unwrap().remove(0);
        assert_eq!(artist.id, "111239");
        // strArtistStripped is null in real data; fallback to strArtist
        assert!(artist.name.is_none());
        assert_eq!(artist.name_raw.as_deref(), Some("Coldplay"));
        assert_eq!(
            artist.mbid.as_deref(),
            Some("cc197bad-dc9c-440d-a5b5-d52ba2e14234")
        );
        // TheAudioDB returns an empty string rather than null for some fields;
        // From<ArtistMeta> normalizes this to None.
        assert_eq!(artist.url.as_deref(), Some(""));
        assert!(artist.bio.is_some());
        assert_eq!(artist.members.as_deref(), Some("4"));
        assert!(artist.thumb.is_some());
        assert!(artist.logo.is_some());
        assert_eq!(artist.country.as_deref(), Some("GB"));
    }

    /// Some endpoints return the year fields as strings instead of numbers.
    #[test]
    fn artist_response_string_years() {
        let json = r#"{
            "artists": [{
                "idArtist": "111239",
                "strArtist": "Coldplay",
                "intFormedYear": "1996",
                "intBornYear": "1996",
                "strDisbanded": "",
                "intDiedYear": "",
                "intMembers": "4"
            }]
        }"#;
        let resp: TheAudioDbArtistResponse = serde_json::from_str(json).unwrap();
        let artist = resp.artists.unwrap().remove(0);
        assert_eq!(artist.formed, Some(1996));
        assert_eq!(artist.born, Some(1996));
        // Empty string disbanded year should parse to None
        assert_eq!(artist.disbanded.as_deref(), Some(""));
        assert_eq!(artist.died, None);
    }
}
