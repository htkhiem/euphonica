use adw::prelude::*;
use glib::Object;
use gtk::{
    CompositeTemplate,
    glib::{self},
    subclass::prelude::*,
};

mod imp {
    use std::cell::{Cell, OnceCell};

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/text-row.ui")]
    pub struct TextRow {
        #[template_child]
        pub enabled: TemplateChild<gtk::Switch>,
        #[template_child]
        pub inner: TemplateChild<adw::EntryRow>,
        pub key: OnceCell<&'static str>,
        pub decimal_digits: Cell<u8>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for TextRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaTextRow";
        type Type = super::TextRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for TextRow {}

    impl WidgetImpl for TextRow {}
}

glib::wrapper! {
    pub struct TextRow(ObjectSubclass<imp::TextRow>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl TextRow {
    pub fn new(key: &'static str, val: Option<&str>, title: &str) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().key.set(key);
        res.imp().inner.set_title(title);

        if let Some(val) = val {
            res.imp().inner.set_text(val);
            res.imp().enabled.set_active(true);
        }
        res
    }

    pub fn generate_config(&self) -> Option<(&'static str, String)> {
        if self.imp().enabled.is_active() {
            Some((
                self.imp().key.get().cloned().unwrap(),
                self.imp().inner.text().to_string(),
            ))
        } else {
            None
        }
    }
}
