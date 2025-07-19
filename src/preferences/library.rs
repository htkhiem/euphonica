use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{glib, gio, CompositeTemplate};

use glib::clone;

use crate::{cache::{get_doc_cache_path, get_image_cache_path, Cache}, utils};

mod imp {
    use std::cell::{Cell, OnceCell};

    use crate::cache::{get_app_cache_path, sqlite};

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

        #[template_child]
        pub n_recent_albums: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub n_recent_artists: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub n_recent_songs: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub pause_recent: TemplateChild<adw::SwitchRow>,

        #[template_child]
        pub image_cache_size: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub info_db_size: TemplateChild<adw::ActionRow>,
        #[template_child]
        pub open_cache_folder: TemplateChild<adw::ButtonRow>,
        #[template_child]
        pub refresh_cache_stats_btn: TemplateChild<gtk::Button>,

        pub n_async_in_progress: Cell<u8>
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

    impl ObjectImpl for LibraryPreferences {
        fn constructed(&self) {
            self.parent_constructed();

            self.refresh_cache_stats_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().refresh_cache_stats();
                }
            ));

            self.open_cache_folder.connect_activated(|_| {
                let _ = open::that(get_app_cache_path());
            });
        }
    }
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
        library_settings
            .bind("n-recent-albums", &imp.n_recent_albums.get(), "value")
            .build();
        library_settings
            .bind("n-recent-artists", &imp.n_recent_artists.get(), "value")
            .build();
        library_settings
            .bind("n-recent-songs", &imp.n_recent_songs.get(), "value")
            .build();
        library_settings
            .bind("pause-recent", &imp.pause_recent.get(), "value")
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
                let _ = library_settings.set_value(
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

    pub fn refresh_cache_stats(&self) {
        // Avoid spawning additional tasks when current ones have not concluded yet
        if self.imp().n_async_in_progress.get() == 0 {
            self.imp().image_cache_size.set_subtitle("Computing...");
            self.imp().info_db_size.set_subtitle("Computing...");
            self.imp().n_async_in_progress.set(3);

            gio::File::for_path(get_image_cache_path()).measure_disk_usage_async(
                gio::FileMeasureFlags::NONE,
                glib::source::Priority::DEFAULT,
                Option::<&gio::Cancellable>::None,
                None,
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |res: Result<(u64, u64, u64), glib::error::Error>| {
                        if let Ok((bytes, _, n_files)) = res {
                            let size_str = glib::format_size(bytes);
                            this.imp().image_cache_size.set_subtitle(
                                &format!("{n_files} file(s) ({size_str})")
                            );
                        }
                        this.imp().n_async_in_progress.set(this.imp().n_async_in_progress.get() - 1);
                    }
                )
            );

            gio::File::for_path(get_doc_cache_path()).measure_disk_usage_async(
                gio::FileMeasureFlags::NONE,
                glib::source::Priority::DEFAULT,
                Option::<&gio::Cancellable>::None,
                None,
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |res: Result<(u64, u64, u64), glib::error::Error>| {
                        if let Ok((bytes, _, _)) = res {
                            this.imp().info_db_size.set_subtitle(&glib::format_size(bytes));
                        }
                        this.imp().n_async_in_progress.set(this.imp().n_async_in_progress.get() - 1);
                    }
                )
            );
        } 
    }
}
