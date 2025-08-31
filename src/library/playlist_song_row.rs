use glib::{clone, closure_local, Object, SignalHandlerId, WeakRef};
use gtk::{gdk, glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, Cache, CacheState},
    common::{CoverSource, Song, SongInfo},
    utils::{format_secs_as_duration, strip_filename_linux},
};

use super::{Library, PlaylistContentView};

mod imp {
    use std::cell::Cell;

    use crate::{cache::placeholders::{EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING}, library::PlaylistContentView};
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecString};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/playlist-song-row.ui")]
    pub struct PlaylistSongRow {
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,
        #[template_child]
        pub thumbnail: TemplateChild<gtk::Image>,
        #[template_child]
        pub playlist_order: TemplateChild<gtk::Label>,
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
        pub thumbnail_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub raise_signal_id: RefCell<Option<SignalHandlerId>>,
        pub lower_signal_id: RefCell<Option<SignalHandlerId>>,
        pub remove_signal_id: RefCell<Option<SignalHandlerId>>,
        pub library: OnceCell<Library>,
        pub item: WeakRef<gtk::ListItem>,
        pub cache: OnceCell<Rc<Cache>>,
        pub content_view: OnceCell<PlaylistContentView>,
        pub queue_controls_visible: Cell<bool>,
        pub edit_controls_visible: Cell<bool>,
        pub thumbnail_source: Cell<CoverSource>
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
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            for elem in [self.replace_queue.get(), self.append_queue.get()] {
                obj.bind_property("queue-controls-visible", &elem, "visible")
                    .sync_create()
                    .build();
            }

            for elem in [self.raise.get(), self.lower.get(), self.remove.get()] {
                obj.bind_property("edit-controls-visible", &elem, "visible")
                    .sync_create()
                    .build();
            }
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("order").build(),
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecString::builder("duration").build(),
                    // ParamSpecInt64::builder("disc").build(),
                    ParamSpecString::builder("quality-grade").build(),
                    ParamSpecBoolean::builder("queue-controls-visible").build(),
                    ParamSpecBoolean::builder("edit-controls-visible").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "order" => self.playlist_order.label().to_value(),
                "name" => self.song_name.label().to_value(),
                // "last_mod" => obj.get_last_mod().to_value(),
                "album" => self.album_name.label().to_value(),
                "artist" => self.artist_name.label().to_value(),
                "duration" => self.duration.label().to_value(),
                // "disc" => self.disc.get_label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                "queue-controls-visible" => self.queue_controls_visible.get().to_value(),
                "edit-controls-visible" => self.edit_controls_visible.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "order" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(new) = value.get::<&str>() {
                        self.playlist_order.set_label(new);
                        self.obj().notify("order");
                    }
                }
                "name" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(name) = value.get::<&str>() {
                        self.song_name.set_label(name);
                    }
                }
                "artist" => {
                    if let Ok(tag) = value.get::<&str>() {
                        self.artist_name.set_label(tag);
                    } else {
                        self.artist_name.set_label(*EMPTY_ARTIST_STRING);
                    }
                }
                "album" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.album_name.set_label(name);
                    } else {
                        self.album_name.set_label(*EMPTY_ALBUM_STRING);
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
                "queue-controls-visible" => {
                    if let Ok(new) = value.get::<bool>() {
                        self.obj().set_queue_controls_visible(new);
                    }
                }
                "edit-controls-visible" => {
                    if let Ok(new) = value.get::<bool>() {
                        self.obj().set_edit_controls_visible(new);
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
    pub fn new(library: Library, view: PlaylistContentView, item: &gtk::ListItem, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, view, item, cache);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, view: PlaylistContentView, item: &gtk::ListItem, cache: Rc<Cache>) {
        let cache_state = cache.get_cache_state();
        self.imp()
           .cache
           .set(cache)
           .expect("ArtistSongRow cannot bind to cache");
        let _ = self.imp().library.set(library);
        let _ = self.imp().content_view.set(view);
        item.property_expression("position")
            .chain_closure::<String>(closure_local!(|_: Option<Object>, val: u32| {
                val.to_string()
            }))
            .bind(self, "order", gtk::Widget::NONE);
        item.property_expression("item")
            .chain_property::<Song>("name")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("artist")
            .bind(self, "artist", gtk::Widget::NONE);

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
                    move |_: CacheState, uri: String, thumb: bool, tex: gdk::Texture| {
                        if !thumb {
                            return;
                        }
                        // Match song URI first then folder URI. Only try to match by folder URI
                        // if we don't have a current thumbnail.
                        if let Some(song) = this.song() {
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
                        if let Some(song) = this.song() {
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
                if let (Some(library), Some(song)) = (this.imp().library.get(), this.song()) {
                    library.queue_uri(song.get_uri(), true, true, false);
                }
            }
        ));

        self.imp().append_queue.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let (Some(library), Some(song)) = (this.imp().library.get(), this.song()) {
                    library.queue_uri(song.get_uri(), false, false, false);
                }
            }
        ));

        self.imp().raise.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let Some(item) = this.imp().item.upgrade() {
                    this.imp().content_view.get().unwrap().shift_backward(item.position());
                }
            }
        ));

        self.imp().lower.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let Some(item) = this.imp().item.upgrade() {
                    this.imp().content_view.get().unwrap().shift_forward(item.position());
                }
            }
        ));

        self.imp().remove.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let Some(item) = this.imp().item.upgrade() {
                    this.imp().content_view.get().unwrap().remove(item.position());
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

    fn song(&self) -> Option<Song> {
        self.imp().item.upgrade().map(|item| item
            .item()
            .and_downcast::<Song>()
            .expect("The item has to be a common::Song."))
    }

    pub fn bind(&self, item: &gtk::ListItem) {
        self.imp().item.set(Some(item));
        let song = self.song().unwrap();
        self.schedule_thumbnail(song.get_info());
    }

    pub fn unbind(&self) {
        self.imp().item.set(Option::<&gtk::ListItem>::None);
        self.clear_thumbnail();
    }

    pub fn get_queue_controls_visible(&self) -> bool {
        self.imp().queue_controls_visible.get()
    }

    pub fn set_queue_controls_visible(&self, new: bool) {
        let old = self.imp().queue_controls_visible.replace(new);
        if old != new {
            self.notify("queue-controls-visible");
        }
    }

    pub fn get_edit_controls_visible(&self) -> bool {
        self.imp().edit_controls_visible.get()
    }

    pub fn set_edit_controls_visible(&self, new: bool) {
        let old = self.imp().edit_controls_visible.replace(new);
        if old != new {
            self.notify("edit-controls-visible");
        }
    }
}
