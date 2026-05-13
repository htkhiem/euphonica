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

use crate::{
    EuphonicaWindow,
    cache::Cache,
    client::{MpdWrapper, Result as ClientResult},
    config::VERSION,
    library::Library,
    player::{Player, QueueView},
    preferences::Preferences,
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

                // Create client instance (not connected yet)
                let client = MpdWrapper::new();

                // Create cache controller
                let cache = Cache::new(client.clone());

                // Create controllers
                // These two are GObjects (already refcounted by GLib)
                let player = Player::default();
                player.setup(self.obj().clone(), client.clone(), cache.clone());
                let library = Library::default();
                library.setup(client.clone(), player.clone());

                let _ = self.cache.set(cache);
                let _ = self.client.set(client);
                let _ = self.library.set(library);
                let _ = self.player.set(player);

                let obj = self.obj();
                obj.setup_gactions();
                obj.set_accels_for_action("app.quit", &["<primary>q"]);
                obj.set_accels_for_action("app.fullscreen", &["F11"]);
                obj.set_accels_for_action("app.refresh", &["F5"]);
                obj.set_accels_for_action("app.view-recent", &["<Ctrl>1"]);
                obj.set_accels_for_action("app.view-albums", &["<Ctrl>2"]);
                obj.set_accels_for_action("app.view-artists", &["<Ctrl>3"]);
                obj.set_accels_for_action("app.view-folders", &["<Ctrl>4"]);
                obj.set_accels_for_action("app.view-dynamic-playlists", &["<Ctrl>5"]);
                obj.set_accels_for_action("app.view-playlists", &["<Ctrl>6"]);
                obj.set_accels_for_action("app.view-queue", &["<Ctrl>7"]);
                obj.set_accels_for_action("queue.scroll-to-playing", &["<Shift>o"]);
                obj.set_accels_for_action("queue.stop-and-clear", &["<Alt>c"]);
                obj.set_accels_for_action("queue.save", &["<Ctrl>s"]);
                obj.set_accels_for_action("queue.jump-to-current", &["<Ctrl>o"]);
                obj.set_accels_for_action("queue.toggle-autoscroll", &["<Ctrl>u"]);

                // Playback shortcuts
                obj.set_accels_for_action("player.toggle-playback", &["<Ctrl>p"]);
                obj.set_accels_for_action("player.next-song", &["<Shift>period"]);
                obj.set_accels_for_action("player.prev-song", &["<Shift>comma"]);
                obj.set_accels_for_action("player.seek-forward", &["<Shift>f"]);
                obj.set_accels_for_action("player.seek-backward", &["<Shift>b"]);
                obj.set_accels_for_action("player.stop", &["<Ctrl><Shift>s"]);
                obj.set_accels_for_action("player.toggle-random", &["<Alt>z"]);
                obj.set_accels_for_action("player.toggle-repeat", &["<Alt>r"]);
                obj.set_accels_for_action("player.toggle-consume", &["<Shift>r"]);
                obj.set_accels_for_action("player.toggle-replaygain", &["<Shift>y"]);
                obj.set_accels_for_action("player.cycle-crossfade", &["<Alt>x"]);
                obj.set_accels_for_action("player.volume-up", &["<Ctrl>up"]);
                obj.set_accels_for_action("player.volume-down", &["<Ctrl>down"]);
                obj.set_accels_for_action("player.toggle-mute", &["<Ctrl>m"]);
                obj.set_accels_for_action("player.next-output", &["<Ctrl>right"]);
                obj.set_accels_for_action("player.prev-output", &["<Ctrl>left"]);

                // Spectrum visualiser
                obj.set_accels_for_action("spectrum-visualizer-toggle", &["F8"]);

                glib::spawn_future_local(clone!(
                    #[weak]
                    application,
                    async move {
                        if let Err(e) = application.refresh().await {
                            dbg!(e);
                        }
                    }
                ));

                self.initialized.set(true);

                // If this is the main instance, respect the minimized flag
                if !self.start_minimized.get() {
                    let player = self.player.get().unwrap();
                    glib::spawn_future_local(clone!(
                        #[weak]
                        player,
                        async move {
                            player.set_is_foreground(true).await;
                        }
                    ));
                    self.obj().raise_window();
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

    pub fn get_player(&self) -> &Player {
        self.imp().player.get().unwrap()
    }

    pub fn get_library(&self) -> &Library {
        self.imp().library.get().unwrap()
    }

    pub fn get_cache(&self) -> Rc<Cache> {
        self.imp().cache.get().unwrap().clone()
    }

    pub fn get_client(&self) -> Rc<MpdWrapper> {
        self.imp().client.get().unwrap().clone()
    }

    fn get_queue_view_if_in_view(&self) -> Option<QueueView> {
        if let Some(win) = self.active_window().and_downcast::<EuphonicaWindow>() {
            let stack = win.get_stack();
            if stack.visible_child_name().as_deref() != Some("queue") {
                None
            } else {
                Some(win.get_queue_view())
            }
        } else {
            None
        }
    }

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
        let view_recent_action = gio::ActionEntry::builder("view-recent")
            .activate(move |this: &Self, _, _| this.switch_to_view("recent"))
            .build();
        let view_albums_action = gio::ActionEntry::builder("view-albums")
            .activate(move |this: &Self, _, _| this.switch_to_view("albums"))
            .build();
        let view_artists_action = gio::ActionEntry::builder("view-artists")
            .activate(move |this: &Self, _, _| this.switch_to_view("artists"))
            .build();
        let view_folders_action = gio::ActionEntry::builder("view-folders")
            .activate(move |this: &Self, _, _| this.switch_to_view("folders"))
            .build();
        let view_dyn_playlists_action = gio::ActionEntry::builder("view-dynamic-playlists")
            .activate(move |this: &Self, _, _| this.switch_to_view("dynamic_playlists"))
            .build();
        let view_playlists_action = gio::ActionEntry::builder("view-playlists")
            .activate(move |this: &Self, _, _| this.switch_to_view("playlists"))
            .build();
        let view_queue_action = gio::ActionEntry::builder("view-queue")
            .activate(move |this: &Self, _, _| this.switch_to_view("queue"))
            .build();

        let queue_scroll_to_playing_action = gio::ActionEntry::builder("queue.scroll-to-playing")
            .activate(move |this: &Self, _, _| {
                if let Some(view) = this.get_queue_view_if_in_view() {
                    view.scroll_to_playing();
                }
            })
            .build();

        let queue_stop_and_clear_action = gio::ActionEntry::builder("queue.stop-and-clear")
            .activate(move |this: &Self, _, _| {
                if let Some(view) = this.get_queue_view_if_in_view() {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        view,
                        async move {
                            view.stop_and_clear().await;
                        }
                    ));
                }
            })
            .build();

        let queue_save_action = gio::ActionEntry::builder("queue.save")
            .activate(move |this: &Self, _, _| {
                if let Some(view) = this.get_queue_view_if_in_view() {
                    view.save();
                }
            })
            .build();

        let queue_jump_to_current_action = gio::ActionEntry::builder("queue.jump-to-current")
            .activate(move |this: &Self, _, _| {
                if let Some(view) = this.get_queue_view_if_in_view() {
                    view.jump_to_current();
                }
            })
            .build();

        let queue_toggle_autoscroll_action = gio::ActionEntry::builder("queue.toggle-autoscroll")
            .activate(move |this: &Self, _, _| {
                if let Some(view) = this.get_queue_view_if_in_view() {
                    view.toggle_autoscroll();
                }
            })
            .build();

        let player_toggle_playback_action = gio::ActionEntry::builder("player.toggle-playback")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.toggle_playback().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_next_song_action = gio::ActionEntry::builder("player.next-song")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.next_song().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_prev_song_action = gio::ActionEntry::builder("player.prev-song")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.prev_song().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_seek_forward_action = gio::ActionEntry::builder("player.seek-forward")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let pos = player.position() + 10.0;
                        if let Err(e) = player.send_seek(pos).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_seek_backward_action = gio::ActionEntry::builder("player.seek-backward")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let pos = player.position() - 10.0;
                        if pos < 0.0 {
                            let _ = player.send_seek(0.0).await;
                        } else if let Err(e) = player.send_seek(pos).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_stop_action = gio::ActionEntry::builder("player.stop")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.stop().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_toggle_random_action = gio::ActionEntry::builder("player.toggle-random")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let new_state = !player.random();
                        if let Err(e) = player.set_random(new_state).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_toggle_repeat_action = gio::ActionEntry::builder("player.toggle-repeat")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.cycle_playback_flow().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_toggle_consume_action = gio::ActionEntry::builder("player.toggle-consume")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let new_state = !player.consume();
                        if let Err(e) = player.set_consume(new_state).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_toggle_replaygain_action = gio::ActionEntry::builder("player.toggle-replaygain")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.cycle_replaygain().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_cycle_crossfade_action = gio::ActionEntry::builder("player.cycle-crossfade")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let current = player.crossfade();
                        let next = match current {
                            0.0 => 1.0,
                            1.0 => 3.0,
                            3.0 => 5.0,
                            5.0 => 10.0,
                            10.0 => 0.0,
                            v => v,
                        };
                        if let Err(e) = player.set_crossfade(next).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_volume_up_action = gio::ActionEntry::builder("player.volume-up")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let vol = player.mpd_volume().saturating_add(2);
                        if let Err(e) = player.send_set_volume(vol).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_volume_down_action = gio::ActionEntry::builder("player.volume-down")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        let vol = player.mpd_volume().saturating_sub(2);
                        if let Err(e) = player.send_set_volume(vol).await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_toggle_mute_action = gio::ActionEntry::builder("player.toggle-mute")
            .activate(move |this: &Self, _, _| {
                let player = this.get_player().clone();
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    async move {
                        if let Err(e) = player.toggle_mute().await {
                            dbg!(e);
                        }
                    }
                ));
            })
            .build();

        let player_cycle_output_action = gio::ActionEntry::builder("player.next-output")
            .activate(move |this: &Self, _, _| {
                this.get_player().cycle_output(false);
            })
            .build();

        let player_cycle_output_prev_action = gio::ActionEntry::builder("player.prev-output")
            .activate(move |this: &Self, _, _| {
                this.get_player().cycle_output(true);
            })
            .build();

        let spectrum_visualizer_toggle_action =
            gio::ActionEntry::builder("spectrum-visualizer-toggle")
                .activate(move |this: &Self, _, _| {
                    let Some(window) = this.active_window() else {
                        return;
                    };
                    let Some(euphonica_window) = window.downcast_ref::<EuphonicaWindow>() else {
                        return;
                    };
                    let imp = euphonica_window.imp();
                    imp.use_visualizer.set(!imp.use_visualizer.get());
                })
                .build();

        self.add_action_entries([
            toggle_fullscreen_action,
            refresh_action,
            update_db_action,
            quit_action,
            about_action,
            preferences_action,
            view_recent_action,
            view_albums_action,
            view_artists_action,
            view_folders_action,
            view_dyn_playlists_action,
            view_playlists_action,
            view_queue_action,
            queue_scroll_to_playing_action,
            queue_stop_and_clear_action,
            queue_save_action,
            queue_jump_to_current_action,
            queue_toggle_autoscroll_action,
            player_toggle_playback_action,
            player_next_song_action,
            player_prev_song_action,
            player_seek_forward_action,
            player_seek_backward_action,
            player_stop_action,
            player_toggle_random_action,
            player_toggle_repeat_action,
            player_toggle_consume_action,
            player_toggle_replaygain_action,
            player_cycle_crossfade_action,
            player_volume_up_action,
            player_volume_down_action,
            player_toggle_mute_action,
            player_cycle_output_action,
            player_cycle_output_prev_action,
            spectrum_visualizer_toggle_action,
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
            window.upcast()
        };
        let player = self.imp().player.get().unwrap().clone();
        glib::spawn_future_local(async move {
            player.set_is_foreground(true).await;
        });
        let player = self.imp().player.get().unwrap().clone();
        glib::spawn_future_local(async move {
            player.set_is_foreground(true).await;
        });
        window.present();
    }

    fn execute_on_exit_action(&self) {
        let settings = settings_manager().child("state");
        let action = settings.enum_("on-exit-action");

        let player = self.imp().player.get().unwrap().clone();
        // Refer to the gschema for enum definition
        match action {
            0 => {
                // The 'do nothing' option
            }
            1 => {
                glib::MainContext::default().block_on(player.pause());
            }
            2 => {
                glib::MainContext::default().block_on(player.stop());
            }
            3 => {
                glib::MainContext::default().block_on(player.clear_queue());
            }
            _ => unimplemented!(),
        }
    }

    pub fn on_window_closed(&self) {
        let settings = settings_manager().child("state");
        if settings.boolean("run-in-background") {
            let player = self.imp().player.get().unwrap().clone();
            glib::spawn_future_local(async move {
                player.set_is_foreground(false).await;
            });
            let _ = self.imp().hold_guard.replace(Some(self.hold()));
            println!("Created a new hold guard");
        } else {
            println!("Dropping hold guard");
            let _ = self.imp().hold_guard.take();
            self.execute_on_exit_action();
        }
    }

    pub async fn refresh(&self) -> ClientResult<()> {
        self.get_client().connect().await?;
        self.get_library().clear();
        self.get_player().clear();
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
        self.quit();
    }

    pub fn switch_to_view(&self, view_name: &str) {
        if let Some(window) = self.active_window() {
            if let Some(euphonica_window) = window.downcast_ref::<EuphonicaWindow>() {
                euphonica_window.imp().sidebar.get().set_view(view_name);
            }
        }
    }
}
