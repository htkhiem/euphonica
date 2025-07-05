mod controller;
mod state;
pub mod sqlite;

pub use state::CacheState;
pub mod placeholders;

pub use controller::Cache;
pub use controller::{
    get_app_cache_path,
    get_image_cache_path,
    get_doc_cache_path,
    get_new_image_paths
};
