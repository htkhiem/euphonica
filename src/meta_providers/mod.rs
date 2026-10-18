mod base;
mod chain;
pub mod lastfm;
pub mod lrclib;
pub mod models;
pub mod musicbrainz;
pub mod the_audio_db;

pub use base::{MetadataProvider, utils};
pub use chain::{MetadataChain, effective_provider_order};

pub mod prelude {
    pub use super::base::{
        MetadataError, MetadataProvider, MetadataResult, sleep_between_requests,
    };
    pub use super::models::Merge;
}
