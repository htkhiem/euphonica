use std::borrow::Cow;

use mpd::{search::{Operation as TagOperation}, Query, Term};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StickerOperation {
    LessThan,
    GreaterThan,
    Contains,
    StartsWith,
    IntLessThan,
    IntGreaterThan
}

impl StickerOperation {
    pub fn to_mpd_syntax(&self) -> &'static str {
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

/// Flattened, no-lifetime version of mpd::search::Term * mpd::search::Operation,
/// only containing supported tag types.
#[derive(Serialize, Deserialize)]
pub enum QueryLhs {
    File,    // matches full song URI, always ==
    Base,    // from this directory
    LastMod, // since (>=)
    // Tags
    Any(TagOperation),  // will match any tag
    Album(TagOperation),
    AlbumArtist(TagOperation),
    Artist(TagOperation),
    // more to come
}

impl QueryLhs {
    /// Consume & add self into an existing mpd::search::Query.
    pub fn add_to_query(self, query: &mut Query, rhs: String) {
        let rhs = Cow::Owned(rhs);
        match self {
            Self::File => {
                query.and(Term::File, rhs);
            }
            Self::Base => {
                query.and(Term::Base, rhs);
            }
            Self::LastMod => {
                query.and(Term::LastMod, rhs);
            }
            Self::Any(op) => {
                query.and_with_op(Term::Any, op, rhs);
            }
            Self::Album(op) => {
                query.and_with_op(Term::Tag(Cow::Borrowed("album")), op, rhs);
            }
            Self::AlbumArtist(op) => {
                query.and_with_op(Term::Tag(Cow::Borrowed("albumartist")), op, rhs);
            }
            Self::Artist(op) => {
                query.and_with_op(Term::Tag(Cow::Borrowed("artist")), op, rhs);
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
pub enum Rule {
    /// LHS (key), operator, RHS (always a string)
    Sticker(String, StickerOperation, String),
    /// A subset of supported query operations.
    /// Optional LHS (tag or MPD term as string), MPD operation, right hand
    /// side (unary ops use this). We don't use mpd::search::Filter directly
    /// here to keep the Rule struct Send+Sync.
    Query(QueryLhs, String)
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
pub struct DynamicPlaylist {
    pub name: String,
    pub description: String,
    pub last_modified: String,
    pub last_queued: String,
    pub play_count: isize,
    pub rules: Vec<Rule>
}
