use glib::{
    Properties, derived_properties
};
use gtk::{CompositeTemplate, gdk::{self}, prelude::*, subclass::prelude::*};
use std::cell::Cell;

use crate::cache::placeholders::{ALBUMART_PLACEHOLDER, ALBUMART_THUMBNAIL_PLACEHOLDER};

use super::ImageState;

mod imp {
    use super::*;

    #[derive(CompositeTemplate, Default, Properties)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/picture-stack.ui")]
    #[properties(wrapper_type = super::PictureStack)]
    pub struct PictureStack {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub picture: TemplateChild<gtk::Picture>,
        pub state: Cell<ImageState>,
        #[property(get, set)]
        pub size: Cell<i32>,
        #[property(get)]
        pub is_thumbnail: Cell<bool>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PictureStack {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPictureStack";
        type Type = super::PictureStack;
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
    impl ObjectImpl for PictureStack {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
        fn constructed(&self) {
            self.parent_constructed();

            self.obj()
                .bind_property("size", &self.stack.get(), "width-request")
                .sync_create()
                .build();

            self.obj()
                .bind_property("size", &self.stack.get(), "height-request")
                .sync_create()
                .build();
        }
    }

    impl WidgetImpl for PictureStack {}
}

glib::wrapper! {
    pub struct PictureStack(ObjectSubclass<imp::PictureStack>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PictureStack {
    fn default() -> Self {
        Self::new()
    }
}

impl PictureStack {
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
        self.imp().picture.set_paintable(Some(
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
        if self.imp().stack.visible_child_name().is_none_or(|name| name != "picture")  {
            self.imp().stack.set_visible_child_name("picture");
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
        self.imp().picture.set_paintable(Some(paintable));
        if self.imp().stack.visible_child_name().is_none_or(|name| name != "picture")  {
            self.imp().stack.set_visible_child_name("picture");
        }
        self.set_state(ImageState::Image);
    }
}
