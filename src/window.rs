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
    client::{ClientError, ClientState, ConnectionState},
    common::{blend_mode::*, paintables::FadePaintable, Album, Artist},
    library::{AlbumView, ArtistContentView, ArtistView, FolderView, PlaylistView, RecentView},
    player::{Player, PlayerBar, QueueView},
    sidebar::Sidebar,
    utils::{self, settings_manager},
};
use adw::{prelude::*, subclass::prelude::*};
use glib::signal::SignalHandlerId;
use gtk::{
    gdk, gio,
    glib::{self, clone, closure_local, BoxedAnyObject},
    graphene, gsk, CssProvider
};
use image::{imageops::FilterType, DynamicImage};
use libblur::{stack_blur, FastBlurChannels, ThreadingPolicy};
use mpd::Subsystem;
use std::{cell::RefCell, ops::Deref, path::PathBuf, thread, time::Duration};
use auto_palette::{ImageData, Palette, Theme, color::RGB};
use std::{
    cell::{Cell, OnceCell},
    sync::{Arc, Mutex},
};

use async_channel::Sender;
use glib::Properties;
use image::ImageReader as Reader;

#[derive(Debug)]
pub struct BlurConfig {
    width: u32,
    height: u32,
    radius: u32,
    fade: bool, // Whether this update requires fading to it. Those for updating radius shouldn't be faded.
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

fn get_dominant_color(img: &DynamicImage) -> RGB {
    let colors = img.as_rgb8().unwrap().pixels().flat_map(|pixel| {
        [pixel[0], pixel[1], pixel[2], 255]
    }).collect::<Vec::<u8>>();

    let palette = Palette::<f32>::extract(&ImageData::new(img.width(), img.height(), &colors).unwrap()).unwrap();

    palette.find_swatches_with_theme(1, Theme::Colorful).first().unwrap().color().to_rgb()
}


pub enum WindowMessage {
    NewBackground(PathBuf, BlurConfig), // Load new image at FULL PATH & blur with given configuration. Will fade.
    UpdateBackground(BlurConfig),       // Re-blur current image but do not fade.
    ClearBackground,                    // Clears last-blurred cache.
    Result(gdk::MemoryTexture, Option<RGB>, bool), // GPU texture and whether to fade to this one.
    Stop,
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
        pub playlist_view: TemplateChild<PlaylistView>,
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

        // Blurred album art background
        #[property(get, set)]
        pub use_album_art_bg: Cell<bool>,
        #[property(get, set)]
        pub bg_opacity: Cell<f64>,
        pub bg_paintable: FadePaintable,
        pub player: OnceCell<Player>,
        pub sender_to_bg: OnceCell<Sender<WindowMessage>>, // sending a None will terminate the thread
        pub bg_handle: OnceCell<gio::JoinHandle<()>>,
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
        pub visualizer_blend_mode: Cell<u32>,
        #[property(get, set)]
        pub visualizer_use_splines: Cell<bool>,
        #[property(get, set)]
        pub visualizer_stroke_width: Cell<f64>,
        #[property(get, set = Self::set_auto_accent)]
        pub auto_accent: Cell<bool>,
        pub tick_callback: RefCell<Option<gtk::TickCallbackId>>,
        pub fft_data: OnceCell<Arc<Mutex<(Vec<f32>, Vec<f32>)>>>,
        pub accent_color: RefCell<Option<RGB>>,

        pub provider: CssProvider,
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
            let settings = settings_manager().child("ui");
            let obj_borrow = self.obj();
            let obj = obj_borrow.as_ref();
            let bg_paintable = &self.bg_paintable;

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

            settings.connect_changed(
                Some("bg-blur-radius"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| {
                        // Blur radius updates need not fade
                        this.obj().queue_background_update(false);
                    }
                ),
            );

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
                .bind("visualizer-blend-mode", obj, "visualizer-blend-mode")
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
            settings.connect_changed(
                Some("use-visualizer"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |settings, key| {
                        this.set_always_redraw(settings.boolean(key));
                    }
                ),
            );

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
            [
                self.recent_view.upcast_ref::<gtk::Widget>(),
                self.album_view.upcast_ref::<gtk::Widget>(),
                self.artist_view.upcast_ref::<gtk::Widget>(),
                self.folder_view.upcast_ref::<gtk::Widget>(),
                self.playlist_view.upcast_ref::<gtk::Widget>(),
                self.queue_view.upcast_ref::<gtk::Widget>()
            ].iter().for_each(clone!(
                #[weak]
                view,
                move |item| {
                    item.connect_local(
                        "show-sidebar-clicked",
                        false,
                        move |_| {
                            view.set_show_sidebar(true);
                            None
                        }
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
                gtk::style_context_add_provider_for_display(&display, &self.provider, gtk::STYLE_PROVIDER_PRIORITY_USER);
            }

            // Set up blur & accent thread
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
                                        Some(get_dominant_color(&di)),
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
                                        Some(get_dominant_color(&data)),  // No need to update accent colour
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
                            WindowMessage::Result(tex, maybe_accent, do_fade) => {
                                this.push_tex(Some(tex), do_fade);
                                let _ = this.accent_color.replace(maybe_accent);
                                this.update_accent_color();
                            }
                            _ => unreachable!(),
                        }
                    }
                }
            ));
        }
    }
    impl WidgetImpl for EuphonicaWindow {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let widget = self.obj();
            let mut should_blend = false;
            let blend_mode: BlendMode = self.visualizer_blend_mode.get().try_into().unwrap();
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
                    if self.will_draw_spectrum() {
                        should_blend = true;
                        snapshot.push_blend(blend_mode.into());
                    }
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
                    if should_blend {
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
                    fg = gdk::RGBA::new(rgb.r as f32 / 255.0, rgb.g as f32 / 255.0, rgb.b as f32 / 255.0, 1.0);
                }
                else {
                    fg = widget.color();
                }
                // Halve configured opacity since we're drawing two channels
                let width32 = widget.width() as f32;
                let height32 = widget.height() as f32;
                let data = mutex.lock().unwrap();
                self.draw_spectrum(snapshot, width32, height32, &data.0, scale, &fg);
                self.draw_spectrum(snapshot, width32, height32, &data.1, scale, &fg);
            }
            if should_blend {
                // Add top layer of blend node
                snapshot.pop();
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
                }
                else {
                    let _ = self.accent_color.take();
                    self.update_accent_color();
                }
                self.obj().notify("auto-accent");
            }
        }

        pub fn update_accent_color(&self) {
            if let (Some(color), true) = (self.accent_color.borrow().as_ref(), self.auto_accent.get()) {
                // Is the generated accent colour too bright?
                // Luminance formula: L = 0.2126 * R + 0.7152 * G + 0.0722 * B
                let lum = 0.2126 * color.r as f32 / 255.0 + 0.7152 * color.g as f32 / 255.0 + 0.0722 * color.b as f32 / 255.0;
                if lum > 0.5 {
                    self.provider.load_from_string(&format!("
:root {{
    --accent-bg-color: rgb({}, {}, {});
    --accent-fg-color: rgb(0 0 0 / 80%);
}}
.fg-auto-accent {{
    color: rgb({}, {}, {});
}}
",
                        color.r, color.g, color.b,
                        color.r, color.g, color.b
                    ));
                }
                else {
                    self.provider.load_from_string(&format!("
:root {{
    --accent-bg-color: rgb({}, {}, {});
}}
.fg-auto-accent {{
    color: rgb({}, {}, {});
}}
",
                        color.r, color.g, color.b,
                        color.r, color.g, color.b
                    ));
                }
            }
            else {
                // If no accent colour is given, revert to system accent colour
                self.provider.load_from_string("");
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
            } else {
                if let Some(old_id) = self.tick_callback.take() {
                    old_id.remove();
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
            color: &gdk::RGBA
        ) {
            let band_width = width / (data.len() as f32 - 1.0);

            let path_builder = gsk::PathBuilder::new();
            path_builder.move_to(0.0, height);
            path_builder.line_to(0.0, (height - data[0] * scale).max(0.0));

            // y-axis is top-down so min-y is the highest point :)
            let mut y_min = height;

            if self.visualizer_use_splines.get() {
                // Spline mode. Since we can make 2 assumptions:
                // - No two points share the same x-coordinate (duh), and
                // - X-coordinates are monotonically increasing
                // we can cheat a bit and avoid having to solve for Beizer control points.
                let half_width = band_width as f32 / 2.0;
                let quarter_width = band_width as f32 / 4.0;
                for i in 0..(data.len() - 1) {
                    let x = (i as f32 + 1.0) * band_width;
                    let y = (height - data[i] * scale * 1000000.0).max(0.0);
                    y_min = y_min.min(y);
                    let x_next = x + band_width;
                    let y_next = (height - data[i + 1] * scale * 1000000.0).max(0.0);
                    // Midpoint
                    let x_mid = x + half_width;
                    let y_mid =
                        (height - (data[i] + data[i + 1]) / 2.0 * scale * 1000000.0).max(0.0);
                    // The next two will serve as control points.
                    // Between current point and midpoint
                    let x_left_mid = x + quarter_width;
                    // Between midpoint and next point
                    let x_right_mid = x_mid + quarter_width;
                    // First curve, from current point to midpoint
                    path_builder.quad_to(
                        // Control point
                        x_left_mid, y, x_mid, y_mid,
                    );
                    // Second curve, from midpoint to next point
                    path_builder.quad_to(
                        // Control point
                        x_right_mid,
                        y_next,
                        x_next,
                        y_next,
                    );
                }
            } else {
                // Straight segments mode
                for (band_idx, level) in data[1..data.len()].iter().enumerate() {
                    let y = (height - level * scale * 1000000.0).max(0.0);
                    y_min = y_min.min(y);
                    path_builder.line_to(
                        (band_idx as f32 + 1.0) * band_width,
                        (height - level * scale * 1000000.0).max(0.0),
                    );
                }
            }
            path_builder.line_to(width, height);
            let path = path_builder.to_path();

            snapshot.push_fill(&path, gsk::FillRule::Winding);
            let bottom_stop = gsk::ColorStop::new(
                0.0,
                gdk::RGBA::new(
                    color.red(),
                    color.green(),
                    color.blue(),
                    self.visualizer_bottom_opacity.get() as f32 / 2.0,
                ),
            );
            let top_stop = gsk::ColorStop::new(
                1.0,
                gdk::RGBA::new(
                    color.red(),
                    color.green(),
                    color.blue(),
                    self.visualizer_top_opacity.get() as f32 / 2.0,
                ),
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
                snapshot.append_stroke(&path, &gsk::Stroke::new(stroke_width), top_stop.color());
            }
        }

        /// Whether any render node will be added to render the visualiser.
        ///
        /// This check is necessary to babysit the blend node's assertion that
        /// both layers be non-empty.
        fn will_draw_spectrum(&self) -> bool {
            if !self.use_visualizer.get() {
                return false;
            }
            if self.visualizer_stroke_width.get() > 0.0 {
                return true;
            }
            if let Some(mutex) = self.fft_data.get() {
                if let Ok(data) = mutex.lock() {
                    return (data.0.iter().sum::<f32>() + data.1.iter().sum::<f32>()) > 0.0;
                }
            }
            return false;
        }

        /// Fade to the new texture, or to nothing if playing song has no album art.
        pub fn push_tex(&self, tex: Option<gdk::MemoryTexture>, do_fade: bool) {
            let bg_paintable = self.bg_paintable.clone();
            if self.use_album_art_bg.get() && tex.is_some() {
                if !self.content.has_css_class("no-shading") {
                    self.content.add_css_class("no-shading");
                }
            } else {
                if self.content.has_css_class("no-shading") {
                    self.content.remove_css_class("no-shading");
                }
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
        let player = app.get_player();

        win.imp()
            .fft_data
            .set(player.fft_data())
            .expect("Unable to bind FFT data to visualiser widget");

        win.queue_new_background();
        client_state.connect_closure(
            "client-error",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                win,
                move |_: ClientState, err: ClientError| {
                    this.handle_client_error(err);
                }
            ),
        );
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
            ),
        );
        win.handle_connection_state(client_state.get_connection_state());
        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                win,
                move |state: &ClientState, _| {
                    this.handle_connection_state(state.get_connection_state());
                }
            )
        );

        player.connect_closure(
            "cover-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                win,
                move |_: Player, _: Option<gdk::Texture>| {
                    this.queue_new_background();
                }
            )
        );
        let _ = win.imp().player.set(player);

        win.restore_window_state();
        win.imp()
            .queue_view
            .setup(app.get_player(), app.get_cache(), win.clone());
        win.imp().recent_view.setup(
            app.get_library(),
            app.get_player(),
            app.get_cache(),
            &win
        );
        win.imp().album_view.setup(
            app.get_library(),
            app.get_cache(),
            app.get_client().get_client_state(),
            &win
        );
        win.imp().artist_view.setup(
            app.get_library(),
            app.get_client().get_client_state(),
            app.get_cache(),
        );
        win.imp().folder_view.setup(
            app.get_library(),
            app.get_cache()
        );
        win.imp().playlist_view.setup(
            app.get_library(),
            app.get_cache(),
            app.get_client().get_client_state(),
            win.clone(),
        );
        win.imp().sidebar.setup(&win, &app);
        win.imp().player_bar.setup(&app.get_player());

        win.imp().player_bar.connect_closure(
            "goto-pane-clicked",
            false,
            closure_local!(
                #[weak(rename_to = this)]
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
                #[weak(rename_to = this)]
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

    pub fn get_stack(&self) -> gtk::Stack {
        self.imp().stack.get()
    }

    pub fn get_split_view(&self) -> adw::OverlaySplitView {
        self.imp().split_view.get()
    }

    pub fn get_recent_view(&self) -> RecentView {
        self.imp().recent_view.get()
    }

    pub fn get_album_view(&self) -> AlbumView {
        self.imp().album_view.get()
    }

    pub fn get_artist_view(&self) -> ArtistView {
        self.imp().artist_view.get()
    }

    pub fn get_folder_view(&self) -> FolderView {
        self.imp().folder_view.get()
    }

    pub fn get_playlist_view(&self) -> PlaylistView {
        self.imp().playlist_view.get()
    }

    pub fn get_queue_view(&self) -> QueueView {
        self.imp().queue_view.get()
    }

    pub fn send_simple_toast(&self, title: &str, timeout: u32) {
        let toast = adw::Toast::builder().title(title).timeout(timeout).build();
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
                diag.choose(
                    self,
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
                self.show_error_dialog(
                    "Incorrect password",
                    "MPD has refused the provided password. Please note that if your MPD instance is not password-protected, providing one will also cause this error.",
                    true
                );
            }
            ConnectionState::Unauthenticated => {
                self.show_error_dialog(
                    "Authentication Failed",
                    "Your MPD instance requires a password, which was either not provided or lacks the necessary privileges for Euphonica to function correctly.",
                    true
                );
            }
            ConnectionState::CredentialStoreError => {
                self.show_error_dialog(
                    "Credential Store Error",
                    "Your MPD instance requires a password, but Euphonica could not access your default credential store to retrieve it. Please ensure that it has been unlocked before starting Euphonica.",
                    false
                );
            }
            _ => {}
        }
    }

    pub fn handle_client_error(&self, err: ClientError) {
        match err {
            ClientError::Queuing => {
                self.send_simple_toast("Some songs could not be queued", 3);
            }
            _ => {}
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
            if (queue_view.pane_collapsed() && queue_view.show_content()) || !queue_view.pane_collapsed() {
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
        // self.imp().stack.set_visible_child_name("queue");
        self.imp().split_view.set_show_sidebar(!self.imp().split_view.is_collapsed());
        self.imp().queue_view.set_show_content(true);
    }

    pub fn goto_album(&self, album: &Album) {
        self.imp().album_view.on_album_clicked(album);
        // self.imp().stack.set_visible_child_name("albums");
        self.imp().sidebar.set_view("albums");
        if self.imp().split_view.shows_sidebar() {
            self.imp().split_view.set_show_sidebar(!self.imp().split_view.is_collapsed());
        }
    }

    pub fn goto_artist(&self, artist: &Artist) {
        self.imp().artist_view.on_artist_clicked(artist);
        // self.imp().stack.set_visible_child_name("artists");
        self.imp().sidebar.set_view("artists");
        if self.imp().split_view.shows_sidebar() {
            self.imp().split_view.set_show_sidebar(!self.imp().split_view.is_collapsed());
        }
    }

    /// Set blurred background to a new image, if enabled. Use thumbnail version to
    /// minimise disk read time.
    fn queue_new_background(&self) {
        if let Some(player) = self.imp().player.get() {
            if let Some(sender) = self.imp().sender_to_bg.get() {
                if let Some(path) = player
                    .current_song_cover_path(true)
                    .map_or(None, |path| if path.exists() {Some(path)} else {None})
                {
                    let settings = settings_manager().child("ui");
                    let config = BlurConfig {
                        width: self.width() as u32,
                        height: self.height() as u32,
                        radius: settings.uint("bg-blur-radius"),
                        fade: true, // new image, must fade
                    };
                    let _ = sender.send_blocking(WindowMessage::NewBackground(path, config));
                } else {
                    let _ = sender.send_blocking(WindowMessage::ClearBackground);
                    self.imp().push_tex(None, true);
                }
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

    fn bind_state(&self) {
        // Bind client state to app name widget
        let client = self.downcast_application().get_client();
        let state = client.get_client_state();
        let title = self.imp().title.get();
        let spinner = self.imp().busy_spinner.get();
        state
            .bind_property("connection-state", &title, "subtitle")
            .transform_to(|_, state: ConnectionState| match state {
                ConnectionState::Connecting => Some("Connecting"),
                ConnectionState::Unauthenticated
                | ConnectionState::WrongPassword
                | ConnectionState::CredentialStoreError => Some("Unauthenticated"),
                ConnectionState::Connected => Some("Connected"),
                _ => Some("Not connected")
            })
            .sync_create()
            .build();

        state
            .bind_property("busy", &spinner, "visible")
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

            // Stop blur thread when closing window.
            // We need to take care of this now that the app's lifetime is decoupled from the window
            // (background running support)
            if window.imp().bg_handle.get().is_some() {
                window.imp().sender_to_bg.get().unwrap().send_blocking(WindowMessage::Stop).expect("Could not stop background blur thread");
            }

            window.downcast_application().on_window_closed();

            // TODO: persist other settings at closing?
            glib::Propagation::Proceed
        });
    }
}
