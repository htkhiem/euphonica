use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, CompositeTemplate};

use glib::clone;

use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/library.ui")]
    pub struct LibraryPreferences {
        #[template_child]
        pub sort_nulls_first: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub sort_case_sensitive: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub search_case_sensitive: TemplateChild<adw::SwitchRow>,

        #[template_child]
        pub artist_delims: TemplateChild<gtk::TextView>,
        #[template_child]
        pub artist_delims_apply: TemplateChild<gtk::Button>,
        #[template_child]
        pub artist_excepts: TemplateChild<gtk::TextView>,
        #[template_child]
        pub artist_excepts_apply: TemplateChild<gtk::Button>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for LibraryPreferences {
        const NAME: &'static str = "EuphonicaLibraryPreferences";
        type Type = super::LibraryPreferences;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for LibraryPreferences {}
    impl WidgetImpl for LibraryPreferences {}
    impl PreferencesPageImpl for LibraryPreferences {}
}

glib::wrapper! {
    pub struct LibraryPreferences(ObjectSubclass<imp::LibraryPreferences>)
        @extends adw::PreferencesPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Widget;
}

impl Default for LibraryPreferences {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl LibraryPreferences {
    pub fn setup(&self) {
        let imp = self.imp();

        // Populate with current gsettings values
        let settings = utils::settings_manager();
        // Set up library settings
        let library_settings = settings.child("library");
        let sort_nulls_first = imp.sort_nulls_first.get();
        library_settings
            .bind("sort-nulls-first", &sort_nulls_first, "active")
            .build();
        let sort_case_sensitive = imp.sort_case_sensitive.get();
        library_settings
            .bind("sort-case-sensitive", &sort_case_sensitive, "active")
            .build();
        let search_case_sensitive = imp.search_case_sensitive.get();
        library_settings
            .bind("search-case-sensitive", &search_case_sensitive, "active")
            .build();

        // Setup artist section
        let artist_delims_buf = imp.artist_delims.buffer();
        let artist_delims_apply = imp.artist_delims_apply.get();
        artist_delims_buf.set_text(
            &library_settings
                .value("artist-tag-delims")
                .array_iter_str()
                .unwrap()
                .collect::<Vec<&str>>()
                .join("\n"),
        );
        artist_delims_buf.connect_changed(clone!(
            #[weak]
            artist_delims_apply,
            move |_| {
                artist_delims_apply.set_sensitive(true);
            }
        ));
        artist_delims_apply.connect_clicked(clone!(
            #[weak]
            library_settings,
            #[weak]
            artist_delims_buf,
            move |btn| {
                library_settings.set_value(
                    "artist-tag-delims",
                    &artist_delims_buf
                        .text(
                            &artist_delims_buf.start_iter(),
                            &artist_delims_buf.end_iter(),
                            false,
                        )
                        .to_string()
                        .lines()
                        .collect::<Vec<&str>>()
                        .to_variant(),
                );
                btn.set_sensitive(false);
                // Reinitialise the automaton
                utils::rebuild_artist_delim_automaton();
            }
        ));

        let artist_excepts_buf = imp.artist_excepts.buffer();
        let artist_excepts_apply = imp.artist_excepts_apply.get();
        artist_excepts_buf.set_text(
            &library_settings
                .value("artist-tag-delim-exceptions")
                .array_iter_str()
                .unwrap()
                .collect::<Vec<&str>>()
                .join("\n"),
        );
        artist_excepts_buf.connect_changed(clone!(
            #[weak]
            artist_excepts_apply,
            move |_| {
                artist_excepts_apply.set_sensitive(true);
            }
        ));
        artist_excepts_apply.connect_clicked(clone!(
            #[weak]
            library_settings,
            #[weak]
            artist_excepts_buf,
            move |btn| {
                let _ = library_settings.set_value(
                    "artist-tag-delim-exceptions",
                    &artist_excepts_buf
                        .text(
                            &artist_excepts_buf.start_iter(),
                            &artist_excepts_buf.end_iter(),
                            false,
                        )
                        .to_string()
                        .lines()
                        .collect::<Vec<&str>>()
                        .to_variant(),
                );
                btn.set_sensitive(false);
                // Reinitialise the automaton
                utils::rebuild_artist_delim_exception_automaton();
            }
        ));
    }
}
