use glib::{clone, closure_local, Object, SignalHandlerId};
use gtk::{gdk, glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, Cache, CacheState},
    common::{CoverSource, Song, SongInfo},
    utils::strip_filename_linux,
};

use super::Library;

mod imp {
    use std::cell::Cell;

    use crate::common::CoverSource;

    use super::*;
    use glib::{ParamSpec, ParamSpecString};
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/recent-song-row.ui")]
    pub struct RecentSongRow {
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
        pub artist_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub last_played: TemplateChild<gtk::Label>,
        pub thumbnail_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub library: OnceCell<Library>,
        pub song: RefCell<Option<Song>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub thumbnail_source: Cell<CoverSource>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for RecentSongRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaRecentSongRow";
        type Type = super::RecentSongRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for RecentSongRow {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecString::builder("artist").build(),
                    // ParamSpecInt64::builder("disc").build(),
                    ParamSpecString::builder("quality-grade").build(),
                    ParamSpecString::builder("last-played").build()
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
                // "disc" => self.disc.get_label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                "last-played" => self.last_played.label().to_value(),
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
                "artist" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.artist_name.set_label(name);
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
                "last-played" => {
                    if let Ok(desc) = value.get::<&str>() {
                        self.last_played.set_label(desc);
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
    impl WidgetImpl for RecentSongRow {}

    // Trait shared by all boxes
    impl BoxImpl for RecentSongRow {}
}

glib::wrapper! {
    pub struct RecentSongRow(ObjectSubclass<imp::RecentSongRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RecentSongRow {
    pub fn new(library: Library, item: &Song, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, item, cache);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &Song, cache: Rc<Cache>) {
        let cache_state = cache.get_cache_state();
        self.imp()
           .cache
           .set(cache)
           .expect("RecentSongRow cannot bind to cache");
        let _ = self.imp().library.set(library);
        item.property_expression("name")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("album")
            .bind(self, "album", gtk::Widget::NONE);

        item.property_expression("artist")
            .bind(self, "artist", gtk::Widget::NONE);

        item.property_expression("last-played-desc")
            .bind(self, "last-played", gtk::Widget::NONE);

        item.property_expression("quality-grade")
            .bind(self, "quality-grade", gtk::Widget::NONE);

        let _ = self.imp().thumbnail_signal_ids.replace(Some((
            cache_state.connect_closure(
                "album-art-downloaded",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String, thumb: bool, tex: gdk::Texture| {
                        if !thumb {
                            return;
                        }
                        // Match song URI first then folder URI. Only try to match by folder URI
                        // if we don't have a current thumbnail.
                        if let Some(song) = this.imp().song.borrow().as_ref() {
                            if uri.as_str() == song.get_uri() {
                                // Force update since we might have been using a folder cover
                                // temporarily
                                this.update_thumbnail(tex, CoverSource::Embedded);
                            } else if this.imp().thumbnail_source.get() != CoverSource::Embedded {
                                if strip_filename_linux(song.get_uri()) == uri {
                                    this.update_thumbnail(tex, CoverSource::Folder);
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

        self.imp().song.replace(Some(item.clone()));
        self.schedule_thumbnail(item.get_info());
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
            .clone()
            .load_cached_embedded_cover(info, true, true) {
                self.imp().thumbnail.set_paintable(Some(&tex));
                self.imp().thumbnail_source.set(
                    if is_embedded {CoverSource::Embedded} else {CoverSource::Folder}
                );
            }
    }

    fn update_thumbnail(&self, tex: gdk::Texture, src: CoverSource) {
        self.imp().thumbnail.set_paintable(Some(&tex));
        self.imp().thumbnail_source.set(src);
    }
}
