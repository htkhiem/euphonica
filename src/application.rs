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

use adw::prelude::*;
use crate::{
    cache::Cache,
    client::{BackgroundTask, MpdWrapper},
    config::{APPLICATION_USER_AGENT, VERSION},
    library::Library,
    player::Player,
    preferences::Preferences,
    utils::{settings_manager, tokio_runtime},
    EuphonicaWindow
};
use adw::subclass::prelude::*;
use gtk::{gio, glib};
use std::{
    cell::{Cell, OnceCell, RefCell}, fs::create_dir_all, ops::ControlFlow, path::PathBuf, rc::Rc
};

use ashpd::desktop::background::Background;

pub fn update_xdg_background_request() {
    let settings = settings_manager().child("state");
    let run_in_background = settings.boolean("run-in-background");
    let autostart = settings.boolean("autostart");
    let start_minimized = settings.boolean("start-minimized");

    tokio_runtime().spawn(async move {
        let mut request = Background::request()
            .reason("Run Euphonica in the background")
            .dbus_activatable(false);

        if autostart {
            request = request
                .auto_start(true);
            if start_minimized {
                request = request.command(&["euphonica", "--minimized"])
            }
        }

        match request.send().await {
            Ok(request) => {
                let settings = settings_manager();
                if let Ok(response) = request.response() {
                    let _ = settings.set_boolean("background-portal-available", true);
                    let state_settings = settings.child("state");

                    // Might have to turn them off if system replies negatively
                    let _ = state_settings.set_boolean("autostart", response.auto_start());
                    // Since we call the above regardless of whether we wish to run in background
                    // or not (to update autostart) we need to do an AND here.
                    let _ = state_settings.set_boolean("run-in-background", run_in_background && response.run_in_background());
                }
            }
            Err(_) => {
                let settings = settings_manager();
                let _ = settings.set_boolean("background-portal-available", false);
            }
        }
    });
}

mod imp {
    use super::*;

    #[derive(Debug)]
    pub struct EuphonicaApplication {
        pub initialized: Cell<bool>,
        pub start_minimized: Cell<bool>,
        pub player: OnceCell<Player>,
        pub library: OnceCell<Library>,
        pub cache: OnceCell<Rc<Cache>>,
        // pub library: Rc<LibraryController>, // TODO
        pub client: OnceCell<Rc<MpdWrapper>>,
        pub cache_path: PathBuf, // Just clone this to construct more detailed paths
        pub hold_guard: RefCell<Option<gio::ApplicationHoldGuard>>,
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

            Self {
                initialized: Cell::new(false),
                start_minimized: Cell::new(false),
                player: OnceCell::new(),
                library: OnceCell::new(),
                client: OnceCell::new(),
                cache: OnceCell::new(),
                cache_path,
                hold_guard: RefCell::new(None),
            }
        }
    }

    impl ObjectImpl for EuphonicaApplication {
        fn constructed(&self) {
            self.parent_constructed();
        }
    }

    impl ApplicationImpl for EuphonicaApplication {
        // We connect to the activate callback to create a window when the application
        // has been launched. Additionally, this callback notifies us when the user
        // tries to launch a "second instance" of the application. When they try
        // to do that, we'll just present any existing window.
        fn activate(&self) {
            let application = self.obj();

            if !self.initialized.get() {
                println!("Creating a new Euphonica instance...");
                // Put init logic here to ensure they're only called on the primary instance.
                // This is to both avoid unneeded processing and creation of bogus child threads
                // that stick around (only a problem now that Euphonica can be left running in
                // the background, and the the easiest way to call it back to foreground is to
                // click on the desktop icon again, spawning another instance which should
                // only live briefly to pass args to the primary one).
                // Create cache controller
                let cache = Cache::new();
                let meta_sender = cache.get_sender();

                // Create client instance (not connected yet)
                let client = MpdWrapper::new(meta_sender.clone());

                // Create controllers
                // These two are GObjects (already refcounted by GLib)
                let player = Player::default();
                let library = Library::default();
                cache.set_mpd_client(client.clone());

                let _ = self.cache.set(cache);
                let _ = self.client.set(client);
                let _ = self.library.set(library);
                let _ = self.player.set(player);

                let obj = self.obj();
                obj.setup_gactions();
                obj.set_accels_for_action("app.quit", &["<primary>q"]);
                obj.set_accels_for_action("app.fullscreen", &["F11"]);
                obj.set_accels_for_action("app.refresh", &["F5"]);

                self.library.get().unwrap().setup(
                    self.client.get().unwrap().clone(),
                    self.cache.get().unwrap().clone(),
                    self.player.get().unwrap().clone(),
                );
                self.player.get().unwrap().setup(
                    self.obj().clone(),
                    self.client.get().unwrap().clone(),
                    self.cache.get().unwrap().clone(),
                );

                application.refresh();

                self.initialized.set(true);

                // If this is the main instance, respect the minimized flag
                if !self.start_minimized.get() {
                    self.player.get().unwrap().set_is_foreground(true);
                    self.obj().raise_window();
                }
            }
            else {
                // Not the main instance -> not starting a new one -> always open a window regardless
                // of whether the main instance was started with the --minimized flag or not.
                self.obj().raise_window();
            }
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
        let app: EuphonicaApplication = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .build();

        app.connect_handle_local_options(|this: &Self, vd: &glib::VariantDict| {
            if vd.lookup_value("minimized", None).is_some() {
                this.imp().start_minimized.set(true);
            }
            ControlFlow::Continue(())  // let execution continue
        });

        // Background mode
        update_xdg_background_request();

        app
    }

    pub fn get_player(&self) -> Player {
        self.imp().player.get().unwrap().clone()
    }

    pub fn get_library(&self) -> Library {
        self.imp().library.get().unwrap().clone()
    }

    pub fn get_cache(&self) -> Rc<Cache> {
        self.imp().cache.get().unwrap().clone()
    }

    pub fn get_client(&self) -> Rc<MpdWrapper> {
        self.imp().client.get().unwrap().clone()
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
        // Overrides background mode and ends instance
        let quit_action = gio::ActionEntry::builder("quit")
            .activate(move |app: &Self, _, _| app.quit_app())
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
            preferences_action
        ]);
    }

    fn toggle_fullscreen(&self) {
        let window = self.active_window().unwrap();
        self.set_fullscreen(!window.is_fullscreen());
    }

    pub fn is_fullscreen(&self) -> bool {
        if let Some(window) = self.active_window() {
            window.is_fullscreen()
        }
        else {
            false
        }
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
        let window = if let Some(window) = self.active_window() {
            window
        } else {
            let window = EuphonicaWindow::new(&*self);
            window.upcast()
        };
        self.imp().player.get().unwrap().set_is_foreground(true);
        window.present();
    }

    pub fn on_window_closed(&self) {
        let settings = settings_manager().child("state");
        if settings.boolean("run-in-background") {
            self.imp().player.get().unwrap().set_is_foreground(false);
            if let Some(_) = self.imp().hold_guard.replace(Some(self.hold())) {
                println!("Created a new hold guard");
            }
        } else {
            println!("Dropping hold guard");
            self.imp().hold_guard.take();
        }
    }

     fn refresh(&self) {
        self.get_client().queue_connect();
    }

    fn update_db(&self) {
        self.get_client()
            .queue_background(BackgroundTask::Update, true);
    }

    pub fn show_about(&self) {
        let window = self.active_window().unwrap();
        let about = adw::AboutDialog::builder()
            .application_name("Euphonica")
            .application_icon("io.github.htkhiem.Euphonica")
            .developer_name("htkhiem2000")
            .version(VERSION)
            .developers(vec!["htkhiem2000", "sonicv6"])
            .license_type(gtk::License::Gpl30)
            .copyright("© 2025 htkhiem2000")
            .build();

        about.add_credit_section(
            Some("Special Thanks"),
            &["Emmanuele Bassi (GTK, LibAdwaita, the Amberol project) https://www.bassi.io/"],
        );
        about.present(Some(&window));
    }

    pub fn show_preferences(&self) {
        let window = self.active_window().unwrap();
        let prefs = Preferences::new(self.get_client(), self.get_cache(), &self.get_player());
        prefs.present(Some(&window));
        prefs.update();
    }

    /// Quit Euphonica. Useful for when run-in-background is true. Otherwise just close the window.
    pub fn quit_app(&self) {
        self.imp().hold_guard.take();
        self.quit();
    }
}
