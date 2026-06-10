use glib::{
    prelude::*,
    subclass::{Signal, prelude::*},
};
use gtk::{gdk, glib};
use std::sync::OnceLock;

mod imp {
    use super::*;

    #[derive(Debug, Default)]
    pub struct CacheState {}

    #[glib::object_subclass]
    impl ObjectSubclass for CacheState {
        const NAME: &'static str = "EuphonicaCacheState";
        type Type = super::CacheState;
    }

    impl ObjectImpl for CacheState {
        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("folder-cover-set")
                        .param_types([
                            String::static_type(),       // folder URI
                            gdk::Texture::static_type(), // handle to hires texture
                            gdk::Texture::static_type(), // handle to thumbnail texture
                        ])
                        .build(),
                    Signal::builder("folder-cover-cleared")
                        .param_types([
                            String::static_type(), // folder URI
                        ])
                        .build(),
                    Signal::builder("artist-avatar-set")
                        .param_types([
                            String::static_type(),       // Artist name (may be part of a tag)
                            gdk::Texture::static_type(), // handle to hires texture
                            gdk::Texture::static_type(), // handle to thumbnail texture
                        ])
                        .build(),
                    Signal::builder("artist-avatar-cleared")
                        .param_types([
                            String::static_type(), // Artist name (may be part of a tag)
                        ])
                        .build(),
                    // Signal::builder("playlist-cover-downloaded")
                    //     .param_types([
                    //         String::static_type(), // playlist name
                    //         bool::static_type(),   // is_thumbnail
                    //         gdk::Texture::static_type()
                    //     ])
                    //     .build(),
                    // Signal::builder("playlist-cover-cleared")
                    //     .param_types([
                    //         String::static_type(), // playlist name
                    //     ])
                    //     .build(),
                    // Dynamic playlists are local & changes would require refreshing the outer list anyway.
                ]
            })
        }
    }
}

glib::wrapper! {
    pub struct CacheState(ObjectSubclass<imp::CacheState>);
}

impl Default for CacheState {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl CacheState {
    // Convenience emit wrapper
    pub fn emit_with_param(&self, name: &str, tag: &str) {
        self.emit_by_name::<()>(name, &[&tag]);
    }

    pub fn emit_texture(&self, name: &str, tag: &str, hires: &gdk::Texture, thumb: &gdk::Texture) {
        self.emit_by_name::<()>(name, &[&tag, hires, thumb]);
    }
}
