use std::rc::Rc;

use adw::subclass::prelude::*;
use gtk::{glib, CompositeTemplate};

use crate::{cache::Cache, client::MpdWrapper, player::Player};

use super::{ClientPreferences, IntegrationsPreferences, LibraryPreferences, UIPreferences};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/dialog.ui")]
    pub struct Preferences {
        #[template_child]
        pub client_tab: TemplateChild<ClientPreferences>,

        #[template_child]
        pub integrations_tab: TemplateChild<IntegrationsPreferences>,

        #[template_child]
        pub library_tab: TemplateChild<LibraryPreferences>,

        #[template_child]
        pub ui_tab: TemplateChild<UIPreferences>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Preferences {
        const NAME: &'static str = "EuphonicaPreferences";
        type Type = super::Preferences;
        type ParentType = adw::PreferencesDialog;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Preferences {}
    impl WidgetImpl for Preferences {}
    impl AdwDialogImpl for Preferences {}
    impl PreferencesDialogImpl for Preferences {}
}

glib::wrapper! {
    pub struct Preferences(ObjectSubclass<imp::Preferences>)
        @extends adw::PreferencesDialog, adw::Dialog, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::ShortcutManager;
}

impl Default for Preferences {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Preferences {
    pub fn new(client: Rc<MpdWrapper>, cache: Rc<Cache>, player: &Player) -> Self {
        let res = Self::default();

        res.imp().client_tab.get().setup(client, player);
        res.imp().library_tab.get().setup();
        res.imp().ui_tab.get().setup(); 
        res.imp().integrations_tab.get().setup(cache);
        
        res
    }

    pub fn update(&self) {
        self.imp().library_tab.get().refresh_cache_stats(); 
    }
}
