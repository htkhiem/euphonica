use adw::prelude::*;
use glib::Object;
use gtk::{
    CompositeTemplate,
    glib::{self},
    subclass::prelude::*,
};

mod imp {
    use std::cell::OnceCell;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/switch-row.ui")]
    pub struct SwitchRow {
        #[template_child]
        pub inner: TemplateChild<adw::SwitchRow>,
        pub key: OnceCell<String>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for SwitchRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaSwitchRow";
        type Type = super::SwitchRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for SwitchRow {}

    impl WidgetImpl for SwitchRow {}
}

glib::wrapper! {
    pub struct SwitchRow(ObjectSubclass<imp::SwitchRow>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl SwitchRow {
    pub fn new(key: String, val: bool, title: &str, subtitle: &str) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().key.set(key);
        res.imp().inner.set_title(title);
        res.imp().inner.set_subtitle(subtitle);
        res.imp().inner.set_active(val);
        res
    }

    pub fn generate_config(&self) -> Option<(String, String)> {
        Some((
            self.imp().key.get().cloned().unwrap(),
            if self.imp().inner.is_active() {
                "yes".into()
            } else {
                "no".into()
            },
        ))
    }
}
