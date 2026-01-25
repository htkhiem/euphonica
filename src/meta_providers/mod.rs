mod base;
mod chain;
pub mod lastfm;
pub mod lrclib;
pub mod models;
pub mod musicbrainz;

pub use base::{MetadataProvider, utils};
pub use chain::{MetadataChain, get_provider};

pub mod prelude {
    pub use super::base::{MetadataProvider, sleep_between_requests};
    pub use super::models::Merge;
}
