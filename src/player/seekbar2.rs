use gtk::{CompositeTemplate, glib, graphene, gdk, gsk, prelude::*, subclass::prelude::*};
use glib::{Object, ParamSpec, ParamSpecDouble, ParamSpecObject, WeakRef, clone};
use once_cell::sync::Lazy;
use std::cell::Cell;
use hsl;
use crate::{common::QualityGrade, utils};

use super::Player;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/seekbar2.ui")]
    pub struct Seekbar {
        pub adjustment: gtk::Adjustment,
        #[template_child]
        pub elapsed: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration: TemplateChild<gtk::Label>,
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub format_desc: TemplateChild<gtk::Label>,
        #[template_child]
        pub bitrate: TemplateChild<gtk::Label>,
        pub seekbar_clicked: Cell<bool>,
        pub old_pos: Cell<f64>,
        pub player: WeakRef<Player>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for Seekbar {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaSeekbar2";
        type Type = super::Seekbar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Seekbar {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Connect gesture signals
            let drag = gtk::GestureDrag::new();

            drag.connect_drag_begin(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, x, _| {
                    this.seekbar_clicked.set(true);
                    this.translate_to_adjustment(x, false);
                    this.old_pos.set(this.adjustment.value());
                }
            ));

            drag.connect_drag_update(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, x, _| {
                    this.translate_to_adjustment(x, true);
                }
            ));

            drag.connect_drag_end(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, x, _| {
                    if this.seekbar_clicked.get() {
                        this.seekbar_clicked.set(false);
                        this.translate_to_adjustment(x, true);
                        if let Some(player) = this.player.upgrade() {
                            let new_pos = this.adjustment.value();
                            glib::spawn_future_local(async move {
                                if let Err(e) = player.send_seek(new_pos).await {
                                    dbg!(e);
                                }
                            });
                        }
                    }
                }
            ));

            obj.add_controller(drag);

            self.adjustment
                .bind_property("value", &self.elapsed.get(), "label")
                .transform_to(|_, pos| Some(utils::format_secs_as_duration(pos)))
                .sync_create()
                .build();

            self.adjustment
                .bind_property("upper", &self.duration.get(), "label")
                .transform_to(|_, dur: f64| {
                    if dur > 0.0 {
                        return Some(utils::format_secs_as_duration(dur));
                    }
                    Some("--:--".to_owned())
                })
                .sync_create()
                .build();
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecObject::builder::<gtk::Adjustment>("adjustment")
                        .read_only()
                        .build(),
                    ParamSpecDouble::builder("position").build(),
                    ParamSpecDouble::builder("duration").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "adjustment" => self.adjustment.to_value(),
                "position" => obj.position().to_value(),
                "duration" => obj.duration().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "position" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_position(v);
                    }
                }
                "duration" => {
                    if let Ok(v) = value.get::<f64>() {
                        obj.set_duration(v);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for Seekbar {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            // Get the current accent colour via this object's foreground. This is possible as
            // we've set it to the fg-auto-accent CSS class, whose foreground colour is set to
            // the current accent (either system or picked from album art) by EuphonicaWindow.
            let style= adw::StyleManager::default();
            let accent = self.obj().color();
            let cursor_x = (self.adjustment.value() / self.adjustment.upper()) as f32 * self.obj().width() as f32;

            // Draw highlight
            let mut bottom_hsl = hsl::HSL::from_rgb(
                &[(accent.red() * 255.0).round() as u8, 
                (accent.green() * 255.0).round() as u8, 
                (accent.blue() * 255.0).round() as u8]
            );
            bottom_hsl.l = bottom_hsl.l.max(0.75);
            let bottom = bottom_hsl.to_rgb();
            let bottom = gdk::RGBA::new(bottom.0 as f32 / 255.0, bottom.1 as f32 / 255.0, bottom.2 as f32 / 255.0, 1.0);
            let stops = if style.is_dark() {
                // In dark mode, the seekbar highlight glows the accent colour and the cursor glows white.
                [
                    gsk::ColorStop::new(0.0, bottom),
                    gsk::ColorStop::new(0.15, accent.with_alpha(0.7)),
                    gsk::ColorStop::new(0.3, accent.with_alpha(0.4)),
                    gsk::ColorStop::new(0.75, accent.with_alpha(0.0))
                ]
            } else {
                [
                    gsk::ColorStop::new(0.0, bottom),
                    gsk::ColorStop::new(0.15, accent.with_alpha(0.5)),
                    gsk::ColorStop::new(0.3, accent.with_alpha(0.3)),
                    gsk::ColorStop::new(0.75, accent.with_alpha(0.0)),
                ]
            };
            snapshot.append_linear_gradient(
                &graphene::Rect::new(
                    0.0,
                    0.0,
                    cursor_x,
                    self.obj().height() as f32,
                ), 
                &graphene::Point::new(0.0, self.obj().height() as f32), 
                &graphene::Point::new(0.0, 0.0), 
                &stops
            );
            // FIXME: without a "solid colour" node the repainting will be erratic.
            // This call does nothing visually & wastes GPU cycles but without it the seekbar 
            // won't redraw itself.
            snapshot.append_color(
                &gdk::RGBA::TRANSPARENT,
                &graphene::Rect::new(
                    0.0,
                    0.0,
                    cursor_x,
                    self.obj().height() as f32,
                )
            );

            // Draw cursor
            let cursor_stops = if style.is_dark() {[
                gsk::ColorStop::new(0.25, gdk::RGBA::WHITE),
                gsk::ColorStop::new(1.0, gdk::RGBA::WHITE.with_alpha(0.0))
            ]} else {[
                gsk::ColorStop::new(0.25, accent),
                gsk::ColorStop::new(1.0, accent.with_alpha(0.0))
            ]};
            snapshot.append_linear_gradient(
                &graphene::Rect::new(
                    // 1px wide cursor
                    cursor_x - 1.0,
                    0.0,
                    1.0,
                    self.obj().height() as f32,
                ), 
                &graphene::Point::new(0.0, self.obj().height() as f32), 
                &graphene::Point::new(0.0, 0.0), 
                &cursor_stops
            );

            self.parent_snapshot(snapshot);
        }
    }

    impl BoxImpl for Seekbar {}

    impl Seekbar {
        /// Figure out which value to set the internal GtkAdjustment
        /// to given the location of the mouse or finger relative to this widget.
        fn translate_to_adjustment(&self, x: f64, offset: bool) {
            let obj = self.obj();
            let full_width = (obj.width() as f64).max(1.0);
            let upper = self.adjustment.upper();
            self.adjustment.set_value(if offset {
                /* println!(
                    "old_pos: {}, x: {}, x / full_width: {}, result: {}", 
                    self.old_pos.get(), 
                    x, 
                    (x / full_width).max(-1.0).min(1.0), self.old_pos.get() + upper * (x / full_width).min(-1.0).max(1.0)); */
                self.old_pos.get() + upper * (x / full_width).max(-1.0).min(1.0)
            } else {
                upper * (x / full_width).max(0.0).min(1.0)
            });
            self.obj().idle_queue_draw();
        }
    }
}

glib::wrapper! {
    pub struct Seekbar(ObjectSubclass<imp::Seekbar>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for Seekbar {
    fn default() -> Self {
        Self::new()
    }
}

impl Seekbar {
    fn idle_queue_draw(&self) {
        glib::idle_add_local_once(clone!(#[weak(rename_to = this)] self, move || {
            this.queue_draw();
        }));
    }

    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn position(&self) -> f64 {
        self.imp().adjustment.value()
    }

    /// Will have no effect while seekbar is being held by the user
    pub fn set_position(&self, new: f64) {
        if !self.imp().seekbar_clicked.get() {
            self.imp().adjustment.set_value(new);
            self.idle_queue_draw();
        }
    }

    pub fn duration(&self) -> f64 {
        self.imp().adjustment.upper()
    }

    pub fn set_duration(&self, new: f64) {
        self.imp().adjustment.set_lower(0.0);
        self.imp().adjustment.set_upper(new);
    }

    pub fn adjustment(&self) -> &gtk::Adjustment {
        self.imp().adjustment.as_ref()
    }

    pub fn setup(&self, player: &Player) {
        player
            .bind_property("position", self, "position")
            .sync_create()
            .build();

        player
            .bind_property("duration", self, "duration")
            .sync_create()
            .build();

        player
            .bind_property(
                "quality-grade",
                &self.imp().quality_grade.get(),
                "icon-name",
            )
            .transform_to(|_, grade: QualityGrade| Some(grade.to_icon_name()))
            .sync_create()
            .build();

        player
            .bind_property("format-desc", &self.imp().format_desc.get(), "label")
            .sync_create()
            .build();

        player
            .bind_property("bitrate", &self.imp().bitrate.get(), "label")
            .transform_to(|_, val: u32| Some(utils::format_bitrate(val)))
            .sync_create()
            .build();

        self.imp().player.set(Some(player));
    }
}
