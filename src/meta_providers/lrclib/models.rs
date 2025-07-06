use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
#[non_exhaustive]
pub struct LrcLibResponse {
    #[serde(rename = "trackName")]
    pub title: String,
    pub duration: f32,
    #[serde(rename = "plainLyrics")]
    pub plain: String,
    #[serde(rename = "syncedLyrics")]
    pub synced: Option<String>,
}
