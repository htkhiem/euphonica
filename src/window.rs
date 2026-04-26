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

use crate::{
    application::EuphonicaApplication,
    client::{ClientState, ConnectionState, Result as ClientResult},
    common::{Album, Artist, INode, ThemeSelector, paintables::FadePaintable},
    library::{
        AlbumView, ArtistContentView, ArtistView, DynamicPlaylistView, FolderView, PlaylistView,
        RecentView,
    },
    player::{Player, PlayerBar, QueueView},
    sidebar::Sidebar,
    utils::{self, LazyInit, settings_manager},
};
use adw::{ColorScheme, StyleManager, prelude::*, subclass::prelude::*};
use auto_palette::{ImageData, Palette, color::RGB};
use glib::WeakRef;
use gtk::{
    CssProvider, cairo, gdk, gio,
    glib::{self, BoxedAnyObject, SignalHandlerId, clone, closure_local},
    graphene, gsk,
};
use image::{DynamicImage, imageops::FilterType};
use libblur::{FastBlurChannels, ThreadingPolicy, stack_blur};
use mpd::Subsystem;
use std::{cell::RefCell, ops::Deref, path::PathBuf, thread, time::Duration};
use std::{
    cell::{Cell, OnceCell},
    sync::{Arc, Mutex},
};

use async_channel::Sender;
use glib::Properties;
use image::ImageReader as Reader;

// How many dominant colours to extract out of the palette for accent colour selection.
static PALETTE_SIZE: usize = 5;

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
#[derive(Debug)]
pub struct BlurConfig {
    width: u32,
    height: u32,
    radius: u32,
    is_dark: bool,
    fade: bool, // Whether this update requires fading to it. Those for updating radius shouldn't be faded.
}

#[inline]
fn compute_visualizer_y(val: f32, surface_height: f32, scale_factor: f32) -> f32 {
    (surface_height - val * scale_factor * 1_000_000.0).max(0.0)
}

fn run_blur(di: &DynamicImage, config: &BlurConfig) -> gdk::MemoryTexture {
    let scaled = di.resize_to_fill(config.width, config.height, FilterType::Nearest);
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
        ThreadingPolicy::Adaptive,
    );
    // Wrap in MemoryTexture for snapshotting
    gdk::MemoryTexture::new(
        config.width as i32,
        config.height as i32,
        gdk::MemoryFormat::R8g8b8,
        &glib::Bytes::from_owned(dst_bytes),
        (config.width * 3) as usize,
    )
}

fn get_dominant_color(img: &DynamicImage, is_dark: bool) -> RGB {
    let colors = img
        .as_rgb8()
        .unwrap()
        .pixels()
        .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
        .collect::<Vec<u8>>();

    let palette =
        Palette::<f32>::extract(&ImageData::new(img.width(), img.height(), &colors).unwrap())
            .unwrap()
            .find_swatches(PALETTE_SIZE)
            .iter()
            .map(|c| c.color().clone())
            .collect::<Vec<auto_palette::color::Color<f32>>>();

    // First, try to find a color that contrasts with the current UI mode (light color in dark mode, dark color in light mode)
    let mut suboptimal_luminance = false;
    let mut dominant = palette.iter().find(|c| c.is_dark() != is_dark).cloned();

    // If no matching color was found, fallback to the first color with saturation > 0.3
    if dominant.is_none() {
        suboptimal_luminance = true;
        if let Some(saturated) = palette.iter().find(|c| c.to_hsl().s > 0.3) {
            dominant = Some(saturated.clone());
        } else {
            // If still no suitable color, fall back to the first color in the palette
            dominant = palette.first().cloned();
        }
    }

    let mut dominant = dominant.unwrap();

    // Convert to HSL for luminance adjustment
    if suboptimal_luminance {
        let mut hsl = dominant.to_hsl();

        // Adjust luminance based on theme mode
        if is_dark {
            hsl.l = hsl.l.max(0.6).min(0.85);
        } else {
            hsl.l = hsl.l.min(0.35).max(0.19);
        }

        RGB::from(&hsl)
    } else {
        dominant.to_rgb()
    }
}

pub enum WindowMessage {
    NewBackground(PathBuf, BlurConfig), // Load new image at FULL PATH & blur with given configuration. Will fade.
    UpdateBackground(BlurConfig),       // Re-blur current image but do not fade.
    UpdateAccent(bool),                 // is_dark
    ClearBackground,                    // Clears last-blurred cache.
    Result(gdk::MemoryTexture, Option<RGB>, bool), // GPU texture and whether to fade to this one.
    AccentResult(RGB),
    Stop,
}

mod imp {
    use super::*;

    #[derive(Debug, Default, Properties, gtk::CompositeTemplate)]
    #[properties(wrapper_type = super::EuphonicaWindow)]
    #[template(resource = "/io/github/htkhiem/Euphonica/window.ui")]
    pub struct EuphonicaWindow {
        // Top level widgets
        #[template_child]
        pub split_view: TemplateChild<adw::OverlaySplitView>,
        #[template_child]
        pub content: TemplateChild<gtk::Box>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        // Main views
        #[template_child]
        pub recent_view: TemplateChild<RecentView>,
        #[template_child]
        pub album_view: TemplateChild<AlbumView>,
        #[template_child]
        pub artist_view: TemplateChild<ArtistView>,
        #[template_child]
        pub folder_view: TemplateChild<FolderView>,
        #[template_child]
        pub dyn_playlist_view: TemplateChild<DynamicPlaylistView>,
        #[template_child]
        pub playlist_view: TemplateChild<PlaylistView>,
        #[template_child]
        pub queue_view: TemplateChild<QueueView>,

        #[template_child]
        pub menu_btn: TemplateChild<gtk::MenuButton>,

        // Content view stack
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,
        // Sidebar
        #[template_child]
        pub pending_tasks_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub pending_fg_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub fg_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub fg_task_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub pending_bg_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub bg_progress: TemplateChild<gtk::ProgressBar>,
        #[template_child]
        pub bg_task_count: TemplateChild<gtk::Label>,

        #[template_child]
        pub title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub sidebar: TemplateChild<Sidebar>,

        // Bottom bar
        #[template_child]
        pub player_bar_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub player_bar: TemplateChild<PlayerBar>,

        // Blurred album art background
        #[property(get, set)]
        pub use_album_art_bg: Cell<bool>,
        #[property(get, set)]
        pub bg_opacity: Cell<f64>,
        pub bg_paintable: FadePaintable,
        pub player: WeakRef<Player>,
        pub sender_to_bg: OnceCell<Sender<WindowMessage>>, // sending a None will terminate the thread
        pub bg_handle: RefCell<Option<gio::JoinHandle<()>>>,
        pub prev_size: Cell<(u32, u32)>,

        // Visualiser on the bottom edge
        #[property(get, set)]
        pub use_visualizer: Cell<bool>,
        #[property(get, set)]
        pub visualizer_top_opacity: Cell<f64>,
        #[property(get, set)]
        pub visualizer_bottom_opacity: Cell<f64>,
        #[property(get, set)]
        pub visualizer_scale: Cell<f64>,
        #[property(get, set)]
        pub visualizer_use_splines: Cell<bool>,
        #[property(get, set)]
        pub visualizer_stroke_width: Cell<f64>,
        #[property(get, set)]
        pub visualizer_use_cairo: Cell<bool>,
        #[property(get, set = Self::set_auto_accent)]
        pub auto_accent: Cell<bool>,
        pub tick_callback: RefCell<Option<gtk::TickCallbackId>>,
        pub fft_data: OnceCell<Arc<Mutex<(Vec<f32>, Vec<f32>)>>>,
        pub accent_color: RefCell<Option<RGB>>,
        pub should_populate_visible: Cell<bool>,

        pub provider: CssProvider,
        pub client_state: OnceCell<ClientState>,

        // FPS tracking (debug)
        pub fps_frame_count: Cell<u64>,
        pub fps_last_time: Cell<Option<std::time::Instant>>,
        pub fps_tick_id: RefCell<Option<gtk::TickCallbackId>>,

        // Signal handler IDs for disconnect on dispose
        pub settings_bg_blur_id: RefCell<Option<SignalHandlerId>>,
        pub settings_visualizer_id: RefCell<Option<SignalHandlerId>>,
        pub client_state_idle_id: RefCell<Option<SignalHandlerId>>,
        pub client_state_conn_state_id: RefCell<Option<SignalHandlerId>>,
        pub player_cover_changed_id: RefCell<Option<SignalHandlerId>>,
        pub client_state_pct_fg_id: RefCell<Option<SignalHandlerId>>,
        pub client_state_pct_bg_id: RefCell<Option<SignalHandlerId>>,
        pub client_state_n_fg_id: RefCell<Option<SignalHandlerId>>,
        pub client_state_n_bg_id: RefCell<Option<SignalHandlerId>>,
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
        fn dispose(&self) {
            // Disconnect all signal handlers registered on global/long-lived objects
            if let Some(id) = self.settings_bg_blur_id.take() {
                let settings = settings_manager().child("ui");
                settings.disconnect(id);
            }
            if let Some(id) = self.settings_visualizer_id.take() {
                let settings = settings_manager().child("ui");
                settings.disconnect(id);
            }
            if let Some(client_state) = self.client_state.get() {
                if let Some(id) = self.client_state_idle_id.take() {
                    client_state.disconnect(id);
                }
                if let Some(id) = self.client_state_conn_state_id.take() {
                    client_state.disconnect(id);
                }
                if let Some(id) = self.client_state_pct_fg_id.take() {
                    client_state.disconnect(id);
                }
                if let Some(id) = self.client_state_pct_bg_id.take() {
                    client_state.disconnect(id);
                }
                if let Some(id) = self.client_state_n_fg_id.take() {
                    client_state.disconnect(id);
                }
                if let Some(id) = self.client_state_n_bg_id.take() {
                    client_state.disconnect(id);
                }
            }
            if let Some(id) = self.player_cover_changed_id.take() {
                if let Some(player) = self.player.upgrade() {
                    player.disconnect(id);
                }
            }

            // Remove display-level CSS provider
            if let Some(display) = gdk::Display::default() {
                let provider = &self.provider;
                gtk::style_context_remove_provider_for_display(&display, provider);
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let settings = settings_manager().child("ui");
            let obj_borrow = self.obj();
            let obj = obj_borrow.as_ref();
            let bg_paintable = &self.bg_paintable;

            // Set theme from setting first, then connect listener later (else we'll have a feedback loop).
            let style = adw::StyleManager::default();
            style.set_color_scheme(match settings.string("colorscheme").as_str() {
                "dark" => ColorScheme::ForceDark,
                "prefer-dark" => ColorScheme::PreferDark,
                "prefer-light" => ColorScheme::PreferLight,
                "light" => ColorScheme::ForceLight,
                _ => ColorScheme::Default,
            });

            // Add theme selector to popover menu
            let primary_menu = self
                .menu_btn
                .popover()
                .and_downcast::<gtk::PopoverMenu>()
                .unwrap();
            let theme_selector = ThemeSelector::new();
            primary_menu.add_child(&theme_selector, "theme_selector");

            theme_selector.connect_closure(
                "changed",
                false,
                closure_local!(
                    #[watch(rename_to = this)]
                    obj,
                    move |_: ThemeSelector, scheme: ColorScheme| {
                        let style = StyleManager::default();
                        println!("Setting theme to {:?}", &scheme);
                        style.set_color_scheme(scheme);

                        // Trigger a background update which will update accent colour too
                        if let Some(sender) = this.imp().sender_to_bg.get() {
                            let _ = sender.send_blocking(WindowMessage::UpdateAccent(
                                adw::StyleManager::default().is_dark(),
                            ));
                        }

                        // Save setting
                        let settings = settings_manager().child("ui");
                        let _ = settings.set_string(
                            "colorscheme",
                            match scheme {
                                ColorScheme::ForceDark => "dark",
                                ColorScheme::PreferDark => "prefer-dark",
                                ColorScheme::PreferLight => "prefer-light",
                                ColorScheme::ForceLight => "light",
                                _ => "follow",
                            },
                        );
                    }
                ),
            );

            settings
                .bind("use-album-art-as-bg", obj, "use-album-art-bg")
                .build();

            settings.bind("bg-opacity", obj, "bg-opacity").build();

            settings
                .bind(
                    "bg-transition-duration-s",
                    bg_paintable,
                    "transition-duration",
                )
                .build();

            self.settings_bg_blur_id
                .replace(Some(settings.connect_changed(
                    Some("bg-blur-radius"),
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move |_, _| {
                            // Blur radius updates need not fade
                            this.obj().queue_background_update(false);
                        }
                    ),
                )));
            self.settings_visualizer_id
                .replace(Some(settings.connect_changed(
                    Some("use-visualizer"),
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move |settings, key| {
                            this.set_always_redraw(settings.boolean(key));
                        }
                    ),
                )));

            // If using album art as background we must disable the default coloured
            // backgrounds that navigation views use for their sidebars.
            // We do this by toggling the "no-shading" CSS class for the top-level
            // content widget, which in turn toggles the CSS selectors selecting those
            // views.
            obj.connect_notify_local(Some("use-album-art-bg"), |this, _| {
                this.queue_new_background();
            });

            settings
                .bind("use-visualizer", obj, "use-visualizer")
                .build();

            settings
                .bind("visualizer-top-opacity", obj, "visualizer-top-opacity")
                .build();

            settings
                .bind(
                    "visualizer-bottom-opacity",
                    obj,
                    "visualizer-bottom-opacity",
                )
                .build();

            settings
                .bind("visualizer-scale", obj, "visualizer-scale")
                .build();

            settings
                .bind("visualizer-use-cairo", obj, "visualizer-use-cairo")
                .get_only()
                .build();

            settings
                .bind("visualizer-use-splines", obj, "visualizer-use-splines")
                .get_only()
                .build();

            settings
                .bind("visualizer-stroke-width", obj, "visualizer-stroke-width")
                .get_only()
                .build();

            settings
                .bind("auto-accent", obj, "auto-accent")
                .get_only()
                .build();

            self.set_always_redraw(self.use_visualizer.get());

            self.sidebar.connect_notify_local(
                Some("showing-queue-view"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.update_player_bar_visibility();
                    }
                ),
            );

            let view = self.split_view.get();
            // Take care not to cause strong references here or we won't be able to
            // dispose window content properly.
            [
                self.recent_view.upcast_ref::<gtk::Widget>(),
                self.album_view.upcast_ref::<gtk::Widget>(),
                self.artist_view.upcast_ref::<gtk::Widget>(),
                self.folder_view.upcast_ref::<gtk::Widget>(),
                self.playlist_view.upcast_ref::<gtk::Widget>(),
                self.dyn_playlist_view.upcast_ref::<gtk::Widget>(),
                self.queue_view.upcast_ref::<gtk::Widget>(),
            ]
            .iter()
            .for_each(clone!(
                #[weak]
                view,
                move |item| {
                    item.connect_closure(
                        "show-sidebar-clicked",
                        false,
                        closure_local!(
                            #[watch]
                            view,
                            move |_: &gtk::Widget| {
                                view.set_show_sidebar(true);
                            }
                        ),
                    );
                }
            ));

            self.queue_view.connect_notify_local(
                Some("show-content"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.update_player_bar_visibility();
                    }
                ),
            );

            self.queue_view.connect_notify_local(
                Some("pane-collapsed"),
                clone!(
                    #[weak(rename_to = this)]
                    obj,
                    move |_, _| {
                        this.update_player_bar_visibility();
                    }
                ),
            );

            // Set up accent colour provider
            if let Some(display) = gdk::Display::default() {
                gtk::style_context_add_provider_for_display(
                    &display,
                    &self.provider,
                    gtk::STYLE_PROVIDER_PRIORITY_USER,
                );
            }

            // Set up blur & accent thread
            // TODO: Use asyncified
            let (sender_to_bg, bg_receiver) = async_channel::unbounded::<WindowMessage>();
            let _ = self.sender_to_bg.set(sender_to_bg);
            let (sender_to_fg, fg_receiver) = async_channel::bounded::<WindowMessage>(1); // block background thread until sent
            let bg_handle = gio::spawn_blocking(move || {
                let settings = settings_manager().child("ui");
                // Cached here to avoid having to load the same image multiple times
                let mut curr_data: Option<DynamicImage> = None;
                let mut curr_path: Option<PathBuf> = None;
                'outer: loop {
                    let curr_path_mut = curr_path.as_mut();
                    // Check if there is work to do (block until there is)
                    let mut last_msg: WindowMessage = bg_receiver
                        .recv_blocking()
                        .expect("Fatal: invalid message sent to window's blur thread");
                    // In case the queue has more than one item, get the last one.
                    while !bg_receiver.is_empty() {
                        last_msg = bg_receiver
                            .recv_blocking()
                            .expect("Fatal: invalid message sent to window's blur thread");
                    }
                    match last_msg {
                        WindowMessage::NewBackground(path, config) => {
                            if (curr_path_mut.is_some() && path != *curr_path_mut.unwrap())
                                || curr_path.is_none()
                            {
                                let di = Reader::open(&path).unwrap().decode().unwrap();
                                curr_path.replace(path);
                                // Guard against calls just after window creation: sizes will be 0, but
                                // we should still record the image data here as the next calls (with sizes)
                                // will only be Updates.
                                if config.width > 0 && config.height > 0 {
                                    let _ = sender_to_fg.send_blocking(WindowMessage::Result(
                                        run_blur(&di, &config),
                                        Some(get_dominant_color(&di, config.is_dark)),
                                        true,
                                    ));
                                    thread::sleep(Duration::from_millis(
                                        (settings.double("bg-transition-duration-s") * 1000.0)
                                            as u64,
                                    ));
                                }

                                curr_data.replace(di);
                            }
                            // Else no need to blur again
                            // (size/radius updates are never sent via this message)
                        }
                        WindowMessage::UpdateBackground(config) => {
                            if let Some(data) = curr_data.as_ref() {
                                if config.width > 0 && config.height > 0 {
                                    let _ = sender_to_fg.send_blocking(WindowMessage::Result(
                                        run_blur(data, &config),
                                        Some(get_dominant_color(data, config.is_dark)),
                                        config.fade,
                                    ));
                                }
                                if config.fade {
                                    thread::sleep(Duration::from_millis(
                                        (settings.double("bg-transition-duration-s") * 1000.0)
                                            as u64,
                                    ));
                                }
                            }
                        }
                        WindowMessage::UpdateAccent(is_dark) => {
                            if let Some(data) = curr_data.as_ref() {
                                let _ = sender_to_fg.send_blocking(WindowMessage::AccentResult(
                                    get_dominant_color(data, is_dark),
                                ));
                            }
                        }
                        WindowMessage::ClearBackground => {
                            curr_data = None;
                            curr_path = None;
                        }
                        WindowMessage::Stop => {
                            println!("Stopping background blur thread...");
                            break 'outer;
                        }
                        _ => unreachable!(), // we shouldn't ever send BlurResult to the child thread
                    }
                }
            });
            let _ = self.bg_handle.replace(Some(bg_handle));

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
                            WindowMessage::Result(tex, maybe_accent, do_fade) => {
                                this.push_tex(Some(tex), do_fade);
                                let _ = this.accent_color.replace(maybe_accent);
                                this.update_accent_color();
                            }
                            WindowMessage::AccentResult(accent) => {
                                let _ = this.accent_color.replace(Some(accent));
                                this.update_accent_color();
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            ));

            // FPS counter (debug) – prints to stdout every 2 seconds
            // self.fps_frame_count.set(0);
            // self.fps_last_time.set(Some(std::time::Instant::now()));
            // let obj = self.obj();
            // let fps_tick = obj.add_tick_callback(clone!(
            //     #[weak(rename_to = this)]
            //     self,
            //     move |_widget, _delta| {
            //         let count = this.fps_frame_count.get();
            //         this.fps_frame_count.set(count + 1);
            //         let now = std::time::Instant::now();
            //         if let Some(last) = this.fps_last_time.get() {
            //             let elapsed = now.duration_since(last);
            //             if elapsed.as_secs_f64() >= 2.0 {
            //                 let fps = count as f64 / elapsed.as_secs_f64();
            //                 println!("FPS: {:.1}", fps);
            //                 this.fps_frame_count.set(0);
            //                 this.fps_last_time.set(Some(now));
            //             }
            //         } else {
            //             this.fps_last_time.set(Some(now));
            //         }
            //         glib::ControlFlow::Continue
            //     }
            // ));
            // self.fps_tick_id.replace(Some(fps_tick));

            self.update_accent_color();
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
                if self.bg_paintable.will_paint() {
                    let bg_opacity = self.bg_opacity.get();
                    if bg_opacity < 1.0 {
                        snapshot.push_opacity(bg_opacity);
                    }
                    self.bg_paintable.snapshot(
                        snapshot,
                        widget.width() as f64,
                        widget.height() as f64,
                    );
                    if bg_opacity < 1.0 {
                        snapshot.pop();
                    }
                }
            }

            // Spectrum visualiser
            if self.use_visualizer.get() {
                let mutex = self.fft_data.get().unwrap();
                let scale = self.visualizer_scale.get() as f32;
                let fg: gdk::RGBA;
                if let Some(rgb) = self.accent_color.borrow().as_ref() {
                    fg = gdk::RGBA::new(
                        rgb.r as f32 / 255.0,
                        rgb.g as f32 / 255.0,
                        rgb.b as f32 / 255.0,
                        1.0,
                    );
                } else {
                    fg = adw::StyleManager::default().accent_color_rgba();
                }
                // Halve configured opacity since we're drawing two channels
                let bar_height = self.player_bar_revealer.height() as f32;
                // eprintln!("bar height: {}", bar_height);
                let width32 = widget.width() as f32;
                let height32 = widget.height() as f32;
                let surface_height = (height32 - bar_height).max(0.0);
                let data = mutex.lock().unwrap();

                match self.visualizer_use_cairo.get() {
                    true => {
                        // New CPU‑only Cairo implementation
                        self.draw_spectrum_cairo_pair(
                            snapshot,
                            width32,
                            surface_height,
                            &data.0,
                            &data.1,
                            scale,
                            &fg,
                        );
                    }
                    false => {
                        // Existing GSK‑node implementation
                        self.draw_spectrum(snapshot, width32, surface_height, &data.0, scale, &fg);
                        self.draw_spectrum(snapshot, width32, surface_height, &data.1, scale, &fg);
                    }
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
        pub fn set_auto_accent(&self, new: bool) {
            let old = self.auto_accent.replace(new);
            if old != new {
                if new {
                    self.obj().queue_background_update(false);
                } else {
                    let _ = self.accent_color.take();
                    self.update_accent_color();
                }
                self.obj().notify("auto-accent");
            }
        }

        pub fn update_accent_color(&self) {
            if let (Some(color), true) =
                (self.accent_color.borrow().as_ref(), self.auto_accent.get())
            {
                // Is the generated accent colour too bright?
                // Luminance formula: L = 0.2126 * R + 0.7152 * G + 0.0722 * B
                let lum = 0.2126 * color.r as f32 / 255.0
                    + 0.7152 * color.g as f32 / 255.0
                    + 0.0722 * color.b as f32 / 255.0;
                if lum > 0.5 {
                    self.provider.load_from_string(&format!(
                        "
:root {{
    --accent-bg-color: rgb({}, {}, {});
    --accent-fg-color: rgb(0 0 0 / 80%);
}}
.fg-auto-accent {{
    color: rgb({}, {}, {});
}}
",
                        color.r, color.g, color.b, color.r, color.g, color.b
                    ));
                } else {
                    self.provider.load_from_string(&format!(
                        "
:root {{
    --accent-bg-color: rgb({}, {}, {});
}}
.fg-auto-accent {{
    color: rgb({}, {}, {});
}}
",
                        color.r, color.g, color.b, color.r, color.g, color.b
                    ));
                }
            } else {
                // If no accent colour is given, revert to system accent colour
                let color = adw::StyleManager::default().accent_color_rgba();
                let (r, g, b) = (
                    (color.red() * 255.0).round() as u32,
                    (color.green() * 255.0).round() as u32,
                    (color.blue() * 255.0).round() as u32,
                );
                self.provider.load_from_string(&format!(
                    "
:root {{
    --accent-bg-color: rgb({}, {}, {});
}}
.fg-auto-accent {{
    color: rgb({}, {}, {});
}}
",
                    r, g, b, r, g, b
                ));
            }
        }

        /// Force window to be redrawn on each frame.
        ///
        /// This is currently necessary for the visualiser to get updated.
        pub fn set_always_redraw(&self, state: bool) {
            if state {
                if let Some(old_id) =
                    self.tick_callback
                        .replace(Some(self.obj().add_tick_callback(move |obj, _| {
                            obj.queue_draw();
                            glib::ControlFlow::Continue
                        })))
                {
                    old_id.remove();
                }
            } else if let Some(old_id) = self.tick_callback.take() {
                old_id.remove();
            }
        }

        #[inline]
        fn trace_spectrum_top(
            path_builder: &gsk::PathBuilder,
            band_width: f32,
            ys: &[f32],
            use_splines: bool,
        ) {
            let n = ys.len();
            if use_splines {
                // Catmull-Rom spline via cubic Bezier (goes smoothly through all points).
                // For segment i (from P[i] to P[i+1]):
                //   cp1 = P[i] + (P[i+1] - P[i-1]) / 6
                //   cp2 = P[i+1] - (P[i+2] - P[i]) / 6
                // Boundary clamping: P[-1] = P[0], P[n+1] = P[n-1].

                for i in 1..(n - 1) {
                    let p0 = if i == 0 { 0 } else { i - 1 };
                    let p1 = i;
                    let p2 = i + 1;
                    let p3 = if i + 2 < n { i + 2 } else { n - 1 };

                    let x1 = p1 as f32 * band_width;
                    let x2 = p2 as f32 * band_width;

                    let ctrl1_x = x1 + (x2 - p0 as f32 * band_width) / 6.0;
                    let ctrl1_y = ys[p1] + (ys[p2] - ys[p0]) / 6.0;
                    let ctrl2_x = x2 - (p3 as f32 * band_width - x1) / 6.0;
                    let ctrl2_y = ys[p2] - (ys[p3] - ys[p1]) / 6.0;

                    path_builder.cubic_to(ctrl1_x, ctrl1_y, ctrl2_x, ctrl2_y, x2, ys[p2]);
                }
            } else {
                // Straight segments mode
                for (band_idx, y) in ys[0..n].iter().enumerate().skip(1) {
                    path_builder.line_to((band_idx as f32) * band_width, *y);
                }
            }
        }

        fn draw_spectrum(
            &self,
            snapshot: &gtk::Snapshot,
            width: f32,
            height: f32,
            data: &[f32],
            scale: f32,
            color: &gdk::RGBA,
        ) {
            let n = data.len();
            let mut ys = Vec::with_capacity(n);
            for &level in data {
                ys.push(compute_visualizer_y(level, height, scale));
            }
            // y-axis is top-down so min-y is the highest point :)
            let band_width = width / (data.len() as f32 - 1.0);
            let path_builder = gsk::PathBuilder::new();
            path_builder.move_to(0.0, height);
            path_builder.line_to(0.0, ys[0]);
            let use_splines = self.visualizer_use_splines.get();
            Self::trace_spectrum_top(&path_builder, band_width, &ys, use_splines);
            // Park at bottom-right
            path_builder.line_to(width, height);
            let path = path_builder.to_path();

            let mut y_min = ys[0];
            for y in ys[1..].iter() {
                if *y < y_min {
                    y_min = *y;
                }
            }

            snapshot.push_fill(&path, gsk::FillRule::EvenOdd);
            let bottom_stop = gsk::ColorStop::new(
                0.0,
                color.with_alpha(self.visualizer_bottom_opacity.get() as f32 / 2.0),
            );
            let top_stop = gsk::ColorStop::new(
                1.0,
                color.with_alpha(self.visualizer_top_opacity.get() as f32 / 2.0),
            );
            snapshot.append_linear_gradient(
                &graphene::Rect::new(0.0, y_min, width, height),
                &graphene::Point::new(0.0, height),
                &graphene::Point::new(0.0, y_min),
                &[bottom_stop, top_stop],
            );
            // Fill node
            snapshot.pop();
            let stroke_width = self.visualizer_stroke_width.get() as f32;
            if stroke_width > 0.0 {
                // Re-trace the top as a different path. This allows us to only stroke the top edge (the wavy bit)
                let path_builder = gsk::PathBuilder::new();
                path_builder.move_to(0.0, ys[0]);
                let use_splines = self.visualizer_use_splines.get();
                Self::trace_spectrum_top(&path_builder, band_width, &ys, use_splines);
                let path = path_builder.to_path();
                snapshot.append_stroke(&path, &gsk::Stroke::new(stroke_width), top_stop.color());
            }
        }

        /// Compute the y position of the highest point in spectrum data (snapshot coordinates).
        fn compute_y_min(data: &[f32], height: f32, scale_factor: f32) -> f32 {
            data.iter()
                .map(|&v| compute_visualizer_y(v, height, scale_factor))
                .fold(height, f32::min)
        }

        // Context should currently be at the leftmost point of the top edge.
        // This will then plot a line to the rightmost point.
        // The resulting context can then be used to draw a stroke, or with a few more edges added,
        // fill the shape.
        #[inline]
        fn cairo_trace_spectrum_top(
            cr: &cairo::Context,
            band_width: f64,
            height: f64, // STILL WINDOW HEIGHT
            ys: &[f64],
            use_splines: bool,
        ) {
            let n = ys.len();
            if use_splines && n >= 2 {
                // Catmull-Rom (goes smoothly through N points)
                // For segment i (from P[i] to P[i+1]):
                //   cp1 = P[i] + (P[i+1] - P[i-1]) / 6
                //   cp2 = P[i+1] - (P[i+2] - P[i]) / 6
                // Boundary clamping: P[-1] = P[0], P[n] = P[n-1].

                for i in 1..(n - 1) {
                    let p0 = if i == 0 { 0 } else { i - 1 };
                    let p1 = i;
                    let p2 = i + 1;
                    let p3 = if i + 2 < n { i + 2 } else { n - 1 };

                    let x0 = p0 as f64 * band_width;
                    let x1 = p1 as f64 * band_width;
                    let x2 = p2 as f64 * band_width;
                    let x3 = p3 as f64 * band_width;

                    // clamped at boundaries
                    let ctrl1_x = x1 + (x2 - x0) / 6.0;
                    let ctrl1_y = ys[p1] + (ys[p2] - ys[p0]) / 6.0;
                    let ctrl2_x = x2 - (x3 - x1) / 6.0;
                    let ctrl2_y = ys[p2] - (ys[p3] - ys[p1]) / 6.0;

                    cr.curve_to(ctrl1_x, ctrl1_y, ctrl2_x, ctrl2_y, x2, ys[p2]);
                }
            } else if n >= 2 {
                // Straight segments (original behavior).
                for (i, &y) in ys.iter().enumerate().skip(1) {
                    let x = i as f64 * band_width;
                    cr.line_to(x, y);
                }
            }
        }

        /// Draw one channel's spectrum data on an already-created Cairo context.
        /// Returns the y_min of the highest point in Cairo coordinates.
        #[allow(clippy::too_many_arguments)]
        fn draw_spectrum_cairo_channel(
            &self,
            cr: &cairo::Context,
            width: f64,
            height: f64, // STILL WINDOW HEIGHT. Undocumented, but regardless of the passed bounds, the Cairo draw area still uses the full window's coordinates
            y_min: f64,
            data: &[f32],
            scale: f32,
            color: &gdk::RGBA,
            has_fill: bool,
            top_opacity: f64,
            bottom_opacity: f64,
            use_splines: bool,
        ) {
            let n = data.len();
            let band_width = width / (n as f64 - 1.0);

            // Pre-compute all y values to avoid redundant calculations.
            let mut ys = Vec::with_capacity(n);
            for &level in data {
                ys.push(compute_visualizer_y(level, height as f32, scale) as f64);
            }

            // ----------  Build the polygon ----------
            if has_fill {
                // Move to bottom left corner then first point (i.e. draw left edge as straight line)
                cr.move_to(0.0, height);
                cr.line_to(0.0, ys[0]);
                Self::cairo_trace_spectrum_top(cr, band_width, height, &ys, use_splines);
                // Park the cursor at the bottom-right corner before closing path
                cr.line_to(width, height);
                cr.close_path();
                let gradient = cairo::LinearGradient::new(0.0, height, 0.0, y_min);
                gradient.add_color_stop_rgba(
                    0.0,
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    bottom_opacity / 2.0,
                );
                gradient.add_color_stop_rgba(
                    1.0,
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    top_opacity / 2.0,
                );
                cr.set_source(&gradient);
                cr.fill();
            }

            // Optional stroke
            let stroke_width = self.visualizer_stroke_width.get();
            if stroke_width > 0.0 {
                // The above has flushed the edge data. Regenerate here.
                // No need to draw the three straight edges.
                cr.move_to(0.0, ys[0]);
                Self::cairo_trace_spectrum_top(cr, band_width, height, &ys, use_splines);
                cr.set_line_width(stroke_width);
                cr.set_source_rgba(
                    color.red() as f64,
                    color.green() as f64,
                    color.blue() as f64,
                    self.visualizer_top_opacity.get(),
                );
                cr.stroke();
            }
        }

        /// Draw both left and right spectrum channels on a single Cairo surface.
        /// The surface is sized to the actual bounding box of the spectrum data,
        /// significantly reducing allocation size compared to full-window surfaces.
        #[allow(clippy::too_many_arguments)]
        fn draw_spectrum_cairo_pair(
            &self,
            snapshot: &gtk::Snapshot,
            width: f32,
            height: f32,
            data_left: &[f32],
            data_right: &[f32],
            scale: f32,
            color: &gdk::RGBA,
        ) {
            if width <= 0.0 || height <= 0.0 {
                return;
            }

            let top_opacity = self.visualizer_top_opacity.get();
            let bottom_opacity = self.visualizer_bottom_opacity.get();
            let has_fill = top_opacity > 0.0 || bottom_opacity > 0.0;

            // Find the bounding box (y_min) for both channels combined.
            let y_min_left = Self::compute_y_min(data_left, height, scale);
            let y_min_right = Self::compute_y_min(data_right, height, scale);
            let y_min = y_min_left.min(y_min_right);

            // Skip drawing entirely when spectrum is flat (near-silence).
            let surface_height = (height - y_min) as f64;
            if surface_height < 1.0 {
                return;
            }

            // Allocate Cairo surface at the bounding box size instead of full window.
            // Cairo coords are still relative to the whole window.
            let stroke_width = self.visualizer_stroke_width.get();
            let cr = snapshot.append_cairo(&graphene::Rect::new(
                0.0,
                y_min - stroke_width as f32,
                width,
                surface_height as f32,
            ));

            // Draw left channel.
            self.draw_spectrum_cairo_channel(
                &cr,
                width as f64,
                height as f64,
                y_min as f64,
                data_left,
                scale,
                color,
                has_fill,
                top_opacity,
                bottom_opacity,
                self.visualizer_use_splines.get(),
            );

            // Draw right channel on the same surface.
            self.draw_spectrum_cairo_channel(
                &cr,
                width as f64,
                height as f64,
                y_min as f64,
                data_right,
                scale,
                color,
                has_fill,
                top_opacity,
                bottom_opacity,
                self.visualizer_use_splines.get(),
            );
        }

        /// Fade to the new texture, or to nothing if playing song has no album art.
        pub fn push_tex(&self, tex: Option<gdk::MemoryTexture>, do_fade: bool) {
            let bg_paintable = self.bg_paintable.clone();
            if self.use_album_art_bg.get() && tex.is_some() {
                if !self.content.has_css_class("no-shading") {
                    self.content.add_css_class("no-shading");
                }
            } else if self.content.has_css_class("no-shading") {
                self.content.remove_css_class("no-shading");
            }
            // Will immediately re-blur and upload to GPU at current size
            bg_paintable.set_new_content(tex);
            if do_fade {
                // Once we've finished the above (expensive) operations, we can safely start
                // the fade animation without worrying about stuttering.
                glib::idle_add_local_once(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move || {
                        // Run fade transition once main thread is free
                        // Remember to queue draw too
                        let duration = (bg_paintable.transition_duration() * 1000.0).round() as u32;
                        let anim_target = adw::CallbackAnimationTarget::new(clone!(
                            #[weak]
                            this,
                            move |progress: f64| {
                                bg_paintable.set_fade(progress);
                                this.obj().queue_draw();
                            }
                        ));
                        let anim = adw::TimedAnimation::new(
                            this.obj().as_ref(),
                            0.0,
                            1.0,
                            duration,
                            anim_target,
                        );
                        anim.play();
                    }
                ));
            } else {
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
    @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible,
    gtk::Buildable, gtk::ConstraintTarget, gtk::Native, gtk::Root,
    gtk::ShortcutManager;
}

impl EuphonicaWindow {
    pub fn new<P: glib::object::IsA<gtk::Application>>(application: &P) -> Self {
        let win: Self = glib::Object::builder()
            .property("application", application)
            .build();

        let app = win.downcast_application();
        let client_state = app.get_client().get_client_state();
        let _ = win.imp().client_state.set(client_state.clone());
        let player = app.get_player();

        // Construct all views first
        win.restore_window_state();
        win.imp()
            .queue_view
            .setup(app.get_player(), app.get_cache(), &client_state, &win);
        win.imp()
            .recent_view
            .setup(app.get_library(), app.get_player(), app.get_cache(), &win);
        win.imp().album_view.setup(
            app.get_library(),
            app.get_cache(),
            &app.get_client().get_client_state(),
            &win,
        );
        win.imp()
            .artist_view
            .setup(app.get_library(), app.get_cache(), &win);
        win.imp()
            .folder_view
            .setup(app.get_library(), app.get_cache());
        win.imp().dyn_playlist_view.setup(
            app.get_library(),
            app.get_cache(),
            &app.get_client().get_client_state(),
            &win,
        );
        win.imp().playlist_view.setup(
            app.get_library(),
            app.get_cache(),
            &app.get_client().get_client_state(),
            &win,
        );
        win.imp().sidebar.setup(&win, &app);
        win.imp()
            .player_bar
            .setup(app.get_player(), app.get_cache());

        // Now that all the components are ready, we can start handling backend state changes
        win.imp()
            .fft_data
            .set(player.fft_data())
            .expect("Unable to bind FFT data to visualiser widget");

        win.queue_new_background();

        win.imp()
            .client_state_idle_id
            .replace(Some(client_state.connect_closure(
                "idle",
                false,
                closure_local!(
                    #[watch(rename_to = this)]
                    win,
                    move |_: ClientState, subsys: BoxedAnyObject| {
                        if subsys.borrow::<Subsystem>().deref() == &Subsystem::Database {
                            this.send_simple_toast("Database updated with changes", 3);
                        }
                    }
                ),
            )));
        win.handle_connection_state(client_state.connection_state());
        win.imp()
            .client_state_conn_state_id
            .replace(Some(client_state.connect_notify_local(
                Some("connection-state"),
                clone!(
                    #[weak(rename_to = this)]
                    win,
                    move |state: &ClientState, _| {
                        this.handle_connection_state(state.connection_state());
                    }
                ),
            )));

        win.imp()
            .player_cover_changed_id
            .replace(Some(player.connect_closure(
                "cover-changed",
                false,
                closure_local!(
                    #[watch(rename_to = this)]
                    win,
                    move |_: Player| {
                        this.queue_new_background();
                    }
                ),
            )));
        win.handle_connection_state(client_state.connection_state());
        *win.imp().client_state_conn_state_id.borrow_mut() =
            Some(client_state.connect_notify_local(
                Some("connection-state"),
                clone!(
                    #[weak(rename_to = this)]
                    win,
                    move |state: &ClientState, _| {
                        this.handle_connection_state(state.connection_state());
                    }
                ),
            ));

        win.imp()
            .player_cover_changed_id
            .replace(Some(player.connect_closure(
                "cover-changed",
                false,
                closure_local!(
                    #[watch(rename_to = this)]
                    win,
                    move |_: Player| {
                        this.queue_new_background();
                    }
                ),
            )));
        win.imp().player.set(Some(player));

        win.imp().stack.connect_visible_child_name_notify(clone!(
            #[weak(rename_to = this)]
            win,
            move |_| {
                this.maybe_populate_visible();
            }
        ));

        win.imp().player_bar.connect_closure(
            "goto-pane-clicked",
            false,
            closure_local!(
                #[watch(rename_to = this)]
                win,
                move |_: PlayerBar| {
                    this.goto_pane();
                }
            ),
        );

        win.imp().artist_view.get_content_view().connect_closure(
            "album-clicked",
            false,
            closure_local!(
                #[watch(rename_to = this)]
                win,
                move |_: ArtistContentView, album: Album| {
                    this.goto_album(&album);
                }
            ),
        );

        win.bind_state();
        win.setup_signals();

        // Refresh background
        win.queue_new_background();
        win
    }

    pub fn update_background_css_classes(&self) {
        if self.imp().use_album_art_bg.get() || self.imp().use_visualizer.get() {
            if !self.imp().content.has_css_class("no-shading") {
                self.imp().content.add_css_class("no-shading");
            }
        } else if self.imp().content.has_css_class("no-shading") {
            self.imp().content.remove_css_class("no-shading");
        }
    }

    pub fn get_stack(&self) -> gtk::Stack {
        self.imp().stack.get()
    }

    pub fn get_split_view(&self) -> adw::OverlaySplitView {
        self.imp().split_view.get()
    }

    pub fn get_playlist_view(&self) -> PlaylistView {
        self.imp().playlist_view.get()
    }

    pub fn get_dyn_playlist_view(&self) -> DynamicPlaylistView {
        self.imp().dyn_playlist_view.get()
    }

    pub fn send_simple_toast(&self, title: &str, timeout: u32) {
        let toast = adw::Toast::builder().title(title).timeout(timeout).build();
        self.imp().toast_overlay.add_toast(toast);
    }

    fn show_error_dialog(&self, heading: &str, body: &str, suggest_open_preferences: bool) {
        // Show an alert ONLY IF the preferences dialog is not already open.
        if self.visible_dialog().is_none() {
            let diag = adw::AlertDialog::builder()
                .heading(heading)
                .body(body)
                .build();
            diag.add_response("close", "_Close");
            if suggest_open_preferences {
                diag.add_response("prefs", "Open _Preferences");
                diag.set_response_appearance("prefs", adw::ResponseAppearance::Suggested);
                diag.choose(
                    Some(self),
                    Option::<gio::Cancellable>::None.as_ref(),
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move |resp| {
                            if resp == "prefs" {
                                this.downcast_application().show_preferences();
                            }
                        }
                    ),
                );
            } else {
                diag.present(Some(self));
            }
        }
    }

    fn handle_connection_state(&self, state: ConnectionState) {
        match state {
            ConnectionState::ConnectionRefused => {
                self.imp().title.set_subtitle("Not connected");
                let conn_settings = utils::settings_manager().child("client");
                self.show_error_dialog(
                    "Connection refused",
                    &format!(
                        "Euphonica could not connect to {}:{}. Please check your connection and network configuration and try again.",
                        conn_settings.string("mpd-host").as_str(),
                        conn_settings.uint("mpd-port")
                    ),
                    true
                );
            }
            ConnectionState::SocketNotFound => {
                self.imp().title.set_subtitle("Not connected");
                let conn_settings = utils::settings_manager().child("client");
                self.show_error_dialog(
                    "Socket not found",
                    &format!(
                        "Euphonica couldn't connect to your socket at {}. Please ensure that MPD has been configured to bind to that socket and try again.",
                        conn_settings.string("mpd-unix-socket").as_str(),
                    ),
                    true
                );
            }
            ConnectionState::WrongPassword => {
                self.imp().title.set_subtitle("Unauthenticated");
                self.show_error_dialog(
                    "Incorrect password",
                    "MPD has refused the provided password. Please note that if your MPD instance is not password-protected, providing one will also cause this error.",
                    true
                );
            }
            ConnectionState::Unauthenticated => {
                self.imp().title.set_subtitle("Not connected");
                self.show_error_dialog(
                    "Authentication Failed",
                    "The current password lacks the necessary privileges for Euphonica to function.",
                    true
                );
            }
            ConnectionState::CredentialStoreError => {
                self.imp().title.set_subtitle("Unauthenticated");
                self.show_error_dialog(
                    "Credential Store Error",
                    "Your MPD instance requires a password, but Euphonica could not access your default credential store to retrieve it. Please ensure that it has been unlocked before starting Euphonica.",
                    false
                );
            }
            ConnectionState::Connecting => {
                let imp = self.imp();
                imp.title.set_subtitle("Connecting");
                imp.should_populate_visible.set(false);
            }
            ConnectionState::Connected => {
                let imp = self.imp();
                imp.title.set_subtitle("Connected");
                imp.should_populate_visible.set(true);
                // Initialise content for the currently-visible view
                self.maybe_populate_visible();
            }
            _ => {}
        }
    }

    pub fn maybe_populate_visible(&self) {
        let imp = self.imp();
        if imp.should_populate_visible.get()
            && let Some(visible_child_name) = imp.stack.visible_child_name()
        {
            match visible_child_name.as_str() {
                "recent" => {
                    imp.recent_view.populate();
                }
                "albums" => {
                    imp.album_view.populate();
                }
                "artists" => {
                    imp.artist_view.populate();
                }
                "folders" => {
                    imp.folder_view.populate();
                }
                "queue" => {
                    imp.queue_view.populate();
                }
                _ => {}
            }
        }
    }

    pub fn show_dialog(&self, heading: &str, body: &str) {
        let diag = adw::AlertDialog::builder()
            .heading(heading)
            .body(body)
            .build();
        diag.present(Some(self));
    }

    fn update_player_bar_visibility(&self) {
        let revealer = self.imp().player_bar_revealer.get();
        if self.imp().sidebar.showing_queue_view() {
            let queue_view = self.imp().queue_view.get();
            if (queue_view.pane_collapsed() && !queue_view.show_content())
                || !queue_view.pane_collapsed()
            {
                revealer.set_reveal_child(false);
            } else {
                revealer.set_reveal_child(true);
            }
        } else {
            revealer.set_reveal_child(true);
        }
    }

    fn goto_pane(&self) {
        self.imp().sidebar.set_view("queue");
        self.imp()
            .split_view
            .set_show_sidebar(!self.imp().split_view.is_collapsed());
        self.imp().queue_view.set_show_content(false);
    }

    pub fn goto_album(&self, album: &Album) {
        self.imp().album_view.on_album_clicked(album);
        self.imp().sidebar.set_view("albums");
        if self.imp().split_view.shows_sidebar() {
            self.imp()
                .split_view
                .set_show_sidebar(!self.imp().split_view.is_collapsed());
        }
    }

    pub fn goto_artist(&self, artist: &Artist) {
        self.imp().artist_view.on_artist_clicked(artist);
        self.imp().sidebar.set_view("artists");
        if self.imp().split_view.shows_sidebar() {
            self.imp()
                .split_view
                .set_show_sidebar(!self.imp().split_view.is_collapsed());
        }
    }

    pub fn goto_playlist(&self, playlist: &INode) {
        self.imp().playlist_view.on_playlist_clicked(playlist);
        self.imp().sidebar.set_view("playlists");
        if self.imp().split_view.shows_sidebar() {
            self.imp()
                .split_view
                .set_show_sidebar(!self.imp().split_view.is_collapsed());
        }
    }

    /// Set blurred background to a new image, if enabled. Use thumbnail version to
    /// minimise disk read time.
    fn queue_new_background(&self) {
        if let Some(player) = self.imp().player.upgrade() {
            if let Some(sender) = self.imp().sender_to_bg.get() {
                glib::spawn_future_local(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[weak]
                    player,
                    #[strong]
                    sender,
                    #[upgrade_or]
                    ClientResult::Ok(()),
                    async move {
                        if let Some(path) = player
                            .current_song_cover_path(true)
                            .await?
                            .and_then(|path| if path.exists() { Some(path) } else { None })
                        {
                            let settings = settings_manager().child("ui");
                            let config = BlurConfig {
                                width: this.width() as u32,
                                height: this.height() as u32,
                                radius: settings.uint("bg-blur-radius"),
                                is_dark: adw::StyleManager::default().is_dark(),
                                fade: true, // new image, must fade
                            };
                            let _ =
                                sender.send_blocking(WindowMessage::NewBackground(path, config));
                        } else {
                            let _ = sender.send_blocking(WindowMessage::ClearBackground);
                            this.imp().push_tex(None, true);
                        }
                        Ok(())
                    }
                ));
            } else {
                self.imp().push_tex(None, true);
            }
        }
    }

    fn queue_background_update(&self, fade: bool) {
        if let Some(sender) = self.imp().sender_to_bg.get() {
            let settings = settings_manager().child("ui");
            let config = BlurConfig {
                width: self.width() as u32,
                height: self.height() as u32,
                radius: settings.uint("bg-blur-radius"),
                is_dark: adw::StyleManager::default().is_dark(),
                fade,
            };
            let _ = sender.send_blocking(WindowMessage::UpdateBackground(config));
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

    fn update_fg_task_count(&self, state: &ClientState) {
        let pct = state.pct_done_fg_tasks();
        self.imp().fg_progress.set_fraction(pct);
        self.imp().fg_task_count.set_label(&format!(
            "{}/{}",
            &state.n_done_fg_tasks(),
            &state.n_fg_tasks()
        ));
    }

    fn update_bg_task_count(&self, state: &ClientState) {
        let pct = state.pct_done_bg_tasks();
        self.imp().bg_progress.set_fraction(pct);
        self.imp().bg_task_count.set_label(&format!(
            "{}/{}",
            &state.n_done_bg_tasks(),
            &state.n_bg_tasks()
        ));
    }

    fn bind_state(&self) {
        // Bind client state to app name widget
        let client = self.downcast_application().get_client();
        let state = client.get_client_state();

        state
            .bind_property(
                "has-pending",
                &self.imp().pending_tasks_btn.get(),
                "visible",
            )
            .sync_create()
            .build();

        state
            .bind_property(
                "n-fg-tasks",
                &self.imp().pending_fg_stack.get(),
                "visible-child-name",
            )
            .transform_to(|_, n: u64| Some((if n > 0 { "pending" } else { "idle" }).to_value()))
            .sync_create()
            .build();

        state
            .bind_property(
                "n-bg-tasks",
                &self.imp().pending_bg_stack.get(),
                "visible-child-name",
            )
            .transform_to(|_, n: u64| Some((if n > 0 { "pending" } else { "idle" }).to_value()))
            .sync_create()
            .build();

        self.imp()
            .client_state_pct_fg_id
            .replace(Some(state.connect_notify_local(
                Some("pct-done-fg-tasks"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |state: &ClientState, _| this.update_fg_task_count(state)
                ),
            )));
        self.imp()
            .client_state_pct_bg_id
            .replace(Some(state.connect_notify_local(
                Some("pct-done-bg-tasks"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |state: &ClientState, _| this.update_bg_task_count(state)
                ),
            )));
        self.imp()
            .client_state_n_fg_id
            .replace(Some(state.connect_notify_local(
                Some("n-fg-tasks"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |state: &ClientState, _| this.update_fg_task_count(state)
                ),
            )));
        self.imp()
            .client_state_n_bg_id
            .replace(Some(state.connect_notify_local(
                Some("n-bg-tasks"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |state: &ClientState, _| this.update_bg_task_count(state)
                ),
            )));

        // Remove default libadwaita sidebar backgrounds when using
        // album art as background, or the visualiser is enabled, or both.
        self.connect_notify_local(Some("use-album-art-as-background"), move |win, _| {
            win.update_background_css_classes();
        });
        self.connect_notify_local(Some("use-visualizer"), move |win, _| {
            win.update_background_css_classes();
        });
        self.update_background_css_classes();
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

            // Stop everything
            // Tick callback for resizing detection
            if let Some(tick) = window.imp().tick_callback.take() {
                tick.remove();
            }

            // Stop blur thread when closing window.
            // We need to take care of this now that the app's lifetime is decoupled from the window
            // (background running support)
            if let Some(handle) = window.imp().bg_handle.take() {
                window
                    .imp()
                    .sender_to_bg
                    .get()
                    .unwrap()
                    .send_blocking(WindowMessage::Stop)
                    .expect("Could not stop background blur thread");

                glib::MainContext::default().block_on(async move {
                    if let Err(e) = handle.await {
                        dbg!(e);
                    }
                });
            }

            window.downcast_application().on_window_closed();

            // TODO: persist other settings at closing?
            glib::Propagation::Proceed
        });
    }
}
