use mpd::search::{Query};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StickerOp {
    #[serde()]
    LessThan,
    GreaterThan,
    Contains,
    StartsWith,
    IntLessThan,
    IntGreaterThan
}

impl StickerOp {
    pub fn op(&self) -> &'static str {
        match self {
            Self::LessThan => "<",
            Self::GreaterThan => ">",
            Self::Contains => "contains",
            Self::StartsWith => "starts_with",
            Self::IntLessThan => "lt",
            Self::IntGreaterThan => "gt"
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct Rules<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<Query<'a>>,
    /// RHS is always a string. If should be treated as numeric, use the relevant StickerOps.
    sticker_conditions: Vec<(String, StickerOp, String)>,
}

/// Dynamic playlist struct.
///
/// MPD's protocol provides for two distinct types of dynamic playlists (DPs):
/// - Search by ONE sticker. This accepts less-than and greater-than comparisons in addition to ==.
///   Usually used to implement most-listened or ratings-based filtering.
/// - Search by query. This includes searching by tags, creation time, etc. Multiple clauses can be
///   ANDed together. Does not support filtering by sticker values.
///
/// Euphonica's approach to DPs combines both types, allowing for multiple stickers-
/// based conditions alongside a traditional query in the same DP. In other words,
/// there is NO distinction made between the above two types in the UI.
///
/// To implement the above, we store both a query and a set of sticker condition triples (sticker
/// key, operator, value). Both are serialised together as a BSON blob in SQLite. JSON export
/// can be added later.
/// sticker conditions. To query a DP, we perform an intersection between sets of URIs
/// returned by the query and each of the sticker conditions.
#[derive(Serialize, Deserialize)]
pub struct DynamicPlaylist<'a> {
    pub name: String,
    pub description: String,
    pub last_modified: String,
    pub last_queued: String,
    pub play_count: isize,
    pub rules: Rules<'a>
}
