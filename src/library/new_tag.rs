use gtk::{
    CompositeTemplate, gio,
    glib::{self, Object, ParamSpec, ParamSpecString, clone},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::OnceCell;

use crate::window::EuphonicaWindow;

mod imp {
    use super::*;
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/new-tag.ui")]
    pub struct NewTag {
        #[template_child]
        pub name: TemplateChild<gtk::Entry>,
        #[template_child]
        pub add_btn: TemplateChild<gtk::Button>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for NewTag {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaNewTag";
        type Type = super::NewTag;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for NewTag {
        fn constructed(&self) {
            self.parent_constructed();

            let btn = self.add_btn.get();
            self.name
                .connect_text_notify(move |entry| {
                    btn.set_sensitive(entry.text_length() > 0);
                });
        }
    }

    impl WidgetImpl for NewTag {}

    impl BoxImpl for NewTag {}
}

glib::wrapper! {
    pub struct NewTag(ObjectSubclass<imp::NewTag>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl NewTag {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn connect_add<T: Fn(&str) + 'static>(&self, on_add: T) {
        self.imp().add_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                on_add(this.imp().name.text().as_str());
            }
        ));
    }
}
