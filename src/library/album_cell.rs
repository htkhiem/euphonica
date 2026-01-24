use glib::{
    Object, ParamSpec, ParamSpecChar, ParamSpecInt, ParamSpecString, clone, closure_local,
    signal::SignalHandlerId, WeakRef
};
use gtk::{CompositeTemplate, Image, Label, gdk, prelude::*, subclass::prelude::*};
use once_cell::sync::Lazy;
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};
use derivative::Derivative;

use crate::{
    cache::{
        Cache, CacheState,
        placeholders::{EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING},
    },
    common::{
        Album, Rating,
        marquee::{Marquee, MarqueeWrapMode},
        PictureStack
    },
    utils::settings_manager,
};

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-cell.ui")]
    pub struct AlbumCell {
        #[template_child]
        pub inner: TemplateChild<gtk::Box>,
        #[template_child]
        pub cover: TemplateChild<PictureStack>, // Use thumbnail version
        #[template_child]
        pub title: TemplateChild<Marquee>,
        #[template_child]
        pub artist: TemplateChild<Label>,
        #[template_child]
        pub quality_grade: TemplateChild<Image>,
        #[template_child]
        pub rating: TemplateChild<Rating>,
        #[derivative(Default(value = "Cell::new(128)"))]
        pub image_size: Cell<i32>,
        #[derivative(Default(value = "Cell::new(-1)"))]
        pub rating_val: Cell<i8>,
        pub album: WeakRef<Album>,
        // Vector holding the bindings to properties of the Album GObject
        pub cover_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub cache: OnceCell<Rc<Cache>>
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
                .bind_property("image-size", &self.cover.get(), "size")
                .sync_create()
                .build();

            self.cover.set_is_thumbnail(true);
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
            // Always as wide as the image, no matter how long the title is.
            let image_size = self.image_size.get();
            if orientation == gtk::Orientation::Horizontal {
                (
                    image_size,
                    image_size,
                    -1, -1,
                )
            } else {
                // Ensure we request enough vertical space for a square cover at the
                // given width. Horizontal rectangular album covers look really weird.
                // Calculating the actual total height is rather involved due to gaps and
                // the like. Instead of re-implementing the sum, we simply calculate the
                // "as usual" height of the cover art when allocated using GTK4 rules to
                // its width and adjust the total accordingly.
                // TODO: this is still hacky. Find a better way later.
                // Return order reminder: min, natural, min baseline, natural baseline.
                let raw_cover_size = dbg!(self.cover.measure(gtk::Orientation::Vertical, for_size));
                let diff = (for_size - raw_cover_size.0).max(0);
                let res = self.inner.get().measure(gtk::Orientation::Vertical, for_size);
                dbg!((
                    res.0 + diff,
                    res.1,
                    res.2,
                    res.3
                ))
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
                "folder-cover-set",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    res,
                    move |_: CacheState, uri: String, _: gdk::Texture, thumb: gdk::Texture| {
                        if this.imp().album.upgrade().is_some_and(|a| a.get_folder_uri() == uri) {
                            this.imp().cover.show(&thumb);
                        }
                    }
                ),
            ),
            cache_state.connect_closure(
                "folder-cover-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    res,
                    move |_: CacheState, uri: String| {
                        if this.imp().album.upgrade().is_some_and(|a| a.get_folder_uri() == uri) {
                            this.imp().cover.clear();
                        }
                    }
                ),
            ),
        )));
        res
    }

    pub fn bind(&self, album: &Album) {
        // The string properties are bound using property expressions in setup().
        // Fetch album cover once here.
        // Set once first (like sync_create)
        self.imp().album.set(Some(album));
        self.imp().cover.show_spinner();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            album,
            async move {
                match this.imp().cache.get().unwrap().clone().get_album_cover(
                    album.get_info(), true, true
                ).await {
                    Ok(Some(tex)) => {
                        this.imp().cover.show(&tex);
                    }
                    Ok(None) => {
                        this.imp().cover.clear();
                    }
                    Err(e) => {
                        this.imp().cover.clear();
                        dbg!(e);
                    }
                }
            }
        ));
    }

    pub fn unbind(&self) {
        self.imp().cover.clear();
        self.imp().album.set(None);
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
