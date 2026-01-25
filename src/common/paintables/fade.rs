use gtk::{
    gdk::{self, prelude::*, subclass::paintable::*},
    glib::{self, Properties},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::{Cell, RefCell};

// Background paintable implementation.
// Euphonica can optionally use the currently-playing track's album art as its
// background. This is always scaled to fill the whole window and can be further
// blurred. When the next song has a different album art, a fade animation will
// be played.
mod imp {
    use super::*;

    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::FadePaintable)]
    pub struct FadePaintable {
        pub current: RefCell<Option<gdk::MemoryTexture>>,
        pub previous: RefCell<Option<gdk::MemoryTexture>>,
        // 0 = previous, 0.5 = halfway, 1.0 = current
        pub fade: Cell<f64>,
        // Kept here so snapshot() does not have to query GSettings on every frame
        #[property(get, set)]
        pub transition_duration: Cell<f64>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FadePaintable {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaFadePaintable";
        type Type = super::FadePaintable;
        type Interfaces = (gdk::Paintable,);
    }

    #[glib::derived_properties]
    impl ObjectImpl for FadePaintable {
        fn dispose(&self) {
            println!("Disposing FadePaintable");
        }
    }

    impl PaintableImpl for FadePaintable {
        fn current_image(&self) -> gdk::Paintable {
            if let Some(current) = self.current.borrow().as_ref() {
                current.current_image()
            } else {
                gdk::Paintable::new_empty(1, 1)
            }
        }

        fn intrinsic_width(&self) -> i32 {
            if let Some(current) = self.current.borrow().as_ref() {
                current.intrinsic_width()
            } else {
                1
            }
        }

        fn intrinsic_height(&self) -> i32 {
            if let Some(current) = self.current.borrow().as_ref() {
                current.intrinsic_height()
            } else {
                1
            }
        }

        fn intrinsic_aspect_ratio(&self) -> f64 {
            if let Some(current) = self.current.borrow().as_ref() {
                current.intrinsic_aspect_ratio()
            } else {
                1.0
            }
        }

        fn snapshot(&self, snapshot: &gdk::Snapshot, width: f64, height: f64) {
            // "Fade" is defined as the progress of the transition from the previous to the current texture.
            // A value of 0.0 indicates that only the previous texture is visible, while 1.0 means only the
            // current texture is visible.
            // Fading procedure:
            // 1. Check if there's a current texture. If there is:
            //     1a. If there is no previous texture and fade != 1.0, then we're fading in from nothing.
            //     Draw the current picture at fade opacity.
            //     1b. Else (there is a previous texture and/or fade is at 1.0), just draw at full opacity.
            // 2. Check if there's a previous texture. If there is and fade < 1.0, draw it at 1-fade opacity.
            let fade = self.fade.get();
            let current = self.current.borrow();
            let previous = self.previous.borrow();
            if current.is_some() {
                if !previous.is_some() && fade < 1.0 {
                    snapshot.push_opacity(fade);
                    current.as_ref().unwrap().snapshot(snapshot, width, height);
                    snapshot.pop();
                } else {
                    current.as_ref().unwrap().snapshot(snapshot, width, height);
                }
            }
            if previous.is_some() && fade < 1.0 {
                snapshot.push_opacity(1.0 - fade);
                previous.as_ref().unwrap().snapshot(snapshot, width, height);
                snapshot.pop();
            }
        }
    }
}

glib::wrapper! {
    pub struct FadePaintable(ObjectSubclass<imp::FadePaintable>) @implements gdk::Paintable;
}

impl FadePaintable {
    pub fn get_fade(&self) -> f64 {
        self.imp().fade.get()
    }

    pub fn set_fade(&self, new: f64) {
        let true_new = new.clamp(0.0, 1.0);
        let old = self.imp().fade.replace(true_new);
        if old != true_new {
            self.invalidate_contents();
        }
    }

    pub fn set_new_content(&self, new: Option<gdk::MemoryTexture>) {
        self.imp().previous.replace(self.imp().current.take());
        self.imp().current.replace(new);
        self.set_fade(0.0);
    }

    /// Returns whether this paintable will paint anything on the next snapshot().
    /// (for example, it won't create any render node with no content set, or after
    /// having fully faded to nothing).
    pub fn will_paint(&self) -> bool {
        let current_has_content = self.imp().current.borrow().is_some();
        let previous_has_content = self.imp().previous.borrow().is_some();
        let fade = self.get_fade();
        (current_has_content && fade > 0.0) || (previous_has_content && fade < 1.0)
    }
}

impl Default for FadePaintable {
    fn default() -> Self {
        glib::Object::new()
    }
}
