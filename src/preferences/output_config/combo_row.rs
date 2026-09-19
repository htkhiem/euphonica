use adw::prelude::*;
use glib::Object;
use gtk::{
    CompositeTemplate,
    glib::{self},
    subclass::prelude::*,
};

use crate::common::list_models::{DualStringList, DualStringObject};

mod imp {
    use std::cell::OnceCell;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/combo-row.ui")]
    pub struct ComboRow {
        #[template_child]
        pub enabled: TemplateChild<gtk::Switch>,
        #[template_child]
        pub inner: TemplateChild<adw::ComboRow>,
        pub key: OnceCell<&'static str>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ComboRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaComboRow";
        type Type = super::ComboRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ComboRow {}

    impl WidgetImpl for ComboRow {}
}

glib::wrapper! {
    pub struct ComboRow(ObjectSubclass<imp::ComboRow>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ComboRow {
    // If given val is not in options, will default to first element and enablement switch will still be off.
    pub fn new(
        key: &'static str,
        options: &DualStringList,
        val: Option<&str>, // on internal side
        title: &str,
        subtitle: &str,
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().key.set(key);
        res.imp().inner.set_title(title);
        res.imp().inner.set_subtitle(subtitle);
        res.imp().inner.set_model(Some(options));
        // Need to tell adw::ComboRow how to read our DualStringList
        res.imp().inner.set_expression(Some(&gtk::PropertyExpression::new(
            DualStringObject::static_type(),
            gtk::Expression::NONE,
            "display",
        )));
        if let Some(val) = val {
            let init_idx = options.find(val);
            if init_idx < options.n_items() {
                res.imp().inner.set_selected(init_idx);
                res.imp().enabled.set_active(true);
            }
        }
        res
    }

    pub fn generate_config(&self) -> Option<(&'static str, String)> {
        if self.imp().enabled.is_active() {
            Some((
                self.imp().key.get().cloned().unwrap(),
                self.imp()
                    .inner
                    .selected_item()
                    .and_downcast::<gtk::StringObject>()
                    .map(|so| so.string().to_string())
                    .unwrap_or("".into()),
            ))
        } else {
            None
        }
    }
}
