use gtk::{
    CompositeTemplate, gdk,
    glib::{self, Object},
    prelude::*,
    subclass::prelude::*,
};
use std::{cell::Cell, f32::consts::PI};

use crate::cache::placeholders::{ALBUMART_PLACEHOLDER, ALBUMART_THUMBNAIL_PLACEHOLDER};

// As soon as a cell comes within this close of the render area, treat it as
// visible & load album art early to avoid showing loading spinners.
static FAN_ANGLE: f32 = 11.25;

/// Widget that shows up to three images in a fan-like arrangement.
/// Design:
/// If no example album is given, simply draw a centered avatar.
/// If one album is given: draw that album's cover behind the avatar, now reduced to half-size and aligned to the bottom middle.
/// If two: rotate the covers 12.5deg to either sides; the album on the right is drawn on top (like a fan of playing cards).
/// If three or more: same as above, with an unrotated middle album cover. Only show up to three cover arts.

mod imp {
    use gtk::graphene;

    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/cover-fan.ui")]
    pub struct CoverFan {
        #[template_child]
        pub cover1_card: TemplateChild<gtk::Box>, // left
        #[template_child]
        pub cover1: TemplateChild<gtk::Picture>, // left
        #[template_child]
        pub cover2_card: TemplateChild<gtk::Box>, // mid or right
        #[template_child]
        pub cover2: TemplateChild<gtk::Picture>, // mid or right
        #[template_child]
        pub cover3_card: TemplateChild<gtk::Box>, // left
        #[template_child]
        pub cover3: TemplateChild<gtk::Picture>, // right
        pub cover_count: Cell<u8>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for CoverFan {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaCoverFan";
        type Type = super::CoverFan;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for CoverFan {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.obj().clear_cover(0, true);
            self.obj().clear_cover(1, true);
            self.obj().clear_cover(2, true);
        }
    }

    impl WidgetImpl for CoverFan {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            if orientation == gtk::Orientation::Vertical {
                (for_size.max(1), for_size.max(1), -1, -1)
            } else {
                (for_size.max(1), for_size.max(1), 0, 0)
            }
            
        }

        fn size_allocate(&self, w: i32, h: i32, baseline: i32) {
            // Depending on how many example arts we're given
            let edge = w.min(h);
            match self.cover_count.get() {
                0 | 1 => {
                    // Sole art => 85% short edge
                    let art_edge = (edge as f32 * 0.85).floor() as i32;
                    self.cover1_card
                        .get()
                        .size_allocate(&gtk::Allocation::new(0, 0, art_edge, art_edge), baseline);
                }
                2 => {
                    let art_edge = (edge as f32 * 0.72).floor() as i32;
                    self.cover1_card
                        .get()
                        .size_allocate(&gtk::Allocation::new(0, 0, art_edge, art_edge), baseline);
                    self.cover2_card
                        .get()
                        .size_allocate(&gtk::Allocation::new(0, 0, art_edge, art_edge), baseline);
                }
                _ => {
                    let art_edge = (edge as f32 * 0.72).floor() as i32;
                    self.cover1_card
                        .get()
                        .size_allocate(&gtk::Allocation::new(0, 0, art_edge, art_edge), baseline);
                    self.cover2_card
                        .get()
                        .size_allocate(&gtk::Allocation::new(0, 0, art_edge, art_edge), baseline);
                    self.cover3_card
                        .get()
                        .size_allocate(&gtk::Allocation::new(0, 0, art_edge, art_edge), baseline);
                }
            };
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let w = obj.width() as f32;
            let h = obj.height() as f32;
            let edge = w.min(h);
            // Actual draw pos will depend on transformation of the snapshot coordinate system
            // Left art's top-left corner's y-offset versus origin
            // Compile time eval should fire on these trigs
            let a = 0.72 * edge * (FAN_ANGLE / 180.0 * PI).sin();
            // Right art's top-left corner's x-offset versus origin
            let b = edge * (1.0 - 0.72 * (FAN_ANGLE / 180.0 * PI).cos());
            // Middle art's top-left corner's x-offset versus origin (when all 3 are shown)
            let c = edge * (1.0 - 0.72) / 2.0;
            // eprintln!("a={}, b={}, c={}", a, b, c);
            match self.cover_count.get() {
                0 | 1 => {
                    // Sole art
                    snapshot.translate(&graphene::Point::new(edge * (1.0 - 0.85) / 2.0, 0.0));
                    obj.snapshot_child(&self.cover1_card.get(), snapshot);
                    // Back to old 0.0
                    // snapshot.translate(&graphene::Point::new(edge * (1.0 - 0.85) / 2.0, 0.0));
                }
                2 => {
                    // Cover 1 to the left and behind
                    snapshot.translate(&graphene::Point::new(0.0, a));
                    snapshot.rotate(-FAN_ANGLE);
                    obj.snapshot_child(&self.cover1_card.get(), snapshot);
                    snapshot.rotate(FAN_ANGLE);

                    // Cover 2 to the right and in front
                    snapshot.translate(&graphene::Point::new(b, -a));
                    snapshot.rotate(FAN_ANGLE);
                    obj.snapshot_child(&self.cover2_card.get(), snapshot);
                    // snapshot.rotate(-FAN_ANGLE);
                    // snapshot.translate(&graphene::Point::new(-b, 0.0));
                }
                _ => {
                    // Cover 1 to the left and behind
                    snapshot.translate(&graphene::Point::new(0.0, a));
                    snapshot.rotate(-FAN_ANGLE);
                    obj.snapshot_child(&self.cover1_card.get(), snapshot);
                    snapshot.rotate(FAN_ANGLE);

                    // Cover 2 in the middle and unrotated
                    snapshot.translate(&graphene::Point::new(c, -a));
                    obj.snapshot_child(&self.cover2_card.get(), snapshot);

                    // Cover 3 to the right and in front
                    snapshot.translate(&graphene::Point::new(b - c, 0.0));
                    snapshot.rotate(FAN_ANGLE);
                    obj.snapshot_child(&self.cover3_card.get(), snapshot);
                    // snapshot.rotate(-FAN_ANGLE);
                    // snapshot.translate(&graphene::Point::new(-b, 0.0));
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct CoverFan(ObjectSubclass<imp::CoverFan>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl CoverFan {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn set_cover_count(&self, count: u8) {
        self.imp().cover_count.set(count.min(3));
    }

    pub fn cover_count(&self) -> u8 {
        self.imp().cover_count.get()
    }

    pub fn set_cover(&self, index: u8, paintable: &impl IsA<gdk::Paintable>) {
        match index {
            0 => {
                self.imp().cover1.set_paintable(Some(paintable));
            }
            1 => {
                self.imp().cover2.set_paintable(Some(paintable));
            }
            2 => {
                self.imp().cover3.set_paintable(Some(paintable));
            }
            _ => {}
        }
    }

    pub fn clear_cover(&self, index: u8, thumb: bool) {
        let placeholder = Some(if thumb {
            &*ALBUMART_THUMBNAIL_PLACEHOLDER
        } else {
            &*ALBUMART_PLACEHOLDER
        });
        match index {
            0 => {
                self.imp().cover1.set_paintable(placeholder);
            }
            1 => {
                self.imp().cover2.set_paintable(placeholder);
            }
            2 => {
                self.imp().cover3.set_paintable(placeholder);
            }
            _ => {}
        }
    }
}
