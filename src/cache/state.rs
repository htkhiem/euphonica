use glib::{
    prelude::*,
    subclass::{prelude::*, Signal},
};
use gtk::{glib, gdk};
use std::sync::OnceLock;

mod imp {
    // use glib::{
    //     ParamSpec,
    //     ParamSpecBoolean,
    //     ParamSpecEnum
    // };
    use super::*;

    #[derive(Debug, Default)]
    pub struct CacheState {}

    #[glib::object_subclass]
    impl ObjectSubclass for CacheState {
        const NAME: &'static str = "EuphonicaCacheState";
        type Type = super::CacheState;
    }

    impl ObjectImpl for CacheState {
        // fn properties() -> &'static [ParamSpec] {
        //     static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
        //         vec![
        //             ParamSpecBoolean::builder("busy").read_only().build(),
        //             ParamSpecEnum::builder::<ConnectionState>("connection-state").read_only().build()
        //         ]
        //     });
        //     PROPERTIES.as_ref()
        // }

        // fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
        //     let obj = self.obj();
        //     match pspec.name() {
        //         "connection-state" => obj.get_connection_state().to_value(),
        //         "busy" => obj.is_busy().to_value(),
        //         _ => unimplemented!(),
        //     }
        // }

        // fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
        //     let obj = self.obj();
        //     match pspec.name() {
        //         "connection-state" => {
        //             let state = value.get().expect("Error in CacheState::set_property");
        //             obj.set_connection_state(state);
        //         },
        //         _ => unimplemented!()
        //     }
        // }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    // Only emitted when a new album art becomes locally available.
                    Signal::builder("album-art-downloaded")
                        .param_types([
                            String::static_type(), // folder or file URI
                            bool::static_type(),   // is_thumbnail
                            gdk::Texture::static_type()
                        ])
                        .build(),
                    Signal::builder("album-art-cleared")
                        .param_types([
                            String::static_type(), // folder URI
                        ])
                        .build(),
                    Signal::builder("album-meta-downloaded")
                        .param_types([
                            String::static_type(), // album tag
                        ])
                        .build(),
                    Signal::builder("artist-meta-downloaded")
                        .param_types([
                            String::static_type(), // artist tag
                        ])
                        .build(),
                    Signal::builder("artist-avatar-downloaded")
                        .param_types([
                            String::static_type(), // artist tag
                            bool::static_type(),   // is_thumbnail
                            gdk::Texture::static_type()
                        ])
                        .build(),
                    Signal::builder("artist-avatar-cleared")
                        .param_types([
                            String::static_type(), // artist tag
                        ])
                        .build(),
                    Signal::builder("song-lyrics-downloaded")
                        .param_types([
                            String::static_type(), // full song URI
                        ])
                        .build(),
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

    pub fn emit_texture(&self, name: &str, tag: &str, thumb: bool, tex: &gdk::Texture) {
        self.emit_by_name::<()>(name, &[&tag, &thumb, tex]);
    }
}
