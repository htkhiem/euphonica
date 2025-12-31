use glib::{
    Object, ParamSpec, ParamSpecChar, ParamSpecInt, ParamSpecString, clone, closure_local,
    signal::SignalHandlerId, Properties, derived_properties
};
use gtk::{CompositeTemplate, Image, Label, gdk::{self, Paintable}, prelude::*, subclass::prelude::*};
use once_cell::sync::Lazy;
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};
use derivative::Derivative;

use crate::{
    cache::{
        Cache, CacheState,
        placeholders::{ALBUMART_PLACEHOLDER, ALBUMART_THUMBNAIL_PLACEHOLDER, EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING},
    },
    common::{
        Album, AlbumInfo, Rating,
        marquee::{Marquee, MarqueeWrapMode},
    },
    utils::settings_manager,
};

#[derive(Clone, Copy, Eq, PartialEq, Debug, Default)]
pub enum ImageState {
    #[default]
    Empty,
    Spinner,
    Image
}

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Default, Properties)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/image-stack.ui")]
    #[properties(wrapper_type = super::ImageStack)]
    pub struct ImageStack {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub image: TemplateChild<gtk::Picture>, // Use thumbnail version
        pub state: Cell<ImageState>,
        #[property(get, set)]
        pub size: Cell<i32>,
        #[property(get)]
        pub is_thumbnail: Cell<bool>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ImageStack {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaImageStack";
        type Type = super::ImageStack;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[derived_properties]
    impl ObjectImpl for ImageStack {
        fn constructed(&self) {
            self.parent_constructed();

            self.obj()
                .bind_property("image-size", &self.image.get(), "width-request")
                .sync_create()
                .build();

            self.obj()
                .bind_property("image-size", &self.image.get(), "height-request")
                .sync_create()
                .build();
        }
    }

    impl WidgetImpl for ImageStack {}
}

glib::wrapper! {
    pub struct ImageStack(ObjectSubclass<imp::ImageStack>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ImageStack {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res.clear();

        res
    }

    pub fn get_state(&self) -> ImageState {
        self.imp().state.get()
    }

    pub fn set_state(&self, new: ImageState) {
        self.imp().state.set(new);
    }

    #[inline]
    fn show_placeholder(&self, thumb: bool) {
        self.imp().image.set_paintable(Some(
            if thumb {
                &*ALBUMART_THUMBNAIL_PLACEHOLDER
            } else {
                &*ALBUMART_PLACEHOLDER
            }
        ));
    }

    pub fn set_is_thumbnail(&self, new: bool) {
        // This might be a hot fn (called on every construction)
        // so avoid calling the full clear() fn
        let old = self.imp().is_thumbnail.replace(new);
        if old != new {
            if self.imp().state.get() == ImageState::Empty {
                self.show_placeholder(new);
            }
            self.notify("is-thumbnail");
        }
    }

    pub fn clear(&self) {
        self.show_placeholder(self.imp().is_thumbnail.get());
        if self.imp().stack.visible_child_name().is_none_or(|name| name != "image")  {
            self.imp().stack.set_visible_child_name("image");
        }
        self.set_state(ImageState::Empty);
    }

    pub fn show_spinner(&self) {
        if self.imp().stack.visible_child_name().is_none_or(|name| name != "spinner")  {
            self.imp().stack.set_visible_child_name("spinner");
        }
        if self.get_state() == ImageState::Image {
            self.show_placeholder(self.imp().is_thumbnail.get());
        }
        self.set_state(ImageState::Spinner);
    }

    pub fn show(&self, paintable: &impl IsA<gdk::Paintable>) {
        self.imp().image.set_paintable(Some(paintable));
        if self.imp().stack.visible_child_name().is_none_or(|name| name != "image")  {
            self.imp().stack.set_visible_child_name("image");
        }
        self.set_state(ImageState::Image);
    }
}
