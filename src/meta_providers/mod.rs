mod base;
mod chain;
pub mod lastfm;
pub mod lrclib;
pub mod models;
pub mod musicbrainz;

pub use base::{utils, MetadataProvider};
pub use chain::{get_provider, MetadataChain};

pub mod prelude {
    pub use super::base::{reqwest_error_is_retryable, sleep_between_requests, MetadataProvider};
    pub use super::models::Merge;
}
