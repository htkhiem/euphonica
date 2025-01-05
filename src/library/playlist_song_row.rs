use std::{
    cell::{RefCell, OnceCell},
    rc::Rc
};
use gtk::{
    glib,
    prelude::*,
    subclass::prelude::*,
    CompositeTemplate,
};
use glib::{
    clone,
    closure_local,
    Object,
    SignalHandlerId
};

use crate::{
    cache::{
        placeholders::ALBUMART_PLACEHOLDER,
        Cache,
        CacheState
    },
    common::{AlbumInfo, Song},
    utils::format_secs_as_duration
};

use super::Library;

mod imp {
    use glib::{
        ParamSpec,
        ParamSpecString
    };
    use once_cell::sync::Lazy;
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/org/euphonica/Euphonica/gtk/library/playlist-song-row.ui")]
    pub struct PlaylistSongRow {
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub thumbnail: TemplateChild<gtk::Image>,
        #[template_child]
        pub song_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration: TemplateChild<gtk::Label>,
        // For unbinding the queue buttons when not bound to a song (i.e. being recycled)
        pub replace_queue_id: RefCell<Option<SignalHandlerId>>,
        pub append_queue_id: RefCell<Option<SignalHandlerId>>,
        pub thumbnail_signal_id: RefCell<Option<SignalHandlerId>>,
        pub library: OnceCell<Library>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistSongRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPlaylistSongRow";
        type Type = super::PlaylistSongRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for PlaylistSongRow {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecString::builder("duration").build(),
                    // ParamSpecInt64::builder("disc").build(),
                    ParamSpecString::builder("quality-grade").build()
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "name" => self.song_name.label().to_value(),
                // "last_mod" => obj.get_last_mod().to_value(),
                "album" => self.album_name.label().to_value(),
                "artist" => self.artist_name.label().to_value(),
                "duration" => self.duration.label().to_value(),
                // "disc" => self.disc.get_label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "name" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(name) = value.get::<&str>() {
                        self.song_name.set_label(name);
                    }
                }
                "artist" => {
                    if let Ok(tag) = value.get::<&str>() {
                        self.artist_name.set_label(tag);
                    }
                }
                "album" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.album_name.set_label(name);
                    }
                }
                "duration" => {
                    // Pre-formatted please
                    if let Ok(dur) = value.get::<&str>() {
                        self.duration.set_label(dur);
                    }
                }
                "quality-grade" => {
                    if let Ok(icon) = value.get::<&str>() {
                        self.quality_grade.set_icon_name(Some(icon));
                        self.quality_grade.set_visible(true);
                    }
                    else {
                        self.quality_grade.set_icon_name(None);
                        self.quality_grade.set_visible(false);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlaylistSongRow {}

    // Trait shared by all boxes
    impl BoxImpl for PlaylistSongRow {}
}

glib::wrapper! {
    pub struct PlaylistSongRow(ObjectSubclass<imp::PlaylistSongRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl PlaylistSongRow {
    pub fn new(library: Library, item: &gtk::ListItem) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, item);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &gtk::ListItem) {
        let _ = self.imp().library.set(library);
        item
            .property_expression("item")
            .chain_property::<Song>("name")
            .bind(self, "name", gtk::Widget::NONE);

        item
            .property_expression("item")
            .chain_property::<Song>("artist")
            .bind(self, "artist", gtk::Widget::NONE);

        item
            .property_expression("item")
            .chain_property::<Song>("album")
            .bind(self, "album", gtk::Widget::NONE);

        item
            .property_expression("item")
            .chain_property::<Song>("duration")
            .chain_closure::<String>(closure_local!(|_: Option<Object>, dur: u64| {
                format_secs_as_duration(dur as f64)
            }))
            .bind(self, "duration", gtk::Widget::NONE);

        item
            .property_expression("item")
            .chain_property::<Song>("quality-grade")
            .bind(self, "quality-grade", gtk::Widget::NONE);
    }

    fn update_thumbnail(&self, info: Option<&AlbumInfo>, cache: Rc<Cache>, schedule: bool) {
        if let Some(album) = info {
            // Should already have been downloaded by the album view
            if let Some(tex) = cache.load_cached_album_art(album, true, schedule) {
                self.imp().thumbnail.set_paintable(Some(&tex));
                return;
            }
        }
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
    }

    pub fn bind(&self, song: &Song, cache: Rc<Cache>) {
        // Bind album art listener. Set once first (like sync_create)
        self.update_thumbnail(song.get_album(), cache.clone(), true);
        let thumbnail_binding = cache.get_cache_state().connect_closure(
            "album-art-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                song,
                #[weak]
                cache,
                move |_: CacheState, folder_uri: String| {
                    if let Some(album) = song.get_album() {
                        if album.uri == folder_uri {
                            this.update_thumbnail(Some(album), cache, false);
                        }
                    }
                }
            )
        );
        self.imp().thumbnail_signal_id.replace(Some(thumbnail_binding));
        // Bind the queue buttons
        let uri = song.get_uri().to_owned();
        if let Some(old_id) = self.imp().replace_queue_id.replace(
            Some(
                self.imp().replace_queue.connect_clicked(
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        #[strong]
                        uri,
                        move |_| {
                            if let Some(library) = this.imp().library.get() {
                                library.queue_uri(&uri, true, true, false);
                            }
                        }
                    )
                )
            )
        ) {
            // Unbind old ID
            self.imp().replace_queue.disconnect(old_id);
        }
        if let Some(old_id) = self.imp().append_queue_id.replace(
            Some(
                self.imp().append_queue.connect_clicked(
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        #[strong]
                        uri,
                        move |_| {
                            if let Some(library) = this.imp().library.get() {
                                library.queue_uri(&uri, false, false, false);
                            }
                        }
                    )
                )
            )
        ) {
            // Unbind old ID
            self.imp().append_queue.disconnect(old_id);
        }
    }

    pub fn unbind(&self) {
        if let Some(id) = self.imp().replace_queue_id.borrow_mut().take() {
            self.imp().replace_queue.disconnect(id);
        }
        if let Some(id) = self.imp().append_queue_id.borrow_mut().take() {
            self.imp().append_queue.disconnect(id);
        }
    }
}
