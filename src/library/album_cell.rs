use glib::{
    Object, ParamSpec, ParamSpecChar, ParamSpecInt, ParamSpecString, clone, closure_local,
    signal::SignalHandlerId,
};
use gtk::{CompositeTemplate, Image, Label, gdk, prelude::*, subclass::prelude::*};
use once_cell::sync::Lazy;
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{
        Cache, CacheState,
        placeholders::{ALBUMART_THUMBNAIL_PLACEHOLDER, EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING},
    },
    common::{
        Album, AlbumInfo, CoverSource, Rating,
        marquee::{Marquee, MarqueeWrapMode},
    },
    utils::settings_manager,
};

mod imp {
    use super::*;

    #[derive(CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-cell.ui")]
    pub struct AlbumCell {
        #[template_child]
        pub inner: TemplateChild<gtk::Box>,
        #[template_child]
        pub cover: TemplateChild<gtk::Picture>, // Use high-resolution version
        #[template_child]
        pub title: TemplateChild<Marquee>,
        #[template_child]
        pub artist: TemplateChild<Label>,
        #[template_child]
        pub quality_grade: TemplateChild<Image>,
        #[template_child]
        pub rating: TemplateChild<Rating>,
        pub image_size: Cell<i32>,
        pub rating_val: Cell<i8>,
        pub album: RefCell<Option<Album>>,
        // Vector holding the bindings to properties of the Album GObject
        pub cover_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub cover_source: Cell<CoverSource>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for AlbumCell {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaAlbumCell";
        type Type = super::AlbumCell;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl Default for AlbumCell {
        fn default() -> Self {
            Self {
                inner: TemplateChild::default(),
                cover: TemplateChild::default(),
                title: TemplateChild::default(),
                artist: TemplateChild::default(),
                quality_grade: TemplateChild::default(),
                rating: TemplateChild::default(),
                image_size: Cell::new(128),
                rating_val: Cell::default(),
                album: RefCell::default(),
                cover_signal_ids: RefCell::default(),
                cache: OnceCell::new(),
                cover_source: Cell::default(),
            }
        }
    }

    impl ObjectImpl for AlbumCell {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }

            if let Some((update_id, clear_id)) = self.cover_signal_ids.take() {
                let cache_state = self.cache.get().unwrap().get_cache_state();

                cache_state.disconnect(update_id);
                cache_state.disconnect(clear_id);
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.obj()
                .bind_property("rating", &self.rating.get(), "value")
                .sync_create()
                .build();

            self.obj()
                .bind_property("rating", &self.rating.get(), "visible")
                .transform_to(|_, r: i8| Some(r >= 0))
                .sync_create()
                .build();

            self.obj()
                .bind_property("image-size", &self.cover.get(), "width-request")
                .sync_create()
                .build();

            self.obj()
                .bind_property("image-size", &self.cover.get(), "height-request")
                .sync_create()
                .build();
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("title").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("quality-grade").build(),
                    ParamSpecChar::builder("rating").build(),
                    ParamSpecInt::builder("image-size").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "title" => self.title.label().to_value(),
                "artist" => self.artist.label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                "rating" => self.rating_val.get().to_value(),
                "image-size" => self.image_size.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "title" => {
                    if let Ok(title) = value.get::<&str>() {
                        self.title.label().set_label(title);
                        obj.notify("title");
                    }
                }
                "artist" => {
                    if let Ok(artist) = value.get::<&str>() {
                        self.artist.set_label(artist);
                        obj.notify("artist");
                    }
                }
                "quality-grade" => {
                    if let Ok(icon_name) = value.get::<&str>() {
                        self.quality_grade.set_icon_name(Some(icon_name));
                        self.quality_grade.set_visible(true);
                    } else {
                        self.quality_grade.set_icon_name(None);
                        self.quality_grade.set_visible(false);
                    }
                }
                "rating" => {
                    if let Ok(new) = value.get::<i8>() {
                        let old = self.rating_val.replace(new);
                        if old != new {
                            obj.notify("rating");
                        }
                    }
                }
                "image-size" => {
                    if let Ok(new) = value.get::<i32>() {
                        obj.set_image_size(new);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for AlbumCell {
        // AlbumCell width is limited by the width of its image.
        // Here we use custom measurement & alloc logic to force ellipsis/line breaks.
        // Without these, AlbumCells can get arbitrarily wide when in a horizontal
        // layout (like the RecentView's recent albums row).
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let image_size = self.image_size.get();
            if orientation == gtk::Orientation::Horizontal {
                (
                    image_size,
                    image_size, // Always as wide as the image, no matter how long the title is
                    -1, -1,
                )
            } else {
                // Depend on the parent Box's measurements for height

                self.inner
                    .get()
                    .measure(gtk::Orientation::Vertical, for_size)
            }
        }

        fn size_allocate(&self, w: i32, h: i32, baseline: i32) {
            self.inner
                .get()
                .size_allocate(&gtk::Allocation::new(0, 0, w, h), baseline);
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            obj.snapshot_child(&self.inner.get(), snapshot);
        }
    }
}

glib::wrapper! {
    pub struct AlbumCell(ObjectSubclass<imp::AlbumCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl AlbumCell {
    pub fn new(item: &gtk::ListItem, cache: Rc<Cache>, wrap_mode: Option<MarqueeWrapMode>) -> Self {
        let res: Self = Object::builder().build();
        let cache_state = cache.get_cache_state();
        res.imp()
            .cache
            .set(cache)
            .expect("AlbumCell cannot bind to cache");
        item.property_expression("item")
            .chain_property::<Album>("title")
            .chain_closure::<String>(closure_local!(
                |_: Option<glib::Object>, title: Option<&str>| {
                    String::from(if title.is_none_or(|t| t.is_empty()) {
                        *EMPTY_ALBUM_STRING
                    } else {
                        title.unwrap()
                    })
                }
            ))
            .bind(&res, "title", gtk::Widget::NONE);
        if let Some(wrap_mode) = wrap_mode {
            // Some views, like the Recent View, requires a specific mode due to UI constraints.
            res.imp().title.set_wrap_mode(wrap_mode);
        } else {
            // If unspecified, bind to GSettings
            settings_manager()
                .child("ui")
                .bind("title-wrap-mode", &res.imp().title.get(), "wrap-mode")
                .mapping(|var, _| {
                    Some(
                        MarqueeWrapMode::try_from(var.get::<String>().unwrap().as_ref())
                            .expect("Invalid title-wrap-mode setting value")
                            .into(),
                    )
                })
                .get_only()
                .build();
        }

        item.property_expression("item")
            .chain_property::<Album>("artist")
            .chain_closure::<String>(closure_local!(
                |_: Option<glib::Object>, artist: Option<&str>| {
                    String::from(if artist.is_none_or(|a| a.is_empty()) {
                        *EMPTY_ARTIST_STRING
                    } else {
                        artist.unwrap()
                    })
                }
            ))
            .bind(&res, "artist", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Album>("quality-grade")
            .bind(&res, "quality-grade", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Album>("rating")
            .bind(&res, "rating", gtk::Widget::NONE);

        // Run only while hovered
        let hover_ctl = gtk::EventControllerMotion::new();
        hover_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
        hover_ctl.connect_enter(clone!(
            #[weak(rename_to = this)]
            res,
            move |_, _, _| {
                if this.imp().title.wrap_mode() == MarqueeWrapMode::Scroll {
                    this.imp().title.set_should_run_and_check(true);
                }
            }
        ));
        hover_ctl.connect_leave(clone!(
            #[weak(rename_to = this)]
            res,
            move |_| {
                this.imp().title.set_should_run_and_check(false);
            }
        ));
        res.add_controller(hover_ctl);
        let _ = res.imp().cover_signal_ids.replace(Some((
            cache_state.connect_closure(
                "album-art-downloaded",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    res,
                    move |_: CacheState, uri: String, thumb: bool, tex: gdk::Texture| {
                        if !thumb {
                            return;
                        }
                        if let Some(album) = this.imp().album.borrow().as_ref() {
                            if album.get_folder_uri() == uri {
                                // Force update since we might have been using an embedded cover
                                // temporarily
                                this.update_cover(tex, CoverSource::Folder);
                            } else if this.imp().cover_source.get() != CoverSource::Folder
                                && album.get_example_uri() == uri
                            {
                                this.update_cover(tex, CoverSource::Embedded);
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
                    res,
                    move |_: CacheState, uri: String| {
                        if let Some(album) = this.imp().album.borrow().as_ref() {
                            match this.imp().cover_source.get() {
                                CoverSource::Folder => {
                                    if album.get_folder_uri() == uri {
                                        this.clear_cover();
                                    }
                                }
                                CoverSource::Embedded => {
                                    if album.get_example_uri() == uri {
                                        this.clear_cover();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                ),
            ),
        )));
        res
    }

    fn clear_cover(&self) {
        self.imp().cover_source.set(CoverSource::None);
        self.imp()
            .cover
            .set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
    }

    fn schedule_cover(&self, info: &AlbumInfo) {
        self.imp().cover_source.set(CoverSource::Unknown);
        self.imp()
            .cover
            .set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
        if let Some((tex, is_embedded)) = self
            .imp()
            .cache
            .get()
            .unwrap()
            .clone()
            .load_cached_folder_cover(info, true, true)
        {
            self.imp().cover.set_paintable(Some(&tex));
            self.imp().cover_source.set(if is_embedded {
                CoverSource::Embedded
            } else {
                CoverSource::Folder
            });
        }
    }

    fn update_cover(&self, tex: gdk::Texture, src: CoverSource) {
        self.imp().cover.set_paintable(Some(&tex));
        self.imp().cover_source.set(src);
    }

    pub fn bind(&self, album: &Album) {
        // The string properties are bound using property expressions in setup().
        // Fetch album cover once here.
        // Set once first (like sync_create)
        let _ = self.imp().album.replace(Some(album.clone()));
        self.schedule_cover(album.get_info());
    }

    pub fn unbind(&self) {
        if let Some(_) = self.imp().album.take() {
            // Clear cover reference
            self.clear_cover();
        }
    }

    pub fn image_size(&self) -> i32 {
        self.imp().image_size.get()
    }

    pub fn set_image_size(&self, new: i32) {
        let old = self.imp().image_size.replace(new);
        if old != new {
            self.notify("image-size");
        }
    }
}
