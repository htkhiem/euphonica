use std::borrow::Cow;
use std::str::FromStr;

use mpd::{Query, Term, search::Operation as TagOperation};
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use strum::EnumCount;
use strum_macros::{EnumCount as EnumCountMacro, EnumIter, VariantArray};

#[derive(
    Serialize,
    Deserialize,
    Debug,
    Copy,
    Clone,
    EnumIter,
    EnumCountMacro,
    VariantArray,
    PartialEq,
    Eq,
)]
pub enum Ordering {
    AscAlbumTitle,
    DescAlbumTitle,
    Track,
    AscArtistTag,
    DescArtistTag,
    AscReleaseDate,
    DescReleaseDate,
    AscRating,
    DescRating,
    AscLastModified,
    DescLastModified,
    AscPlayCount,
    DescPlayCount,
    AscSkipCount,
    DescSkipCount,
    Random,
}

impl Ordering {
    pub fn model() -> &'static [&'static str] {
        // TODO: gettext
        // Enforce type to remind myself to add a name here for every new enum type
        static MODEL: Lazy<[&str; Ordering::COUNT]> = Lazy::new(|| {
            [
                "Asc. album title",
                "Desc. album title",
                "Track number",
                "Asc. artist tag",
                "Desc. artist tag",
                "Asc. release date",
                "Desc. release date",
                "Asc. rating",
                "Desc. rating",
                "Asc. last modified",
                "Desc. last modified",
                "Asc. play count",
                "Desc. play count",
                "Asc. skip count",
                "Desc. skip count",
                "Random", // Keep this the last option please
            ]
        });

        MODEL.as_ref()
    }

    pub fn readable_name(&self) -> &'static str {
        Self::model()[*self as usize]
    }

    /// Some orderings have an inverse version. Figuring this out is useful
    /// for input sanitising (i.e. preventing both asc. rating and desc. rating
    /// from being specified together).
    pub fn reverse(&self) -> Option<Self> {
        match self {
            Self::AscAlbumTitle => Some(Self::DescAlbumTitle),
            Self::DescAlbumTitle => Some(Self::AscAlbumTitle),
            Self::AscReleaseDate => Some(Self::DescReleaseDate),
            Self::DescReleaseDate => Some(Self::AscReleaseDate),
            Self::AscArtistTag => Some(Self::DescArtistTag),
            Self::DescArtistTag => Some(Self::AscArtistTag),
            Self::AscRating => Some(Self::DescRating),
            Self::DescRating => Some(Self::AscRating),
            Self::AscLastModified => Some(Self::DescLastModified),
            Self::DescLastModified => Some(Self::AscLastModified),
            Self::AscPlayCount => Some(Self::DescPlayCount),
            Self::DescPlayCount => Some(Self::AscPlayCount),
            Self::AscSkipCount => Some(Self::DescSkipCount),
            Self::DescSkipCount => Some(Self::AscSkipCount),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Copy, Clone, Serialize, Deserialize, Eq, PartialEq, PartialOrd)]
pub enum AutoRefresh {
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "hourly")]
    Hourly,
    #[serde(rename = "daily")]
    Daily,
    #[serde(rename = "weekly")]
    Weekly,
    #[serde(rename = "monthly")]
    Monthly,
    #[serde(rename = "yearly")]
    Yearly,
}

impl FromStr for AutoRefresh {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "hourly" => Ok(Self::Hourly),
            "daily" => Ok(Self::Daily),
            "weekly" => Ok(Self::Weekly),
            "monthly" => Ok(Self::Monthly),
            "yearly" => Ok(Self::Yearly),
            _ => Err(()),
        }
    }
}

impl AutoRefresh {
    pub fn to_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Hourly => "hourly",
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
            Self::Yearly => "yearly",
        }
    }
}

#[derive(Default, Serialize, Deserialize, Debug, Clone, Copy)]
pub enum StickerObjectType {
    #[default]
    #[serde(rename = "song")]
    Song,
    #[serde(rename = "playlist")]
    Playlist,
    // Tags
    #[serde(rename = "album")]
    Album,
    #[serde(rename = "artist")]
    Artist,
    #[serde(rename = "albumartist")]
    AlbumArtist,
}

impl StickerObjectType {
    pub fn to_str(&self) -> &'static str {
        match self {
            Self::Song => "song",
            Self::Playlist => "playlist",
            Self::Album => "album",
            Self::Artist => "artist",
            Self::AlbumArtist => "albumartist",
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum StickerOperation {
    Equals,
    LessThan,
    GreaterThan,
    Contains,
    StartsWith,
    IntEquals,
    IntLessThan,
    IntGreaterThan,
}

impl StickerOperation {
    pub fn numeric_model() -> &'static [&'static str] {
        static MODEL: Lazy<Vec<&str>> = Lazy::new(|| vec!["==", ">", "<"]);

        MODEL.as_ref()
    }

    pub fn numeric_model_index(&self) -> Option<u32> {
        match self {
            Self::IntEquals => Some(0),
            Self::IntGreaterThan => Some(1),
            Self::IntLessThan => Some(2),
            _ => None,
        }
    }

    // TODO: translations
    pub fn text_model() -> &'static [&'static str] {
        static MODEL: Lazy<Vec<&str>> =
            Lazy::new(|| vec!["==", ">", "<", "contains", "starts with"]);

        MODEL.as_ref()
    }

    pub fn text_model_index(&self) -> Option<u32> {
        match self {
            Self::Equals => Some(0),
            Self::GreaterThan => Some(1),
            Self::LessThan => Some(2),
            Self::Contains => Some(3),
            Self::StartsWith => Some(4),
            _ => None,
        }
    }

    pub fn to_mpd_syntax(&self) -> &'static str {
        match self {
            Self::Equals => "==",
            Self::LessThan => "<",
            Self::GreaterThan => ">",
            Self::Contains => "contains",
            Self::StartsWith => "starts_with",
            Self::IntEquals => "eq",
            Self::IntLessThan => "lt",
            Self::IntGreaterThan => "gt",
        }
    }
}

/// Flattened, no-lifetime version of mpd::search::Term * mpd::search::Operation,
/// only containing supported tag types.
#[derive(Debug, Serialize, Deserialize, Clone, Copy)]
pub enum QueryLhs {
    File, // matches full song URI, always ==
    Base, // from this directory
    // Tags
    LastMod,
    Any(TagOperation), // will match any tag
    Album(TagOperation),
    AlbumArtist(TagOperation),
    Artist(TagOperation),
    // more to come
}

impl<'a, 'b: 'a> QueryLhs {
    /// Consume & add self into an existing mpd::search::Query.
    pub fn add_to_query<V: 'b + Into<Cow<'b, str>>>(self, query: &mut Query<'a>, rhs: V) {
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

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Rule {
    /// Entity type (song, album, etc), LHS (key), operator, RHS (always a string)
    Sticker(StickerObjectType, String, StickerOperation, String),
    /// A subset of supported query operations.
    /// Optional LHS (tag or MPD term as string), MPD operation, right hand
    /// side (unary ops use this). We don't use mpd::search::Filter directly
    /// here to keep the Rule struct Send+Sync.
    Query(QueryLhs, String),
    /// Special case for Last-Modified, taking number of seconds to support
    /// querying in relative to current datetime.
    LastModified(i64),
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
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DynamicPlaylist {
    pub name: String,
    pub last_queued: Option<i64>,
    pub play_count: usize,
    pub rules: Vec<Rule>,
    pub ordering: Vec<Ordering>,
    pub auto_refresh: AutoRefresh,
    pub last_refresh: Option<i64>,
    pub limit: Option<u32>,
}
