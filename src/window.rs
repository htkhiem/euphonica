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

use std::{cell::RefCell, ops::Deref, path::PathBuf, thread, time::Duration};
use adw::{
    prelude::*,
    subclass::prelude::*
};
use gtk::{
    gdk, gio, glib::{self, clone, closure_local, BoxedAnyObject}
};
use glib::signal::SignalHandlerId;
use image::{imageops::FilterType, DynamicImage};
use libblur::{stack_blur, FastBlurChannels, ThreadingPolicy};
use mpd::Subsystem;
use crate::{
    application::EuphonicaApplication,
    client::{ClientState, ConnectionState},
    common::Album,
    library::{AlbumView, ArtistContentView, ArtistView},
    player::{PlayerBar, QueueView},
    sidebar::Sidebar,
    utils::{self, settings_manager}
};

#[derive(Debug)]
pub struct BlurConfig {
    width: u32,
    height: u32,
    radius: u32,
    fade: bool  // Whether this update requires fading to it. Those for updating radius shouldn't be faded.
}

fn run_blur(di: &DynamicImage, config: &BlurConfig) -> gdk::MemoryTexture {
    let scaled = di.resize_to_fill(
        config.width,
        config.height,
        FilterType::Nearest
    );
    let mut dst_bytes: Vec<u8> = scaled.as_bytes().to_vec();
    // Always assume RGB8 (no alpha channel)
    // This works since we're the ones who wrote the original images
    // to disk in the first place.
    stack_blur(
        &mut dst_bytes,
        config.width * 3,
        config.width,
        config.height,
        config.radius,
        FastBlurChannels::Channels3,
        ThreadingPolicy::Adaptive
    );
    // Wrap in MemoryTexture for snapshotting
    gdk::MemoryTexture::new(
        config.width as i32,
        config.height as i32,
        gdk::MemoryFormat::R8g8b8,
        &glib::Bytes::from_owned(dst_bytes),
        (config.width * 3) as usize
    )
}

pub enum BlurMessage {
    New(PathBuf, BlurConfig), // Load new image at FULL PATH & blur with given configuration. Will fade.
    Update(BlurConfig), // Re-blur current image but do not fade.
    Clear, // Clears last-blurred cache.
    Result(gdk::MemoryTexture, bool), // GPU texture and whether to fade to this one.
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

    use async_channel::Sender;
    use glib::Properties;
    use gtk::gdk;
    use image::io::Reader;
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
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
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
        pub bg_handle: OnceCell<gio::JoinHandle<()>>,
        pub prev_size: Cell<(u32, u32)>
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

            settings.connect_changed(Some("bg-blur-radius"), clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    // Blur radius updates need not fade
                    this.obj().queue_background_update(false);
                }
            ));

            // If using album art as background we must disable the default coloured
            // backgrounds that navigation views use for their sidebars.
            // We do this by toggling the "no-shading" CSS class for the top-level
            // content widget, which in turn toggles the CSS selectors selecting those
            // views.
            obj.connect_notify_local(
                Some("use-album-art-bg"),
                |this, _| {
                    this.queue_new_background();
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
            let _ = self.sender_to_bg.set(sender_to_bg);
            let (sender_to_fg, fg_receiver) = async_channel::bounded::<BlurMessage>(1); // block background thread until sent
            let bg_handle = gio::spawn_blocking(move || {
                println!("Starting background blur thread...");
                let settings = settings_manager().child("player");
                // Cached here to avoid having to load the same image multiple times
                let mut curr_data: Option<DynamicImage> = None;
                let mut curr_path: Option<PathBuf> = None;
                'outer: loop {
                    let curr_path_mut = curr_path.as_mut();
                    // Check if there is work to do (block until there is)
                    let mut last_msg: BlurMessage = bg_receiver
                        .recv_blocking()
                        .expect("Fatal: invalid message sent to window's blur thread");
                    // In case the queue has more than one item, get the last one.
                    while !bg_receiver.is_empty() {
                        last_msg = bg_receiver
                            .recv_blocking()
                            .expect("Fatal: invalid message sent to window's blur thread");
                    }
                    match last_msg {
                        BlurMessage::New(path, config) => {
                            if (curr_path_mut.is_some() && path != *curr_path_mut.unwrap()) || curr_path.is_none() {
                                let di = Reader::open(&path).unwrap().decode().unwrap();
                                curr_path.replace(path);
                                // Guard against calls just after window creation: sizes will be 0, but
                                // we should still record the image data here as the next calls (with sizes)
                                // will only be Updates.
                                if config.width > 0 && config.height > 0 {
                                    let _ = sender_to_fg.send_blocking(BlurMessage::Result(run_blur(&di, &config), true));
                                    thread::sleep(Duration::from_millis(
                                        (settings.double("bg-transition-duration-s") * 1000.0) as u64
                                    ));
                                }
                                curr_data.replace(di);
                            }
                            // Else no need to blur again
                            // (size/radius updates are never sent via this message)
                        }
                        BlurMessage::Update(config) => {
                            if let Some(data) = curr_data.as_ref() {
                                if config.width > 0 && config.height > 0 {
                                    let _ = sender_to_fg.send_blocking(BlurMessage::Result(run_blur(data, &config), config.fade));
                                }
                                if config.fade {
                                    thread::sleep(Duration::from_millis(
                                        (settings.double("bg-transition-duration-s") * 1000.0) as u64
                                    ));
                                }
                            }
                        }
                        BlurMessage::Clear => {
                            curr_data = None;
                            curr_path = None;
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
                            BlurMessage::Result(tex, do_fade) => this.push_tex(Some(tex), do_fade),
                            _ => unreachable!()
                        }

                    }
                }));
        }
    }
    impl WidgetImpl for EuphonicaWindow {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            // Statically-cached blur
            if self.use_album_art_bg.get() {
                // Check if window has been resized (will need reblur)
                let new_size = (widget.width() as u32, widget.height() as u32);
                if new_size != self.prev_size.get() {
                    self.prev_size.replace(new_size);
                    // Size changes are disorienting so we need to fade.
                    widget.queue_background_update(true);
                    // Will still reuse old (mis-sized) blur texture until child thread
                    // comes back with a better one.
                }
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
        pub fn push_tex(&self, tex: Option<gdk::MemoryTexture>, do_fade: bool) {
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
        let client_state = app.get_client().get_client_state();
        let player = app.get_player();
        win.queue_new_background();
        client_state.connect_closure(
            "idle",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                win,
                move |_: ClientState, subsys: BoxedAnyObject| {
                    match subsys.borrow::<Subsystem>().deref() {
                        Subsystem::Database => {
                            this.send_simple_toast("Database updated with changes", 3);
                        }
                        _ => {}
                    }
                }
            )
        );
        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                win,
                move |state: &ClientState, _| {
                    match state.get_connection_state() {
                        ConnectionState::WrongPassword => {
                            this.show_error_dialog(
                                "Incorrect password",
                                "MPD has refused the provided password. Please note that if your MPD instance is not password-protected, providing one will also cause this error.",
                                true
                            );
                        }
                        ConnectionState::Unauthenticated => {
                            this.show_error_dialog(
                                "Authentication Failed",
                                "Your MPD instance requires a password, which was either not provided or lacks the necessary privileges for Euphonica to function correctly.",
                                true
                            );
                        }
                        ConnectionState::CredentialStoreError => {
                            this.show_error_dialog(
                                "Credential Store Error",
                                "Your MPD instance requires a password, but Euphonica could not access your default credential store to retrieve it. Please ensure that it has been unlocked before starting Euphonica.",
                                false
                            );
                        }
                        _ => {}
                    }
                }
            )
        );

        player.connect_notify_local(
            Some("album-art"),
            clone!(
                #[weak(rename_to = this)]
                win,
               move |_, _| {
                    this.queue_new_background();
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

    pub fn send_simple_toast(&self, title: &str, timeout: u32) {
        let toast = adw::Toast::builder()
            .title(title)
            .timeout(timeout)
            .build();
        self.imp().toast_overlay.add_toast(toast);
    }

    fn show_error_dialog(&self, heading: &str, body: &str, suggest_open_preferences: bool) {
        // Show an alert ONLY IF the preferences dialog is not already open.
        if !self.visible_dialog().is_some() {
            let diag = adw::AlertDialog::builder()
                .heading(heading)
                .body(body)
                .build();
            diag.add_response("close", "_Close");
            if suggest_open_preferences {
                diag.add_response("prefs", "Open _Preferences");
                diag.set_response_appearance("prefs", adw::ResponseAppearance::Suggested);
                diag.choose(self, Option::<gio::Cancellable>::None.as_ref(), clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |resp| {
                        if resp == "prefs" {
                            this.downcast_application().show_preferences();
                        }
                    }
                ));
            }
            else {
                diag.present(Some(self));
            }
        }
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

    /// Set blurred background to a new image, if enabled. Use thumbnail version to
    /// minimise disk read time.
    fn queue_new_background(&self) {
        if let Some(player) = self.imp().player.get() {
            if let Some(sender) =  self.imp().sender_to_bg.get() {
                if let Some(path) = player.current_song_album_art_path(true) {
                    if path.exists() {
                        let settings = settings_manager().child("player");
                        let config = BlurConfig {
                            width: self.width() as u32,
                            height: self.height() as u32,
                            radius: settings.uint("bg-blur-radius"),
                            fade: true  // new image, must fade
                        };
                        let _ = sender.send_blocking(BlurMessage::New(path, config));
                    }
                    else {
                        let _ = sender.send_blocking(BlurMessage::Clear);
                        self.imp().push_tex(None, true);
                    }
                }
                else {
                    let _ = sender.send_blocking(BlurMessage::Clear);
                    self.imp().push_tex(None, true);
                }
            }
            else {
                self.imp().push_tex(None, true);
            }
        }
    }

    fn queue_background_update(&self, fade: bool) {
        if let Some(sender) = self.imp().sender_to_bg.get() {
            let settings = settings_manager().child("player");
            let config = BlurConfig {
                width: self.width() as u32,
                height: self.height() as u32,
                radius: settings.uint("bg-blur-radius"),
                fade
            };
            let _ = sender.send_blocking(BlurMessage::Update(config));
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
                    ConnectionState::Unauthenticated |
                    ConnectionState::WrongPassword |
                    ConnectionState::CredentialStoreError => Some("Unauthenticated"),
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
