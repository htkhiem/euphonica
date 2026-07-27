use gtk::{
    gdk::{self, prelude::*, subclass::paintable::*},
    glib::{self, Properties},
    graphene, gsk,
    prelude::*,
    subclass::prelude::*,
};
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::RotatingPaintable)]
    pub struct RotatingPaintable {
        pub paintable: RefCell<Option<gdk::Paintable>>,
        pub rotation: Cell<f64>,
        pub duration: Cell<f64>,
        #[property(get, set = Self::set_circular)]
        pub circular: Cell<bool>,
        #[property(get, set)]
        pub degrees_per_second: Cell<f64>,
        #[property(get, set)]
        pub return_to_starting_angle: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RotatingPaintable {
        const NAME: &'static str = "EuphonicaRotatingPaintable";
        type Type = super::RotatingPaintable;
        type Interfaces = (gdk::Paintable,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for RotatingPaintable {}

    impl PaintableImpl for RotatingPaintable {
        fn current_image(&self) -> gdk::Paintable {
            let current = super::RotatingPaintable::new();
            if let Some(paintable) = self.paintable.borrow().as_ref() {
                current.set_paintable(Some(&paintable.current_image()));
            }
            current.set_rotation(self.rotation.get());
            current.set_circular(self.circular.get());
            current.upcast()
        }

        fn intrinsic_width(&self) -> i32 {
            self.intrinsic_size(|paintable| paintable.intrinsic_width())
        }

        fn intrinsic_height(&self) -> i32 {
            self.intrinsic_size(|paintable| paintable.intrinsic_height())
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            if self.circular.get() {
                1.0
            } else {
                self.paintable
                    .borrow()
                    .as_ref()
                    .map_or(1.0, |paintable| paintable.intrinsic_aspect_ratio())
            }
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            let paintable = self.paintable.borrow();
            let Some(paintable) = paintable.as_ref() else {
                return;
            };

            if !self.circular.get() {
                paintable.snapshot(snapshot, width, height);
                return;
            }

            // Inset the disc to prevent clipping
            let diameter = width.min(height) * 0.96;
            if diameter <= 0.0 {
                return;
            }

            let bounds = graphene::Rect::new(
                ((width - diameter) / 2.0) as f32,
                ((height - diameter) / 2.0) as f32,
                diameter as f32,
                diameter as f32,
            );
            let clip = gsk::RoundedRect::from_rect(bounds, diameter as f32 / 2.0);
            snapshot.push_rounded_clip(&clip);

            let source_ratio = paintable.intrinsic_aspect_ratio();
            let (paint_width, paint_height) = if source_ratio.is_finite() && source_ratio > 0.0 {
                if source_ratio >= 1.0 {
                    (diameter * source_ratio, diameter)
                } else {
                    (diameter, diameter / source_ratio)
                }
            } else {
                (diameter, diameter)
            };

            snapshot.save();
            let center = graphene::Point::new(width as f32 / 2.0, height as f32 / 2.0);
            snapshot.translate(&center);
            snapshot.rotate(self.rotation.get() as f32);
            snapshot.translate(&graphene::Point::new(
                (-paint_width / 2.0) as f32,
                (-paint_height / 2.0) as f32,
            ));
            paintable.snapshot(snapshot, paint_width, paint_height);

            snapshot.restore();
            snapshot.pop();
        }
    }

    impl RotatingPaintable {
        fn set_circular(&self, circular: bool) {
            if self.circular.replace(circular) != circular {
                self.obj().invalidate_size();
                self.obj().invalidate_contents();
            }
        }

        fn intrinsic_size(&self, get: impl FnOnce(&gdk::Paintable) -> i32) -> i32 {
            self.paintable.borrow().as_ref().map_or(1, |paintable| {
                if self.circular.get() {
                    // A zero width/height means that dimension is unavailable
                    let width = paintable.intrinsic_width();
                    let height = paintable.intrinsic_height();
                    match (width > 0, height > 0) {
                        (true, true) => width.min(height),
                        (true, false) => width,
                        (false, true) => height,
                        (false, false) => 1,
                    }
                } else {
                    get(paintable)
                }
            })
        }
    }
}

glib::wrapper! {
    pub struct RotatingPaintable(ObjectSubclass<imp::RotatingPaintable>) @implements gdk::Paintable;
}

impl RotatingPaintable {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn set_paintable(&self, paintable: Option<&impl IsA<gdk::Paintable>>) {
        let paintable = paintable.map(|paintable| paintable.as_ref().clone());
        if *self.imp().paintable.borrow() == paintable {
            return;
        }
        self.imp().paintable.replace(paintable);
        self.invalidate_size();
        self.invalidate_contents();
    }

    pub fn set_duration(&self, duration: f64) {
        self.imp().duration.set(duration);
    }

    pub fn rotation_speed(&self) -> f64 {
        let speed = self.degrees_per_second();
        let duration = self.imp().duration.get();
        if self.return_to_starting_angle() && duration > 0.0 {
            let rotations = (duration * speed / 360.0).round().max(1.0);
            rotations * 360.0 / duration
        } else {
            speed
        }
    }

    // Don't make property as it changes every frame
    pub fn rotation(&self) -> f64 {
        self.imp().rotation.get()
    }

    pub fn set_rotation(&self, rotation: f64) {
        if self.imp().rotation.replace(rotation) != rotation {
            self.invalidate_contents();
        }
    }
}

impl Default for RotatingPaintable {
    fn default() -> Self {
        Self::new()
    }
}
