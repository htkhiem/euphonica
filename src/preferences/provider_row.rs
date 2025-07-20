use adw::prelude::*;
use glib::{clone, Object, Properties};
use gtk::{glib, subclass::prelude::*, CompositeTemplate};

use crate::utils::meta_provider_settings;

use super::IntegrationsPreferences;

mod imp {
    use std::cell::{Cell, RefCell};

    use adw::subclass::{action_row::ActionRowImpl, preferences_row::PreferencesRowImpl};

    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::ProviderRow)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/provider-row.ui")]
    pub struct ProviderRow {
        #[template_child]
        pub enabled: TemplateChild<gtk::Switch>,
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[property(get, set)]
        pub priority: Cell<i32>,
        #[property(get, set)]
        pub key: RefCell<String>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ProviderRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaProviderRow";
        type Type = super::ProviderRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for ProviderRow {}

    impl WidgetImpl for ProviderRow {}

    impl ListBoxRowImpl for ProviderRow {}

    impl PreferencesRowImpl for ProviderRow {}

    impl ActionRowImpl for ProviderRow {}
}

glib::wrapper! {
    pub struct ProviderRow(ObjectSubclass<imp::ProviderRow>)
    @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::Actionable, gtk::ConstraintTarget;
}

impl ProviderRow {
    pub fn new(
        controller: &IntegrationsPreferences,
        key: &str, // For accessing GSettings
        priority: i32,
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().priority.replace(priority);
        res.set_key(key.to_owned());
        res.setup_actions(controller);
        let settings = meta_provider_settings(key);
        // At minimum, each provider's GSettings schema must contain these two keys:
        // - "name": a GUI-friendly name string (s)
        // - "enabled" (b)
        res.upcast_ref::<adw::ActionRow>()
            .set_title(settings.string("name").as_str());
        settings
            .bind("enabled", &res.imp().enabled.get(), "active")
            .build();
        res
    }

    #[inline(always)]
    pub fn setup_actions(&self, controller: &IntegrationsPreferences) {
        self.imp().raise.connect_clicked(clone!(
            #[weak]
            controller,
            #[weak(rename_to = this)]
            self,
            move |_| {
                controller.on_raise_provider(this.priority());
            }
        ));

        self.imp().lower.connect_clicked(clone!(
            #[weak]
            controller,
            #[weak(rename_to = this)]
            self,
            move |_| {
                controller.on_lower_provider(this.priority());
            }
        ));
    }
    // Getters & setters for key & priority are derived.
}
