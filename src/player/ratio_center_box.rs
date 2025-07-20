use gtk::{glib::{Properties}, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

mod imp {
    use super::*;

    /// A version of GtkCenterBox that tries to keep the centre widget always centered
    /// and allocates it at least its minimum width.

    /// This widget follows height-for-width sizing with the following rules:
    /// - Minimum or natural width is the sum of all child widgets' minimum or natural heights, rounded up.
    /// - Minimum or natural height is the maximum of the three widgets' minimum or natural heights.
    ///
    /// Minimum and natural baselines are not defined for this widget.
    #[derive(Default, Properties)]
    #[properties(wrapper_type = super::RatioCenterBox)]
    pub struct RatioCenterBox {
        #[property(get, set = Self::set_left_widget)]
        pub left_widget: RefCell<Option<gtk::Widget>>,
        #[property(get, set = Self::set_center_widget)]
        pub center_widget: RefCell<Option<gtk::Widget>>,
        #[property(get, set = Self::set_right_widget)]
        pub right_widget: RefCell<Option<gtk::Widget>>,

        #[property(get, set, default = 0.95)]
        pub max_center_ratio: Cell<f32>,
        pub nat_center_width: Cell<i32>,
        pub nat_side_width: Cell<i32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RatioCenterBox {
        const NAME: &'static str = "EuphonicaRatioCenterBox";
        type Type = super::RatioCenterBox;
        type ParentType = gtk::Widget;

        fn class_init(_: &mut Self::Class) {}
    }

    #[glib::derived_properties]
    impl ObjectImpl for RatioCenterBox {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for RatioCenterBox {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }
        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let m_left = if let Some(left_widget) = self.left_widget.borrow().as_ref() {
                left_widget.measure(orientation, for_size)
            } else {
                (0, 0, -1, -1)
            };
            let m_center = if let Some(center_widget) = self.center_widget.borrow().as_ref() {
                center_widget.measure(orientation, for_size)
            } else {
                (0, 0, -1, -1)
            };
            let m_right = if let Some(right_widget) = self.right_widget.borrow().as_ref() {
                right_widget.measure(orientation, for_size)
            } else {
                (0, 0, -1, -1)
            };
            if orientation == gtk::Orientation::Horizontal {
                self.nat_center_width.set(m_center.1);
                self.nat_side_width.set(m_left.1.max(m_right.1));
                (
                    (m_center.0 as f32 + m_left.0 as f32 + m_right.0 as f32).ceil() as i32,
                    (m_center.1 as f32 + m_left.1 as f32 + m_right.1 as f32).ceil() as i32,
                    -1,
                    -1,
                )
            } else {
                (
                    m_left.0.max(m_center.0).max(m_right.0),
                    m_left.1.max(m_center.1).max(m_right.1),
                    -1,
                    -1,
                )
            }
        }

        fn size_allocate(&self, w: i32, h: i32, baseline: i32) {
            let nat_side_width = self.nat_side_width.get();
            let min_center_width = self.nat_center_width.get().min((w as f32 * self.max_center_ratio.get()).floor() as i32);
            let available_center_width = w - 2 * nat_side_width;

            let final_side_width: i32;
            let final_center_width: i32;
            if available_center_width < min_center_width {
                // Shrink both sides symmetrically to keep centre widget centered
                final_side_width = ((w - min_center_width) as f32 / 2.0).floor() as i32;
                final_center_width = w - 2 * final_side_width;
            }
            else {
                final_side_width = nat_side_width;
                final_center_width = available_center_width;
            }

            if let Some(widget) = self.center_widget.borrow().as_ref() {
                widget.size_allocate(&gtk::Allocation::new(final_side_width + 1, 0, final_center_width, h), baseline);
            }
            if let Some(widget) = self.left_widget.borrow().as_ref() {
                widget.size_allocate(&gtk::Allocation::new(0, 0, final_side_width, h), baseline);
            }
            if let Some(widget) = self.right_widget.borrow().as_ref() {
                widget.size_allocate(&gtk::Allocation::new(final_side_width + final_center_width + 1, 0, final_side_width, h), baseline);
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();

            // Draw left side
            if let Some(widget) = self.left_widget.borrow().as_ref() {
                obj.snapshot_child(widget, snapshot);
            }

            // Draw centre
            if let Some(widget) = self.center_widget.borrow().as_ref() {
                // snapshot_child takes care of translation (uses information given by size_allocate)
                obj.snapshot_child(widget, snapshot);
            }

            // Draw right side
            if let Some(widget) = self.right_widget.borrow().as_ref() {
                // snapshot_child takes care of translation (uses information given by size_allocate)
                obj.snapshot_child(widget, snapshot);
            }
        }
    }

    impl RatioCenterBox {
        fn set_left_widget(&self, widget: gtk::Widget) {
            let obj = self.obj();
            let parent = obj.upcast_ref::<gtk::Widget>();
            widget.set_parent(parent);
            if let Some(old_widget) = self.left_widget.borrow_mut().replace(widget) {
                old_widget.unparent();
            }
        }

        fn set_center_widget(&self, widget: gtk::Widget) {
            let obj = self.obj();
            let parent = obj.upcast_ref::<gtk::Widget>();
            widget.set_parent(parent);
            if let Some(old_widget) = self.center_widget.borrow_mut().replace(widget) {
                old_widget.unparent();
            }
        }

        fn set_right_widget(&self, widget: gtk::Widget) {
            let obj = self.obj();
            let parent = obj.upcast_ref::<gtk::Widget>();
            widget.set_parent(parent);
            if let Some(old_widget) = self.right_widget.borrow_mut().replace(widget) {
                old_widget.unparent();
            }
        }
    }
}

glib::wrapper! {
    pub struct RatioCenterBox(ObjectSubclass<imp::RatioCenterBox>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for RatioCenterBox {
    fn default() -> Self {
        glib::Object::new()
    }
}
