/* window.rs
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

use std::{cell::RefCell, thread, time::Duration};
use adw::{
    prelude::*,
    subclass::prelude::*
};
use gtk::{
    gdk, gio, glib::{self, clone, closure_local}
};
use glib::signal::SignalHandlerId;
use image::DynamicImage;
use crate::{
    application::EuphonicaApplication, client::ConnectionState, common::Album, library::{AlbumView, ArtistContentView, ArtistView}, player::{PlayerBar, QueueView}, sidebar::Sidebar, utils::{self, settings_manager}
};

enum BlurMessage {
    Blur(String, bool), // URI and whether to fade to this image
    BlurResult(gdk::MemoryTexture, bool), // GPU texture and whether to fade to this one
    Stop
}

// Blurred background logic. Runs in a background thread. Both interpretations are valid :)
// Our asynchronous background switching algorithm is pretty simple: Player controller
// sends paths of album arts (just strings) to this thread. It then loads the image from
// disk as a DynamicImage (CPU-side, not GdkTextures, which are quickly dumped into VRAM),
// blurs it using libblur, uploads to GPU and fades background to it.
// In case more paths arrive as we are in the middle of processing one for fading, the loop
// will come back to the async channel with many messages in it. In this case, pop and drop
// all except the last, which we will process normally. This means quickly skipping songs
// will not result in a rapidly-changing background - it will only change as quickly as it
// can fade or the CPU can blur, whichever is slower.
mod imp {
    use std::cell::{Cell, OnceCell};

    use async_channel::{Receiver, Sender};
    use glib::Properties;
    use gtk::gdk;
    use utils::settings_manager;

    use crate::{common::paintables::FadePaintable, library::FolderView, player::Player};

    use super::*;

    #[derive(Debug, Default, Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::EuphonicaWindow)]
    #[template(resource = "/org/euphonica/Euphonica/window.ui")]
    pub struct EuphonicaWindow {
        // Top level widgets
        #[template_child]
        pub split_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub content: TemplateChild<gtk::Box>,
        // Main views
        #[template_child]
        pub album_view: TemplateChild<AlbumView>,
        #[template_child]
        pub artist_view: TemplateChild<ArtistView>,
        #[template_child]
        pub folder_view: TemplateChild<FolderView>,
        #[template_child]
        pub queue_view: TemplateChild<QueueView>,

        // Content view stack
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,

        // Sidebar
        // TODO: Replace with Libadwaita spinner when v1.6 hits stable
        #[template_child]
        pub busy_spinner: TemplateChild<gtk::Spinner>,
        #[template_child]
        pub title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub sidebar: TemplateChild<Sidebar>,

        // Bottom bar
        #[template_child]
        pub player_bar_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub player_bar: TemplateChild<PlayerBar>,

        // RefCells to notify IDs so we can unbind later
        pub notify_position_id: RefCell<Option<SignalHandlerId>>,
        pub notify_playback_state_id: RefCell<Option<SignalHandlerId>>,
        pub notify_duration_id: RefCell<Option<SignalHandlerId>>,

        #[property(get, set)]
        pub use_album_art_bg: Cell<bool>,
        #[property(get, set)]
        pub opacity: Cell<f64>,
        pub bg_paintable: FadePaintable,
        pub player: OnceCell<Player>,
        pub sender_to_bg: OnceCell<Sender<BlurMessage>>, // sending a None will terminate the thread
        pub bg_handle: OnceCell<gio::JoinHandle<()>>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for EuphonicaWindow {
        const NAME: &'static str = "EuphonicaWindow";
        type Type = super::EuphonicaWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            // klass.set_layout_manager_type::<gtk::BoxLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for EuphonicaWindow {
        fn constructed(&self) {
            self.parent_constructed();
            let settings = settings_manager().child("player");
            let obj_borrow = self.obj();
            let obj = obj_borrow.as_ref();
            let bg_paintable = &self.bg_paintable;

            settings
                .bind(
                    "use-album-art-as-bg",
                    obj,
                    "use-album-art-bg"
                )
                .build();

            settings
                .bind(
                    "bg-blur-radius",
                    bg_paintable.current(),
                    "blur-radius"
                )
                .build();

            settings
                .bind(
                    "bg-blur-radius",
                    bg_paintable.previous(),
                    "blur-radius"
                )
                .build();

            settings
                .bind(
                    "bg-opacity",
                    obj,
                    "opacity"
                )
                .build();

            settings
                .bind(
                    "bg-transition-duration-s",
                    bg_paintable,
                    "transition-duration"
                )
                .build();

            // If using album art as background we must disable the default coloured
            // backgrounds that navigation views use for their sidebars.
            // We do this by toggling the "no-shading" CSS class for the top-level
            // content widget, which in turn toggles the CSS selectors selecting those
            // views.
            obj.connect_notify_local(
                Some("use-album-art-bg"),
                |this, _| {
                    this.update_background();
                }
            );

            self.sidebar.connect_notify_local(
                Some("showing-queue-view"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.update_player_bar_visibility();
                    }
                )
            );

            self.queue_view.connect_notify_local(
                Some("show-content"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.update_player_bar_visibility();
                    }
                )
            );

            self.queue_view.connect_notify_local(
                Some("collapsed"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.update_player_bar_visibility();
                    }
                )
            );

            // Set up blur thread
            let (sender_to_bg, bg_receiver) = async_channel::unbounded::<BlurMessage>();
            self.sender_to_bg.set(sender_to_bg);
            let (sender_to_fg, fg_receiver) = async_channel::bounded::<BlurMessage>(1); // block background thread until sent
            let bg_handle = gio::spawn_blocking(move || {
                println!("Starting background blur thread...");
                let settings = settings_manager().child("player");
                'outer: loop {
                    // Check if there is work to do (block until there is)
                    let mut last_msg: BlurMessage = bg_receiver
                        .recv_blocking()
                        .expect("Fatal: invalid message sent to window's blur thread");
                    // In case the queue has more than one item, get the last one.
                    while !bg_receiver.is_empty() {
                        println!("Dropping extraneous blur requests...");
                        last_msg = bg_receiver
                            .recv_blocking()
                            .expect("Fatal: invalid message sent to window's blur thread");
                    }
                    match last_msg {
                        BlurMessage::Blur(uri, do_fade) => {
                            // TODO
                            // Wait until the ensuing fade operation has completed before
                            // blurring the next art.
                            thread::sleep(Duration::from_millis(
                                (settings.double("bg-transition-duration-s") * 1000.0) as u64
                            ));
                        }
                        BlurMessage::Stop => {
                            break 'outer;
                        }
                        _ => unreachable!()  // we shouldn't ever send BlurResult to the child thread
                    }
                }
            });
            let _ = self.bg_handle.set(bg_handle);

            // Use an async loop to wait for messages from the blur thread.
            // The blur thread will send us handles to GPU textures. Upon receiving one,
            // fade to it.
            glib::MainContext::default().spawn_local(clone!(
                #[weak(rename_to = this)]
                self,
                async move {
                    use futures::prelude::*;
                    // Allow receiver to be mutated, but keep it at the same memory address.
                    // See Receiver::next doc for why this is needed.
                    let mut receiver = std::pin::pin!(fg_receiver);
                    while let Some(blur_msg) = receiver.next().await {
                        match blur_msg {
                            BlurMessage::BlurResult(tex, do_fade) => this.push_tex(tex, do_fade),
                            _ => unreachable!()
                        }

                    }
                }));
        }
    }
    impl WidgetImpl for EuphonicaWindow {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            // GPU-accelerated, statically-cached blur
            if self.use_album_art_bg.get() {
                let opacity = self.opacity.get();
                if opacity < 1.0 {
                    snapshot.push_opacity(opacity);
                }
                self.bg_paintable.snapshot(
                    snapshot,
                    widget.width() as f64,
                    widget.height() as f64
                );
                if opacity < 1.0 {
                    snapshot.pop();
                }
            }
            // Call the parent class's snapshot() method to render child widgets
            self.parent_snapshot(snapshot);
        }
    }
    impl WindowImpl for EuphonicaWindow {}
    impl ApplicationWindowImpl for EuphonicaWindow {}
    impl AdwApplicationWindowImpl for EuphonicaWindow {}

    impl EuphonicaWindow {
        /// Fade to the new texture, or to nothing if playing song has no album art.
        fn push_tex(&self, tex: Option<gdk::MemoryTexture>, do_fade: bool) {
            let bg_paintable = self.bg_paintable.clone();
            if self.use_album_art_bg.get() && tex.is_some() {
                if !self.content.has_css_class("no-shading") {
                    self.content.add_css_class("no-shading");
                }
            }
            else {
                if self.content.has_css_class("no-shading") {
                    self.content.remove_css_class("no-shading");
                }
            }
            // Will immediately re-blur and upload to GPU at current size
            bg_paintable.set_new_content(tex);
            if do_fade {
                // Once we've finished the above (expensive) operations, we can safely start
                // the fade animation without worrying about stuttering.
                glib::idle_add_local_once(
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move || {
                            // Run fade transition once main thread is free
                            // Remember to queue draw too
                            let duration = (bg_paintable.transition_duration() * 1000.0).round() as u32;
                            let anim_target = adw::CallbackAnimationTarget::new(
                                clone!(
                                    #[weak]
                                    this,
                                    move |progress: f64| {
                                        bg_paintable.set_fade(progress);
                                        this.obj().queue_draw();
                                    }
                                )
                            );
                            let anim = adw::TimedAnimation::new(
                                this.obj().as_ref(),
                                0.0, 1.0,
                                duration,
                                anim_target
                            );
                            anim.play();
                        }
                    )
                );
            }
            else {
                // Just immediately show the new texture. Used for blur radius adjustments.
                bg_paintable.set_fade(1.0);
            }
        }
    }
}

glib::wrapper! {
    pub struct EuphonicaWindow(ObjectSubclass<imp::EuphonicaWindow>)
        @extends gtk::Widget, gtk::Window, gtk::ApplicationWindow,
    adw::ApplicationWindow,
    @implements gio::ActionGroup, gio::ActionMap;
}

impl EuphonicaWindow {
    pub fn new<P: glib::object::IsA<gtk::Application>>(application: &P) -> Self {
        let win: Self =  glib::Object::builder()
            .property("application", application)
            .build();

        let app = win.downcast_application();
        let player = app.get_player();
        win.update_background();
        player.connect_notify_local(
            Some("album-art"),
            clone!(
                #[weak(rename_to = this)]
                win,
               move |_, _| {
                    this.update_background();
                }
            )
        );
        let _ = win.imp().player.set(player);

        win.restore_window_state();
        win.imp().queue_view.setup(
            app.get_player(),
            app.get_cache()
        );
        win.imp().album_view.setup(
            app.get_library(),
            app.get_cache(),
            app.get_client().get_client_state()
        );
        win.imp().artist_view.setup(
            app.get_library(),
            app.get_cache(),
            app.get_client().get_client_state()
        );
        win.imp().folder_view.setup(
            app.get_library(),
            app.get_cache(),
            app.get_client().get_client_state()
        );
        win.imp().sidebar.setup(
            win.imp().stack.get(),
            win.imp().split_view.get(),
            app.get_player()
        );
        win.imp().player_bar.setup(
            app.get_player()
        );

        win.imp().player_bar.connect_closure(
            "goto-pane-clicked",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                win,
                move |_: PlayerBar| {
                    this.goto_pane();
                }
            )
        );

        win.imp().artist_view.get_content_view().connect_closure(
            "album-clicked",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                win,
                move |_: ArtistContentView, album: Album| {
                    this.goto_album(&album);
                }
            )
        );

        win.bind_state();
        win.setup_signals();
        win
    }

    fn update_player_bar_visibility(&self) {
        let revealer = self.imp().player_bar_revealer.get();
        if self.imp().sidebar.showing_queue_view() {
            let queue_view = self.imp().queue_view.get();
            if (queue_view.collapsed() && queue_view.show_content()) || !queue_view.collapsed() {
                revealer.set_reveal_child(false);
            }
            else {
                revealer.set_reveal_child(true);
            }
        }
        else {
            revealer.set_reveal_child(true);
        }
    }

    fn goto_pane(&self) {
        self.imp().stack.set_visible_child_name("queue");
        self.imp().split_view.set_show_content(true);
        self.imp().queue_view.set_show_content(true);
    }

    pub fn goto_album(&self, album: &Album) {
        self.imp().album_view.on_album_clicked(album);
        // self.imp().stack.set_visible_child_name("albums");
        self.imp().sidebar.set_view("albums");
        if !self.imp().split_view.shows_content() {
            self.imp().split_view.set_show_content(true);
        }
    }

    /// Update blurred background, if enabled. Use thumbnail version to minimise disk read time
    /// since we're doing this synchronously.
    fn update_background(&self) {
        if let Some(player) = self.imp().player.get() {
            let imp = self.imp();
            let tex: Option<DynamicImage> = player.current_song_album_art_cpu(true);
            let bg_paintable = imp.bg_paintable.clone();
            if imp.use_album_art_bg.get() && tex.is_some() {
                if !imp.content.has_css_class("no-shading") {
                    imp.content.add_css_class("no-shading");
                }
            }
            else {
                if imp.content.has_css_class("no-shading") {
                    imp.content.remove_css_class("no-shading");
                }
            }
            // Will immediately re-blur and upload to GPU at current size
            bg_paintable.set_new_content(tex);
            // Once we've finished the above (expensive) operations, we can safely start
            // the fade animation without worrying about stuttering.
            glib::idle_add_local_once(
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move || {
                        // Run fade transition once main thread is free
                        // Remember to queue draw too
                        let duration = (bg_paintable.transition_duration() * 1000.0).round() as u32;
                        let anim_target = adw::CallbackAnimationTarget::new(
                            clone!(
                                #[weak]
                                this,
                                move |progress: f64| {
                                    bg_paintable.set_fade(progress);
                                    this.queue_draw();
                                }
                            )
                        );
                        let anim = adw::TimedAnimation::new(
                            &this,
                            0.0, 1.0,
                            duration,
                            anim_target
                        );
                        anim.play();
                    }
                )
            );
        }
    }

    fn restore_window_state(&self) {
        let settings = utils::settings_manager();
        let state = settings.child("state");
        let width = state.int("last-window-width");
        let height = state.int("last-window-height");
        self.set_default_size(width, height);
    }

    fn downcast_application(&self) -> EuphonicaApplication {
        self.application()
            .unwrap()
            .downcast::<crate::application::EuphonicaApplication>()
            .unwrap()
    }

    fn bind_state(&self) {
        // Bind client state to app name widget
        let client = self.downcast_application().get_client();
        let state = client.get_client_state();
        let title = self.imp().title.get();
        let spinner = self.imp().busy_spinner.get();
        state
            .bind_property(
                "connection-state",
                &title,
                "subtitle"
            )
            .transform_to(|_, state: ConnectionState| {
                match state {
                    ConnectionState::NotConnected => Some("Not connected"),
                    ConnectionState::Connecting => Some("Connecting"),
                    ConnectionState::Unauthenticated => Some("Unauthenticated"),
                    ConnectionState::Connected => Some("Connected")
                }
            })
            .sync_create()
            .build();

        state
            .bind_property(
                "busy",
                &spinner,
                "visible"
            )
            .sync_create()
            .build();
    }

    fn setup_signals(&self) {
        self.connect_close_request(move |window| {
            let size = window.default_size();
            let width = size.0;
            let height = size.1;
            let settings = utils::settings_manager();
            let state = settings.child("state");
            state
                .set_int("last-window-width", width)
                .expect("Unable to store last-window-width");
            state
                .set_int("last-window-height", height)
                .expect("Unable to stop last-window-height");

	        // TODO: persist other settings at closing?
            glib::Propagation::Proceed
        });
	}
}
