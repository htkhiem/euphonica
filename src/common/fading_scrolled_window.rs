use adw::prelude::*;
use gtk::{
    CompositeTemplate, gdk,
    glib::{self, Properties, clone, derived_properties},
    graphene, gsk,
    prelude::*,
    subclass::prelude::*,
};
use std::cell::{Cell, RefCell};

use crate::cache::placeholders::{ALBUMART_PLACEHOLDER, ALBUMART_THUMBNAIL_PLACEHOLDER};

use super::ImageState;

fn maybe_play<T: IsA<adw::Animation>>(anim: &T) {
    if anim.state() != adw::AnimationState::Playing {
        anim.play();
    }
}

// Maximum fade width, relative to fade axis. Actual width depends on how close the scroll is to that end.
static FADE_WIDTH: f32 = 0.2;

// Thin wrapper around a normal ScrolledWindow to fade the
mod imp {
    use super::*;

    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::FadingScrolledWindow)]
    pub struct FadingScrolledWindow {
        #[property(get, set = Self::set_inner)]
        pub inner: RefCell<Option<gtk::ScrolledWindow>>,
        // For simplicity, only implement the above fade-out in one axis.
        #[property(get, set)]
        pub vertical: Cell<bool>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for FadingScrolledWindow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaFadingScrolledWindow";
        type Type = super::FadingScrolledWindow;
        type ParentType = gtk::Widget;
    }

    #[derived_properties]
    impl ObjectImpl for FadingScrolledWindow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for FadingScrolledWindow {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            self.inner
                .borrow()
                .as_ref()
                .map_or(gtk::SizeRequestMode::ConstantSize, |inner| {
                    inner.request_mode()
                })
        }
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            self.inner
                .borrow()
                .as_ref()
                .map_or((0, 0, 0, 0), |inner| inner.measure(orientation, for_size))
        }

        fn size_allocate(&self, w: i32, h: i32, baseline: i32) {
            if let Some(inner) = self.inner.borrow().as_ref() {
                inner.size_allocate(&gtk::Allocation::new(0, 0, w, h), baseline);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(inner) = self.inner.borrow().as_ref() {
                let (w, h) = (self.obj().width(), self.obj().height());
                let bounds = graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
                let (hadj, vadj) = (inner.hadjustment(), inner.vadjustment());
                let start_point = graphene::Point::new(0.0, 0.0);
                let end_point: graphene::Point;
                // Construct in one pass the gradient for the whole axis. 
                // This avoid having to stack two gradients.
                let mut stops = Vec::with_capacity(4);
                if self.vertical.get() {
                    end_point = graphene::Point::new(0.0, h as f32);
                    if vadj.value() > 0.0 {
                        // Not at top => fade top out
                        stops.push(
                            gsk::ColorStop::new(0.0, gdk::RGBA::BLACK.with_alpha(0.0))
                        );
                        stops.push(
                            gsk::ColorStop::new(
                                (vadj.value() as f32 / h as f32).min(FADE_WIDTH),
                                gdk::RGBA::BLACK,
                            )
                        );
                    }
                    let lower_pos = vadj.value() + vadj.page_size();
                    if lower_pos < vadj.upper() {
                        // Not at bottom => fade bottom out
                        stops.push(
                            gsk::ColorStop::new(
                                (lower_pos as f32 / vadj.upper() as f32).max(1.0 - FADE_WIDTH),
                                gdk::RGBA::BLACK,
                            )
                        );
                        stops.push(
                            gsk::ColorStop::new(1.0, gdk::RGBA::BLACK.with_alpha(0.0))
                        );
                    }
                } else {
                    end_point = graphene::Point::new(w as f32, 0.0);
                    if hadj.value() > 0.0 {
                        // Not at top => fade top out
                        stops.push(
                            gsk::ColorStop::new(0.0, gdk::RGBA::BLACK.with_alpha(0.0))
                        );
                        stops.push(
                            gsk::ColorStop::new(
                                (hadj.value() as f32 / w as f32).min(FADE_WIDTH),
                                gdk::RGBA::BLACK,
                            )
                        );
                    }
                    let rightmost_pos = hadj.value() + hadj.page_size();
                    if rightmost_pos < hadj.upper() {
                        // Not at bottom => fade bottom out
                        stops.push(
                            gsk::ColorStop::new(
                                (rightmost_pos as f32 / hadj.upper() as f32).max(1.0 - FADE_WIDTH),
                                gdk::RGBA::BLACK,
                            )
                        );
                        stops.push(
                            gsk::ColorStop::new(1.0, gdk::RGBA::BLACK.with_alpha(0.0))
                        );
                    }
                }
                if !stops.is_empty() {
                    snapshot.push_mask(gsk::MaskMode::Alpha);
                    snapshot.append_linear_gradient(
                        &bounds,
                        &start_point,
                        &end_point,
                        &stops,
                    );
                    // Write mask
                    snapshot.pop();
                }
                self.obj().snapshot_child(inner, snapshot);
                if !stops.is_empty() {
                    // Blend
                    snapshot.pop();
                }
            }
        }
    }

    impl FadingScrolledWindow {
        fn set_inner(&self, widget: gtk::ScrolledWindow) {
            let obj = self.obj();
            let parent = obj.upcast_ref::<gtk::Widget>();
            widget.set_parent(parent);
            if let Some(old_widget) = self.inner.borrow_mut().replace(widget) {
                old_widget.unparent();
            }
        }
    }
}

glib::wrapper! {
    pub struct FadingScrolledWindow(ObjectSubclass<imp::FadingScrolledWindow>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FadingScrolledWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl FadingScrolledWindow {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn get_inner(&self) -> Option<gtk::ScrolledWindow> {
        self.imp().inner.borrow().as_ref().cloned()
    }
}
