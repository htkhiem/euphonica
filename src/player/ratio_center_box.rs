use gtk::{glib, prelude::*, subclass::prelude::*};
use std::cell::{Cell, RefCell};

mod imp {
    use glib::Properties;

    use super::*;

    /// A fixed-ratio version of GtkCenterBox.
    /// Given a centre ratio of C (0 < C < 1), this version always assigns
    /// the centre widget C*100% of its width, centering it horizontally if
    /// the centre widget does not expand to the allocated width. The left(right)
    /// side always gets (1-C)/2*100% of the total width, and will be left-(right-)
    /// aligned if they do not use up all the allocated width. All widgets will
    /// be vertically top-aligned.
    ///
    /// This widget follows height-for-width sizing with the following rules:
    /// - Minimum or natural width is the center widget's minimum or natural width divided by C, rounded up
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
        #[property(get, set)]
        pub center_ratio: Cell<f32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RatioCenterBox {
        const NAME: &'static str = "EuphonicaRatioCenterBox";
        type Type = super::RatioCenterBox;
        type ParentType = gtk::Widget;

        fn class_init(_: &mut Self::Class) {}
    }

    #[glib::derived_properties]
    impl ObjectImpl for RatioCenterBox {}

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
            let center_ratio = self.center_ratio.get();
            if orientation == gtk::Orientation::Horizontal {
                (
                    (m_center.0 as f32 / center_ratio).ceil() as i32,
                    (m_center.0 as f32 / center_ratio).ceil() as i32,
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
            let w_center = (w as f32 * self.center_ratio.get()).ceil() as i32;
            let w_left = ((w - w_center) as f32 / 2.0).ceil() as i32;
            let w_right = w - w_left - w_center;
            // Allocate C*100% to the centre
            if let Some(widget) = self.center_widget.borrow().as_ref() {
                widget.size_allocate(&gtk::Allocation::new(w_left + 1, 0, w_center, h), baseline);
            }
            if let Some(widget) = self.left_widget.borrow().as_ref() {
                widget.size_allocate(&gtk::Allocation::new(0, 0, w_left, h), baseline);
            }
            if let Some(widget) = self.right_widget.borrow().as_ref() {
                widget.size_allocate(&gtk::Allocation::new(w_left + w_center + 1, 0, w_right, h), baseline);
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
        @implements gio::ActionGroup, gio::ActionMap, gtk::Buildable;
}

impl Default for RatioCenterBox {
    fn default() -> Self {
        glib::Object::new()
    }
}
