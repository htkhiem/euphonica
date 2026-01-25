use glib::{
    WeakRef, ParamSpecObject, ParamSpec
};
use once_cell::sync::Lazy;
use gtk::{CompositeTemplate, gdk::{self}, prelude::*, subclass::prelude::*};
use std::cell::Cell;

use super::ImageState;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/content-stack.ui")]
    pub struct ContentStack {
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,

        // Optional
        pub placeholder_page: WeakRef<gtk::StackPage>,
        pub content_page: WeakRef<gtk::StackPage>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ContentStack {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaContentStack";
        type Type = super::ContentStack;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ContentStack {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecObject::builder::<gtk::Widget>("placeholder")
                        .construct_only()
                        .build(),
                    ParamSpecObject::builder::<gtk::Widget>("content")
                        .construct_only()
                        .build()
                ]
            });
            PROPERTIES.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "placeholder" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.stack.add_named(&widget, Some("placeholder"));
                    }
                }
                "content" => {
                    if let Ok(widget) = value.get::<gtk::Widget>() {
                        self.stack.add_named(&widget, Some("content"));
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for ContentStack {}
}

glib::wrapper! {
    pub struct ContentStack(ObjectSubclass<imp::ContentStack>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ContentStack {
    fn default() -> Self {
        Self::new()
    }
}

impl ContentStack {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();
        res
    }

    pub fn show_spinner(&self) {
        let stack = self.imp().stack.get();
        if stack.visible_child_name().is_some_and(|name| name != "spinner") {
            stack.set_visible_child_name("spinner");
        }
    }

    pub fn show_placeholder(&self) {
        let stack = self.imp().stack.get();
        if stack.child_by_name("placeholder").is_some() &&
            stack.visible_child_name().is_some_and(|name| name != "placeholder")
        {
            stack.set_visible_child_name("placeholder");
        }
    }

    pub fn show_content(&self) {
        let stack = self.imp().stack.get();
        if stack.child_by_name("content").is_some() &&
            stack.visible_child_name().is_some_and(|name| name != "content")
        {
            stack.set_visible_child_name("content");
        }
    }
}
