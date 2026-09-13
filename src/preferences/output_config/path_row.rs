use adw::prelude::*;
use glib::{Object, clone};
use gtk::{
    CompositeTemplate,
    glib::{self},
    subclass::prelude::*,
};

mod imp {
    use std::cell::{OnceCell, RefCell};

    use adw::subclass::{action_row::ActionRowImpl, preferences_row::PreferencesRowImpl};

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/switch-row.ui")]
    pub struct PathRow {
        #[template_child]
        pub browse: TemplateChild<gtk::Button>,
        #[template_child]
        pub clear: TemplateChild<gtk::Button>,
        pub key: OnceCell<String>,
        pub value: RefCell<Option<String>>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PathRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPathRow";
        type Type = super::PathRow;
        type ParentType = adw::ActionRow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PathRow {
        fn constructed(&self) {
            self.parent_constructed();

            self.clear.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().set_value(None);
                }
            ));
        }
    }

    impl WidgetImpl for PathRow {}

    impl ListBoxRowImpl for PathRow {}

    impl PreferencesRowImpl for PathRow {}

    impl ActionRowImpl for PathRow {}
}

glib::wrapper! {
    pub struct PathRow(ObjectSubclass<imp::PathRow>)
    @extends adw::ActionRow, adw::PreferencesRow, gtk::ListBoxRow, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::Actionable, gtk::ConstraintTarget;
}

impl PathRow {
    /// Subtitle is not supported as we're using it to display the selected path.
    /// Path should
    pub fn new(key: String, val: Option<String>, title: &str) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().key.set(key);
        res.set_title(title);
        res.set_value(val);
        res
    }

    pub fn set_value(&self, val: Option<String>) {
        if let Some(val) = val.as_deref() {
            self.set_subtitle(val);
            self.imp().clear.set_visible(true);
        } else {
            self.set_subtitle("");
            self.imp().clear.set_visible(false);
        }
        let _ = self.imp().value.replace(val);
    }

    pub fn generate_config(&self) -> Option<(String, String)> {
        if let Some(path) = self.imp().value.borrow().as_deref() {
            Some((self.imp().key.get().cloned().unwrap(), path.to_owned()))
        } else {
            None
        }
    }
}
