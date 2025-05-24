/* application.rs
 *
 * Copyright 2024 htkhiem2000
 *
 * This program is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * This program is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 *
 * SPDX-License-Identifier: GPL-3.0-or-later
 */
use adw::subclass::prelude::*;
use gtk::prelude::*;
use gtk::{gio, glib};
use std::{fs::create_dir_all, path::PathBuf, rc::Rc};
use gio::prelude::*;

use crate::{
    cache::Cache,
    client::{BackgroundTask, MpdWrapper},
    config::{APPLICATION_USER_AGENT, VERSION},
    library::Library,
    player::Player,
    preferences::Preferences,
    EuphonicaWindow,
};

use adw::prelude::*;

mod imp {
    use std::cell::RefCell;

    use glib::clone;

    use crate::utils;

    use super::*;

    #[derive(Debug)]
    pub struct EuphonicaApplication {
        pub player: Player,
        pub library: Library,
        pub cache: Rc<Cache>,
        // pub library: Rc<LibraryController>, // TODO
        pub client: Rc<MpdWrapper>,
        pub cache_path: PathBuf, // Just clone this to construct more detailed paths
        pub hold_guard: RefCell<Option<gio::ApplicationHoldGuard>>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EuphonicaApplication {
        const NAME: &'static str = "EuphonicaApplication";
        type Type = super::EuphonicaApplication;
        type ParentType = adw::Application;

        fn new() -> Self {
            // Create cache folder. This is where the cached album arts go.
            let mut cache_path: PathBuf = glib::user_cache_dir();
            cache_path.push("euphonica");
            // println!("Cache path: {}", cache_path.to_str().unwrap());
            create_dir_all(&cache_path).expect("Could not create temporary directories!");

            // Create cache controller
            let cache = Cache::new(&cache_path);
            let meta_sender = cache.get_sender();

            // Create client instance (not connected yet)
            let client = MpdWrapper::new(meta_sender.clone());

            // Create controllers
            // These two are GObjects (already refcounted by GLib)
            let player = Player::default();
            let library = Library::default();
            cache.set_mpd_client(client.clone());

            Self {
                player,
                library,
                client,
                cache,
                cache_path,
                hold_guard: RefCell::new(None)
            }
        }
    }

    impl ObjectImpl for EuphonicaApplication {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.setup_gactions();
            obj.set_accels_for_action("app.quit", &["<primary>q"]);
            obj.set_accels_for_action("app.fullscreen", &["F11"]);
            obj.set_accels_for_action("app.refresh", &["F5"]);

            self.library.setup(self.client.clone(), self.cache.clone());
            self.player
                .setup(self.obj().clone(), self.client.clone(), self.cache.clone());

            // Background mode
            let settings = utils::settings_manager().child("state");
            if settings.boolean("run-in-background") {
                println!("Creating new hold guard");
                self.hold_guard.replace(Some(self.obj().hold()));
            }
            else {
                println!("Dropping hold guard");
                self.hold_guard.take();
            }

            settings.connect_changed(
                Some("run-in-background"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |settings, _| {
                        if settings.boolean("run-in-background") {
                            println!("Creating new hold guard");
                            this.hold_guard.replace(Some(this.obj().hold()));
                        }
                        else {
                            println!("Dropping hold guard");
                            this.hold_guard.take();
                        }
                    }
                )
            );
        }
    }

    impl ApplicationImpl for EuphonicaApplication {
        // We connect to the activate callback to create a window when the application
        // has been launched. Additionally, this callback notifies us when the user
        // tries to launch a "second instance" of the application. When they try
        // to do that, we'll just present any existing window.
        fn activate(&self) {
            let application = self.obj();
            // Get the current window or create one if necessary
            let window = if let Some(window) = application.active_window() {
                window
            } else {
                let window = EuphonicaWindow::new(&*application);
                window.upcast()
            };

            // Ask the window manager/compositor to present the window
            window.present();

            // Piggyback on the refresh method to trigger a connect attempt.
            // Start attempting to connect to the daemon once the window has been displayed.
            // This avoids delaying the presentation until the connection process concludes.
            application.refresh();
        }
    }

    impl GtkApplicationImpl for EuphonicaApplication {}
    impl AdwApplicationImpl for EuphonicaApplication {}
}

glib::wrapper! {
    pub struct EuphonicaApplication(ObjectSubclass<imp::EuphonicaApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl EuphonicaApplication {
    pub fn new(application_id: &str, flags: &gio::ApplicationFlags) -> Self {
        // TODO: Find a better place to put these
        musicbrainz_rs::config::set_user_agent(APPLICATION_USER_AGENT);
        glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .build()
    }

    pub fn get_player(&self) -> Player {
        self.imp().player.clone()
    }

    pub fn get_library(&self) -> Library {
        self.imp().library.clone()
    }

    pub fn get_cache(&self) -> Rc<Cache> {
        self.imp().cache.clone()
    }

    pub fn get_client(&self) -> Rc<MpdWrapper> {
        self.imp().client.clone()
    }

    fn setup_gactions(&self) {
        let toggle_fullscreen_action = gio::ActionEntry::builder("fullscreen")
            .activate(move |app: &Self, _, _| app.toggle_fullscreen())
            .build();
        let refresh_action = gio::ActionEntry::builder("refresh")
            .activate(move |app: &Self, _, _| app.refresh())
            .build();
        let update_db_action = gio::ActionEntry::builder("update-db")
            .activate(move |app: &Self, _, _| app.update_db())
            .build();
        let quit_action = gio::ActionEntry::builder("quit")
            .activate(move |app: &Self, _, _| app.quit())
            .build();
        let about_action = gio::ActionEntry::builder("about")
            .activate(move |app: &Self, _, _| app.show_about())
            .build();
        let preferences_action = gio::ActionEntry::builder("preferences")
            .activate(move |app: &Self, _, _| app.show_preferences())
            .build();
        self.add_action_entries([
            toggle_fullscreen_action,
            refresh_action,
            update_db_action,
            quit_action,
            about_action,
            preferences_action,
        ]);
    }

    fn toggle_fullscreen(&self) {
        let window = self.active_window().unwrap();
        self.set_fullscreen(!window.is_fullscreen());
    }

    pub fn is_fullscreen(&self) -> bool {
        self.active_window().unwrap().is_fullscreen()
    }

    pub fn set_fullscreen(&self, state: bool) {
        let window = self.active_window().unwrap();
        if state {
            window.fullscreen();
            // Send a toast with instructions on how to return to windowed mode
            window
                .downcast_ref::<EuphonicaWindow>()
                .unwrap()
                .send_simple_toast("Press F11 to exit fullscreen", 3);
        } else {
            window.unfullscreen();
        }
    }

    pub fn raise_window(&self) {
        let window = self.active_window().unwrap();
        window.present();
    }

    fn refresh(&self) {
        self.imp().client.clone().queue_connect();
    }

    fn update_db(&self) {
        self.imp()
            .client
            .clone()
            .queue_background(BackgroundTask::Update, true);
    }

    pub fn show_about(&self) {
        let window = self.active_window().unwrap();
        let about = adw::AboutDialog::builder()
            .application_name("Euphonica")
            .application_icon("io.github.htkhiem.Euphonica")
            .developer_name("htkhiem2000")
            .version(VERSION)
            .developers(vec!["htkhiem2000"])
            .license_type(gtk::License::Gpl30)
            .copyright("© 2024 htkhiem2000")
            .build();

        about.add_credit_section(
            Some("Special Thanks"),
            &["Emmanuele Bassi (GTK, LibAdwaita, the Amberol project) https://www.bassi.io/"],
        );
        about.present(Some(&window));
    }

    pub fn show_preferences(&self) {
        let window = self.active_window().unwrap();
        let prefs = Preferences::new(
            self.imp().client.clone(),
            self.imp().cache.clone(),
            &self.imp().player,
        );
        prefs.present(Some(&window));
        prefs.update();
    }
}
