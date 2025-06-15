use crate::utils::meta_provider_settings;
use gtk::prelude::SettingsExt;
use musicbrainz_rs::entity::artist::ArtistType;
use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct LrcLibResponse {
    #[serde(rename = "#trackName")]
    pub title: String,
    pub duration: f32,
    #[serde(rename = "#plainLyrics")]
    pub plain: String,
    #[serde(rename = "#syncedLyrics")]
    pub synced: Option<String>,
}
