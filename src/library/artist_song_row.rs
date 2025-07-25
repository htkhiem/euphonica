use glib::{clone, closure_local, Object, SignalHandlerId, ParamSpec, ParamSpecString};
use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, Cache, CacheState},
    common::{CoverSource, Song, SongInfo},
    utils::{format_secs_as_duration, strip_filename_linux},
};

use super::Library;

mod imp {
    use std::cell::Cell;

    use crate::common::CoverSource;

    use super::*;
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-song-row.ui")]
    pub struct ArtistSongRow {
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
        pub album_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration: TemplateChild<gtk::Label>,
        pub thumbnail_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub library: OnceCell<Library>,
        pub song: RefCell<Option<Song>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub thumbnail_source: Cell<CoverSource>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ArtistSongRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaArtistSongRow";
        type Type = super::ArtistSongRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for ArtistSongRow {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecString::builder("duration").build(),
                    // ParamSpecInt64::builder("disc").build(),
                    ParamSpecString::builder("quality-grade").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "name" => self.song_name.label().to_value(),
                // "last_mod" => obj.get_last_mod().to_value(),
                "album" => self.album_name.label().to_value(),
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
                    } else {
                        self.quality_grade.set_icon_name(None);
                        self.quality_grade.set_visible(false);
                    }
                }
                _ => unimplemented!(),
            }
        }

        fn dispose(&self) {
            if let Some((set_id, clear_id)) = self.thumbnail_signal_ids.take() {
                let cache_state = self.cache.get().unwrap().get_cache_state();
                cache_state.disconnect(set_id);
                cache_state.disconnect(clear_id);
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for ArtistSongRow {}

    // Trait shared by all boxes
    impl BoxImpl for ArtistSongRow {}
}

glib::wrapper! {
    pub struct ArtistSongRow(ObjectSubclass<imp::ArtistSongRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl ArtistSongRow {
    pub fn new(library: Library, item: &gtk::ListItem, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, item, cache);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &gtk::ListItem, cache: Rc<Cache>) {
        let cache_state = cache.get_cache_state();
        self.imp()
           .cache
           .set(cache)
           .expect("ArtistSongRow cannot bind to cache");
        let _ = self.imp().library.set(library);
        item.property_expression("item")
            .chain_property::<Song>("name")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("album")
            .bind(self, "album", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("duration")
            .chain_closure::<String>(closure_local!(|_: Option<Object>, dur: u64| {
                format_secs_as_duration(dur as f64)
            }))
            .bind(self, "duration", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("quality-grade")
            .bind(self, "quality-grade", gtk::Widget::NONE);

        let _ = self.imp().thumbnail_signal_ids.replace(Some((
            cache_state.connect_closure(
                "album-art-downloaded",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String| {
                        // Match song URI first then folder URI. Only try to match by folder URI
                        // if we don't have a current thumbnail.
                        if let Some(song) = this.imp().song.borrow().as_ref() {
                            if uri.as_str() == song.get_uri() {
                                // Force update since we might have been using a folder cover
                                // temporarily
                                this.update_thumbnail(song.get_info());
                            } else if this.imp().thumbnail_source.get() != CoverSource::Embedded {
                                if strip_filename_linux(song.get_uri()) == uri {
                                    this.update_thumbnail(song.get_info());
                                }
                            }
                        }
                    }
                ),
            ),
            cache_state.connect_closure(
                "album-art-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String| {
                        if let Some(song) = this.imp().song.borrow().as_ref() {
                            match this.imp().thumbnail_source.get() {
                                CoverSource::Folder => {
                                    if strip_filename_linux(song.get_uri()) == uri {
                                        this.clear_thumbnail();
                                    }
                                }
                                CoverSource::Embedded => {
                                    if song.get_uri() == &uri {
                                        this.clear_thumbnail();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                ),
            ),
        )));

        self.imp().replace_queue.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let (Some(library), Some(song)) = (this.imp().library.get(), this.imp().song.borrow().as_ref()) {
                    library.queue_uri(song.get_uri(), true, true, false);
                }
            }
        ));

        self.imp().append_queue.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let (Some(library), Some(song)) = (this.imp().library.get(), this.imp().song.borrow().as_ref()) {
                    library.queue_uri(song.get_uri(), false, false, false);
                }
            }
        ));
    }

    fn clear_thumbnail(&self) {
        self.imp().thumbnail_source.set(CoverSource::None);
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
    }

    fn schedule_thumbnail(&self, info: &SongInfo) {
        self.imp().thumbnail_source.set(CoverSource::Unknown);
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
        if let Some((tex, is_embedded)) = self
            .imp()
            .cache
            .get()
            .unwrap()
            .load_cached_embedded_cover(info, true, true) {
                self.imp().thumbnail.set_paintable(Some(&tex));
                self.imp().thumbnail_source.set(
                    if is_embedded {CoverSource::Embedded} else {CoverSource::Folder}
                );
            }
    }

    fn update_thumbnail(&self, info: &SongInfo) {
        if let Some((tex, is_embedded)) = self
            .imp()
            .cache
            .get()
            .unwrap()
            .load_cached_embedded_cover(info, true, false) {
                let curr_src = self.imp().thumbnail_source.get();
                // Only use embedded if we currently have nothing
                if curr_src != CoverSource::Embedded {
                    self.imp().thumbnail.set_paintable(Some(&tex));
                    self.imp().thumbnail_source.set(if is_embedded {CoverSource::Embedded} else {CoverSource::Folder});
                }
            }
    }

    pub fn bind(&self, song: &Song) {
        // Bind album art listener. Set once first (like sync_create)
        self.imp().song.replace(Some(song.clone()));
        self.schedule_thumbnail(song.get_info());
    }

    pub fn unbind(&self) {
        if let Some(_) = self.imp().song.take() {
            self.clear_thumbnail();
        }
    }
}
