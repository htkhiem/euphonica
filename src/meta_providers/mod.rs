mod base;
mod chain;
pub mod lastfm;
pub mod models;
pub mod musicbrainz;
pub mod lrclib;

pub use base::{utils, ProviderMessage, MetadataType, MetadataProvider};
pub use chain::{get_provider_with_priority, MetadataChain};

pub mod prelude {
    pub use super::base::{sleep_after_request, MetadataProvider};
    pub use super::models::Merge;
}
