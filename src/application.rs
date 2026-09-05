/* application.rs
 *
 * Copyright 2026 htkhiem2000
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

use crate::{
    EuphonicaWindow,
    cache::Cache,
    client::{MpdWrapper, Result as ClientResult},
    config::VERSION,
    library::Library,
    onboarding::EuphonicaOnboardingWindow,
    player::{Player, get_next_replaygain},
    preferences::Preferences,
    server::{ManagedMpdError, ManagedMpdServer},
    utils::{settings_manager, tokio_runtime},
};
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    gio,
    glib::{self, clone},
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    fs::create_dir_all,
    ops::ControlFlow,
    path::PathBuf,
    rc::Rc,
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
            request = request.auto_start(true);
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
                    let _ = state_settings.set_boolean(
                        "run-in-background",
                        run_in_background && response.run_in_background(),
                    );
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
    use crate::utils::{get_app_cache_path, get_config_basepath};

    use super::*;

    #[derive(Debug)]
    pub struct EuphonicaApplication {
        pub initialized: Cell<bool>,
        pub start_minimized: Cell<bool>,
        pub player: Player,
        pub library: Library,
        pub cache: OnceCell<Rc<Cache>>,
        pub client: OnceCell<Rc<MpdWrapper>>,
        pub server: ManagedMpdServer,
        pub cache_path: PathBuf, // Just clone this to construct more detailed paths
        pub hold_guard: RefCell<Option<gio::ApplicationHoldGuard>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EuphonicaApplication {
        const NAME: &'static str = "EuphonicaApplication";
        type Type = super::EuphonicaApplication;
        type ParentType = adw::Application;

        fn new() -> Self {
            // Create cache folder
            let cache_path: PathBuf = get_app_cache_path();
            let mut server_config_path: PathBuf = get_config_basepath();
            server_config_path.push("playlists"); // create down to this folder too for managed playlists
            // println!("Cache path: {}", cache_path.to_str().unwrap());
            create_dir_all(&cache_path).expect("Could not create temporary directories!");
            create_dir_all(&server_config_path).expect("Could not create temporary directories!");

            Self {
                initialized: Cell::new(false),
                start_minimized: Cell::new(false),
                player: Player::default(),
                library: Library::default(),
                client: OnceCell::new(),
                server: ManagedMpdServer::default(),
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

        fn dispose(&self) {
            let _ = self.server.stop();
        }
    }

    impl ApplicationImpl for EuphonicaApplication {
        // We connect to the activate callback to create a window when the application
        // has been launched. Additionally, this callback notifies us when the user
        // tries to launch a "second instance" of the application. When they try
        // to do that, we'll just present any existing window.
        fn activate(&self) {
            if !self.initialized.get() {
                println!("Creating a new Euphonica instance...");
                // Put init logic here to ensure they're only called on the primary instance.
                // This is to both avoid unneeded processing and creation of bogus child threads
                // that stick around (only a problem now that Euphonica can be left running in
                // the background, and the the easiest way to call it back to foreground is to
                // click on the desktop icon again, spawning another instance which should
                // only live briefly to pass args to the primary one).

                // Create client instance (not connected yet)
                let client = MpdWrapper::new();
                // Create cache controller
                let cache = Cache::new(client.clone());
                let _ = self.client.set(client);
                let _ = self.cache.set(cache);

                // Check whether we need to show the onboarding wizard
                let state = settings_manager().child("state");
                if state.boolean("onboarded") {
                    // Onboarded => show main window directly.
                    // Window is initialised early so we can show startup errors like connection issues.
                    if !self.start_minimized.get() {
                        self.obj().raise_window();
                        self.continue_startup();
                    } else {
                        let _ = self.hold_guard.replace(Some(self.obj().hold()));
                        self.continue_startup();
                    }
                } else {
                    // NOT onboarded yet.
                    if !self.start_minimized.get() {
                        // Show onboarding wizard
                        let onboarding_wizard = EuphonicaOnboardingWindow::new(self.obj().as_ref());
                        // Keep app alive as we transition from onboarding wizard to main window
                        let _ = self.hold_guard.replace(Some(self.obj().hold()));
                        onboarding_wizard.present();
                    } else {
                        // Keep the onboarded flag at false and yell in stderr
                        eprintln!(
                            "WARNING: not yet onboarded. Using default settings where possible. If this is your first time using Euphonica, the default settings might not be suitable for your MPD setup and might fail to connect."
                        );
                        let _ = self.hold_guard.replace(Some(self.obj().hold()));
                        self.continue_startup();
                    }
                }
            } else {
                // Not the main instance -> not starting a new one -> always open a window regardless
                // of whether the main instance was started with the --minimized flag or not.
                self.obj().raise_window();
            }
        }
    }

    impl GtkApplicationImpl for EuphonicaApplication {}
    impl AdwApplicationImpl for EuphonicaApplication {}

    impl EuphonicaApplication {
        pub fn handle_managed_server_error(&self, e: ManagedMpdError) {
            if let Some(win) = self.obj().active_window().and_downcast::<EuphonicaWindow>() {
                win.show_error_dialog(
                    "Failed to start MPD server",
                    match e {
                        ManagedMpdError::Config => "MPD server manager reported a configuration issue. Please review your configuration in Preferences.",
                        ManagedMpdError::NotConfigured => "Euphonica was started in Standalone Mode without having been set up for it. Please open Preferences to set things up or switch to an external MPD server.",
                        ManagedMpdError::Subprocess => "The MPD subprocess failed to start. Ensure you are running MPD 0.23 or greater.",
                        _ => "Unknown error."
                    },
                    !matches!(e, ManagedMpdError::Subprocess)
                );
            }
        }
        /// Continue startup by initialising the two controllers.
        /// This expects the clients to have been initialised and connected.
        /// Now a separate function as we may either directly enter the main UI or go through
        /// an onboarding wizard first.
        pub fn continue_startup(&self) {
            let client = self.client.get().unwrap();
            let cache = self.cache.get().unwrap();

            // Create controllers
            // These two are GObjects (already refcounted by GLib)
            let player = &self.player;
            player.setup(self.obj().clone(), client.clone(), cache.clone());
            let library = &self.library;
            library.setup(client.clone(), player.clone(), cache.clone());

            let obj = self.obj();
            obj.setup_gactions();
            // See https://github.com/GNOME/gtk/blob/main/gdk/gdkkeysyms.h for key names
            obj.set_accels_for_action("app.quit", &["<primary>q"]);
            obj.set_accels_for_action("app.fullscreen", &["F11"]);
            obj.set_accels_for_action("app.refresh", &["F5"]);
            obj.set_accels_for_action("app.update-db", &["F6"]);
            obj.set_accels_for_action("app.toggle-visualizer", &["F8"]);

            // Playback shortcuts
            obj.set_accels_for_action("app.toggle-playback", &["<Ctrl>p"]);
            obj.set_accels_for_action("app.next-song", &["<Shift>greater"]);
            obj.set_accels_for_action("app.prev-song", &["<Shift>less"]);
            obj.set_accels_for_action("app.seek-forward", &["<Ctrl><Shift>f"]);
            obj.set_accels_for_action("app.seek-backward", &["<Ctrl><Shift>b"]);
            obj.set_accels_for_action("app.stop", &["<Ctrl><Shift>s"]);
            obj.set_accels_for_action("app.toggle-random", &["<Alt>z"]);
            obj.set_accels_for_action("app.cycle-flow", &["<Alt>r"]);
            obj.set_accels_for_action("app.toggle-consume", &["<Alt><Shift>r"]);
            obj.set_accels_for_action("app.cycle-replaygain", &["<Alt><Shift>y"]);
            obj.set_accels_for_action("app.cycle-crossfade", &["<Alt>x"]);
            obj.set_accels_for_action("app.volume-up", &["<Ctrl><Shift>Up"]);
            obj.set_accels_for_action("app.volume-down", &["<Ctrl><Shift>Down"]);
            obj.set_accels_for_action("app.toggle-mute", &["<Ctrl>m"]);
            obj.set_accels_for_action("app.next-output", &["<Ctrl><Shift>Right"]);
            obj.set_accels_for_action("app.prev-output", &["<Ctrl><Shift>Left"]);
            obj.set_accels_for_action("app.toggle-output", &["<Ctrl>slash"]);

            let app = self.obj();

            glib::spawn_future_local(clone!(
                #[weak]
                app,
                async move {
                    if let Err(e) = app.refresh().await {
                        dbg!(e);
                    }
                }
            ));

            self.initialized.set(true);
        }
    }
}

glib::wrapper! {
    pub struct EuphonicaApplication(ObjectSubclass<imp::EuphonicaApplication>)
        @extends gio::Application, gtk::Application, adw::Application,
    @implements gio::ActionGroup, gio::ActionMap;
}

impl EuphonicaApplication {
    pub fn new(application_id: &str, flags: &gio::ApplicationFlags) -> Self {
        let app: EuphonicaApplication = glib::Object::builder()
            .property("application-id", application_id)
            .property("flags", flags)
            .build();

        app.connect_handle_local_options(|this: &Self, vd: &glib::VariantDict| {
            if vd.lookup_value("minimized", None).is_some() {
                this.imp().start_minimized.set(true);
            }
            ControlFlow::Continue(())
        });

        // Background mode
        update_xdg_background_request();

        app
    }

    pub fn conclude_onboarding(&self, should_continue: bool) {
        if should_continue {
            let state = settings_manager().child("state");
            let _ = state.set_boolean("onboarded", true);
            self.imp().continue_startup();
            // Explicitly create window to force the app to bind to it instead of the will-be-destroyed onboarding one
            let _ = EuphonicaWindow::new(self);
            self.raise_window();
            // Safe to remove hold guard now, if any
            let _ = self.imp().hold_guard.take();
        } else {
            eprintln!("FATAL: user exited onboarding.");
            self.quit_app();
        }
    }

    pub fn get_player(&self) -> &Player {
        &self.imp().player
    }

    pub fn get_library(&self) -> &Library {
        &self.imp().library
    }

    pub fn get_cache(&self) -> Rc<Cache> {
        self.imp().cache.get().unwrap().clone()
    }

    pub fn get_client(&self) -> Rc<MpdWrapper> {
        self.imp().client.get().unwrap().clone()
    }

    pub fn get_server(&self) -> &ManagedMpdServer {
        &self.imp().server
    }

    /// Set up app-level shortcuts. These are shortcuts that will work regardless of which Euphonica window is being focused.
    /// We currently only have a single window, but who knows, maybe in the future we can have multiple windows?
    fn setup_gactions(&self) {
        let toggle_fullscreen_action = gio::ActionEntry::builder("fullscreen")
            .activate(move |this: &Self, _, _| this.toggle_fullscreen())
            .build();
        let refresh_action = gio::ActionEntry::builder("refresh")
            .activate(move |this: &Self, _, _| {
                glib::spawn_future_local(clone!(
                    #[weak(rename_to = this)]
                    this,
                    async move {
                        if let Err(e) = this.refresh().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();
        let update_db_action = gio::ActionEntry::builder("update-db")
            .activate(move |this: &Self, _, _| this.update_db())
            .build();
        // Overrides background mode and ends instance
        let quit_action = gio::ActionEntry::builder("quit")
            .activate(move |this: &Self, _, _| this.quit_app())
            .build();
        let about_action = gio::ActionEntry::builder("about")
            .activate(move |this: &Self, _, _| this.show_about())
            .build();
        let preferences_action = gio::ActionEntry::builder("preferences")
            .activate(move |this: &Self, _, _| this.show_preferences())
            .build();

        let player_toggle_playback_action = gio::ActionEntry::builder("toggle-playback")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = player.toggle_playback().await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_next_song_action = gio::ActionEntry::builder("next-song")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = player.next_song().await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_prev_song_action = gio::ActionEntry::builder("prev-song")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = player.prev_song().await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_seek_forward_action = gio::ActionEntry::builder("seek-forward")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    let pos = player.position() + 10.0;
                    if let Err(e) = player.send_seek(pos).await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_seek_backward_action = gio::ActionEntry::builder("seek-backward")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    let pos = player.position() - 10.0;
                    if pos < 0.0 {
                        let _ = player.send_seek(0.0).await;
                    } else if let Err(e) = player.send_seek(pos).await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_stop_action = gio::ActionEntry::builder("stop")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = player.stop().await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_toggle_random_action = gio::ActionEntry::builder("toggle-random")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                let win = this.active_window().and_downcast::<EuphonicaWindow>();
                glib::spawn_future_local(async move {
                    let new_state = !player.random();
                    if let Err(e) = player.set_random(new_state).await {
                        dbg!(e);
                    } else if let Some(win) = win {
                        // Toast since it's not immediately obvious
                        win.send_simple_toast(
                            &format!("Random: {}", if new_state { "On" } else { "Off" }),
                            3,
                        );
                    }
                });
            })
            .build();

        let player_cycle_flow_action = gio::ActionEntry::builder("cycle-flow")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                let win = this.active_window().and_downcast::<EuphonicaWindow>();
                glib::spawn_future_local(async move {
                    // We have to guess what the next flow is ourselves instead of immediately calling playback_flow() after awaiting.
                    // This is because the playback_flow in the controller is only updated after the next update_status().
                    let next_flow = player.playback_flow().next_in_cycle();
                    if let Err(e) = player.cycle_playback_flow().await {
                        dbg!(e);
                    } else if let Some(win) = win {
                        // Toast since it's not immediately obvious
                        win.send_simple_toast(
                            &format!("Playback Flow: {}", next_flow.description()),
                            3,
                        );
                    }
                });
            })
            .build();

        let player_toggle_consume_action = gio::ActionEntry::builder("toggle-consume")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                let win = this.active_window().and_downcast::<EuphonicaWindow>();
                glib::spawn_future_local(async move {
                    let new_state = !player.consume();
                    if let Err(e) = player.set_consume(new_state).await {
                        dbg!(e);
                    } else if let Some(win) = win {
                        // Toast since it's not immediately obvious
                        win.send_simple_toast(
                            &format!("Consume: {}", if new_state { "On" } else { "Off" }),
                            3,
                        );
                    }
                });
            })
            .build();

        let player_cycle_replaygain_action = gio::ActionEntry::builder("cycle-replaygain")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                let win = this.active_window().and_downcast::<EuphonicaWindow>();
                glib::spawn_future_local(async move {
                    let next_mode = get_next_replaygain(player.replaygain());
                    if let Err(e) = player.cycle_replaygain().await {
                        dbg!(e);
                    } else if let Some(win) = win {
                        // Toast since it's not immediately obvious
                        // TODO: have our own description instead of relying on rust-mpd's display impl
                        win.send_simple_toast(&format!("ReplayGain: {}", next_mode), 3);
                    }
                });
            })
            .build();

        let player_next_output_action = gio::ActionEntry::builder("next-output")
            .activate(move |this: &Self, _, _| {
                this.get_player().switch_output(false);
            })
            .build();

        let player_prev_output_action = gio::ActionEntry::builder("prev-output")
            .activate(move |this: &Self, _, _| {
                this.get_player().switch_output(true);
            })
            .build();

        let player_cycle_crossfade_action = gio::ActionEntry::builder("cycle-crossfade")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                let win = this.active_window().and_downcast::<EuphonicaWindow>();
                glib::spawn_future_local(async move {
                    // Keyboard shortcut will cycle through these values, a bit like ncmpcpp but more granular
                    let current = player.crossfade();
                    let next = match current {
                        0.0 => 1.0,
                        1.0 => 3.0,
                        3.0 => 5.0,
                        5.0 => 10.0,
                        10.0 => 0.0,
                        _ => 1.0, // Any other custom value will be set to 1 to enter the "predefined values loop"
                    };
                    if let Err(e) = player.set_crossfade(next).await {
                        dbg!(e);
                    } else if let Some(win) = win {
                        // Toast since it's not immediately obvious
                        if next == 0.0 {
                            win.send_simple_toast("Crossfade: Off", 3);
                        } else {
                            win.send_simple_toast(&format!("Crossfade: {:.1}s", next), 3);
                        }
                    }
                });
            })
            .build();

        let player_volume_up_action = gio::ActionEntry::builder("volume-up")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    let vol = player.mpd_volume().saturating_add(2).min(100);
                    if let Err(e) = player.send_set_volume(vol).await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_volume_down_action = gio::ActionEntry::builder("volume-down")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    let vol = player.mpd_volume().saturating_sub(2).max(0);
                    if let Err(e) = player.send_set_volume(vol).await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_toggle_mute_action = gio::ActionEntry::builder("toggle-mute")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = player.toggle_mute().await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let player_toggle_output_action = gio::ActionEntry::builder("toggle-output")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(async move {
                    if let Err(e) = player.toggle_current_output().await {
                        dbg!(e);
                    }
                });
            })
            .build();

        let toggle_visualizer_action = gio::ActionEntry::builder("toggle-visualizer")
            .activate(move |_, _, _| {
                let settings = settings_manager().child("ui");
                let _ = settings.set_boolean("use-visualizer", !settings.boolean("use-visualizer"));
            })
            .build();

        self.add_action_entries([
            toggle_fullscreen_action,
            refresh_action,
            update_db_action,
            quit_action,
            about_action,
            preferences_action,
            player_toggle_playback_action,
            player_next_song_action,
            player_prev_song_action,
            player_seek_forward_action,
            player_seek_backward_action,
            player_stop_action,
            player_toggle_random_action,
            player_cycle_flow_action,
            player_toggle_consume_action,
            player_cycle_replaygain_action,
            player_cycle_crossfade_action,
            player_volume_up_action,
            player_volume_down_action,
            player_toggle_mute_action,
            player_next_output_action,
            player_prev_output_action,
            player_toggle_output_action,
            toggle_visualizer_action,
        ]);
    }

    fn toggle_fullscreen(&self) {
        let window = self.active_window().unwrap();
        self.set_fullscreen(!window.is_fullscreen());
    }

    pub fn is_fullscreen(&self) -> bool {
        if let Some(window) = self.active_window() {
            window.is_fullscreen()
        } else {
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
            let window = EuphonicaWindow::new(self);

            // Window-level keybinds, have to be created again for every new window
            self.set_accels_for_action("win.view-recent", &["<Ctrl>1"]);
            self.set_accels_for_action("win.view-albums", &["<Ctrl>2"]);
            self.set_accels_for_action("win.view-artists", &["<Ctrl>3"]);
            self.set_accels_for_action("win.view-folders", &["<Ctrl>4"]);
            self.set_accels_for_action("win.view-dynamic-playlists", &["<Ctrl>5"]);
            self.set_accels_for_action("win.view-playlists", &["<Ctrl>6"]);
            self.set_accels_for_action("win.view-queue", &["<Ctrl>7"]);
            self.set_accels_for_action("win.search-current-view", &["<Ctrl>f"]);
            self.set_accels_for_action("queue.scroll-to-playing", &["<Ctrl><Shift>o"]); // toggle autoscroll on-off, bad name
            self.set_accels_for_action("queue.stop-and-clear", &["<Alt>c"]);
            self.set_accels_for_action("win.save", &["<Ctrl>s"]);
            self.set_accels_for_action("queue.jump-to-current", &["<Ctrl>o"]);
            self.set_accels_for_action("queue.toggle-autoscroll", &["<Ctrl>u"]);
            self.set_accels_for_action("playlist-editor.undo", &["<Ctrl>z"]);
            self.set_accels_for_action("playlist-editor.redo", &["<Ctrl>y"]);
            self.set_accels_for_action("dyn-playlist-editor.refresh", &["<Ctrl>r"]);
            window.upcast()
        };
        let player = self.imp().player.clone();
        glib::spawn_future_local(async move {
            player.set_is_foreground(true).await;
        });
        window.present();
    }

    fn execute_on_exit_action(&self) {
        let settings = settings_manager().child("state");
        let action = settings.enum_("on-exit-action");

        let player = self.imp().player.clone();
        // Refer to the gschema for enum definition
        match action {
            0 => {
                // The 'do nothing' option
            }
            1 => {
                let _ = glib::MainContext::default().block_on(player.pause());
            }
            2 => {
                let _ = glib::MainContext::default().block_on(player.stop());
            }
            3 => {
                let _ = glib::MainContext::default().block_on(player.clear_queue());
            }
            _ => unimplemented!(),
        }
    }

    pub fn on_window_closed(&self) {
        let settings = settings_manager().child("state");
        if settings.boolean("run-in-background") {
            let player = self.imp().player.clone();
            glib::spawn_future_local(async move {
                player.set_is_foreground(false).await;
            });
            let _ = self.imp().hold_guard.replace(Some(self.hold()));
            // println!("Created a new hold guard");
        } else {
            // println!("Dropping hold guard");
            let _ = self.imp().hold_guard.take();
            self.execute_on_exit_action();
        }
    }

    pub async fn refresh(&self) -> ClientResult<()> {
        // If managing a server, stop and start it again
        if settings_manager()
            .child("client")
            .boolean("mpd-use-own-server")
        {
            if let Err(e) = self.imp().server.stop() {
                dbg!(e);
            }
            if let Err(e) = self.imp().server.start() {
                self.imp().handle_managed_server_error(e);
            }
        }
        self.get_client().connect().await?;
        self.get_library().clear();
        self.get_player().clear()?;
        Ok(())
    }

    fn update_db(&self) {
        let client = self.get_client();
        glib::spawn_future_local(async move {
            if let Err(e) = client.update_db().await {
                dbg!(e);
            }
        });
    }

    pub fn show_about(&self) {
        let window = self.active_window().unwrap();
        let about = adw::AboutDialog::builder()
            .application_name("Euphonica")
            .application_icon("io.github.htkhiem.Euphonica")
            .developer_name("htkhiem2000")
            .version(VERSION)
            .developers(vec!["htkhiem2000", "ShadiestGoat", "sonicv6"])
            .license_type(gtk::License::Gpl30)
            .copyright("© 2026 htkhiem2000")
            .build();

        about.add_credit_section(
            Some("Special Thanks"),
            &["Emmanuele Bassi (GTK, LibAdwaita, the Amberol project) https://www.bassi.io/"],
        );
        about.present(Some(&window));
    }

    pub fn show_preferences(&self) {
        let window = self.active_window().unwrap();
        let prefs = Preferences::new(self, self.get_cache(), self.get_player());
        prefs.present(Some(&window));
        prefs.update();
    }

    /// Quit Euphonica. Useful for when run-in-background is true. Otherwise just close the window.
    pub fn quit_app(&self) {
        self.imp().hold_guard.take();
        if let Some(win) = self.active_window().and_downcast::<EuphonicaWindow>() {
            win.save_state();
        }
        self.quit();
    }
}
