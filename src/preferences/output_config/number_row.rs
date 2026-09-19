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
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/number-row.ui")]
    pub struct NumberRow {
        #[template_child]
        pub enabled: TemplateChild<gtk::Switch>,
        #[template_child]
        pub inner: TemplateChild<adw::SpinRow>,
        pub key: OnceCell<&'static str>,
        pub decimal_digits: Cell<u8>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for NumberRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaNumberRow";
        type Type = super::NumberRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for NumberRow {}

    impl WidgetImpl for NumberRow {}
}

glib::wrapper! {
    pub struct NumberRow(ObjectSubclass<imp::NumberRow>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl NumberRow {
    pub fn new(
        key: &'static str,
        adj: gtk::Adjustment,
        title: &str,
        subtitle: &str,
        decimal_digits: u8,
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().key.set(key);
        res.imp().inner.set_adjustment(Some(&adj));
        res.imp().inner.set_title(title);
        res.imp().inner.set_subtitle(subtitle);
        res.imp().decimal_digits.set(decimal_digits);
        res
    }

    pub fn generate_config(&self) -> Option<(&'static str, String)> {
        if self.imp().enabled.is_active() {
            Some((
                self.imp().key.get().unwrap(),
                format!(
                    "{0:.1$}",
                    self.imp().inner.value(),
                    self.imp().decimal_digits.get() as usize
                ),
            ))
        } else {
            None
        }
    }
}
