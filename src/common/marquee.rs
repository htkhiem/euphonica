use adw::prelude::*;
use gtk::{gio, glib, graphene, subclass::prelude::*};
use std::cell::Cell;

mod imp {
    use super::*;
    use adw::TimedAnimation;
    use glib::{clone, Properties};
    use gtk::CompositeTemplate;
    use std::cell::OnceCell;

    #[derive(Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::Marquee)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/marquee.ui")]
    pub struct Marquee {
        #[template_child]
        pub child: TemplateChild<gtk::Label>,
        #[property(get, set)]
        pub speed: Cell<f64>, // in pixels per second.
        animation: OnceCell<TimedAnimation>,
        curr_offset: Cell<f64>,
        child_width: Cell<i32>,
        #[property(get, set)]
        should_run: Cell<bool>,
    }
    impl Marquee {
        pub fn check_animation(&self) {
            if self.should_run.get() && self.child_width.get() > self.obj().width() {
                let anim = self.animation.get().unwrap();
                // println!("Child: {}, allocated: {}, should_run: {}", self.child_width.get(), self.obj().width(), self.should_run.get());
                if anim.state() != adw::AnimationState::Playing {
                    let _ = self.curr_offset.replace(0.0);
                    anim.play();
                }
            } else {
                self.animation.get().unwrap().reset();
                self.curr_offset.replace(0.0);
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Marquee {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaMarquee";
        type Type = super::Marquee;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for Marquee {
        fn constructed(&self) {
            self.parent_constructed();
            let anim_target = adw::CallbackAnimationTarget::new(clone!(
                #[weak(rename_to = this)]
                self,
                move |progress: f64| {
                    // Update render offset
                    // Reacquired on the fly to deal with changing child width
                    let child_width = this.child_width.get();
                    if child_width > 0 {
                        let allocated_width = this.obj().width();
                        let anim = this.animation.get().unwrap();
                        if child_width > allocated_width {
                            // Recomputed on the fly to deal with window resizing
                            let distance = (child_width - allocated_width) as f64;
                            let _ = this.curr_offset.replace(-distance * progress);
                            anim.set_duration((distance / this.speed.get() * 1000.0) as u32);
                            this.obj().queue_draw();
                        } else {
                            let _ = this.curr_offset.replace(0.0);
                        }
                    }
                }
            ));
            let anim = adw::TimedAnimation::new(
                self.obj().as_ref(),
                0.0,
                1.0,
                1000, // Default 1s duration until we have a child widget
                // (self.obj().width() as f64 / self.speed.get() * 1000.0).round() as u32,
                anim_target,
            );
            anim.set_easing(adw::Easing::EaseInOutSine);
            anim.set_repeat_count(0); // Repeat endlessly
            anim.set_alternate(true); // Back and forth
            let _ = self.animation.set(anim);
        }

        fn dispose(&self) {
            self.animation.get().unwrap().reset();
            self.child.get().unparent();  // GtkWidget doesn't do this for us, leading to all the console warnings
        }
    }

    impl WidgetImpl for Marquee {
        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            let child = self.child.get();
            let preferred_size = child.preferred_size().1;
            let natural_width = preferred_size.width();
            let _ = self.child_width.replace(natural_width);
            self.check_animation();

            // Allocate space for the child widget
            child.size_allocate(&gtk::Allocation::new(0, 0, width, height), baseline);
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            let min_width = self.obj().width_request();
            let child = self.child.get();
            // Measure the child's natural size in the given orientation
            let (min_size, natural_size, min_baseline, natural_baseline) =
                child.measure(orientation, for_size);

            // For horizontal orientation, override the label's min width
            if orientation == gtk::Orientation::Horizontal {
                return (
                    min_width,
                    natural_size.max(min_width),
                    min_baseline,
                    natural_baseline,
                );
            } else {
                return (
                    min_size,
                    natural_size.max(min_width),
                    min_baseline,
                    natural_baseline,
                );
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            snapshot.push_clip(&graphene::Rect::new(
                0.0,
                0.0,
                self.obj().width() as f32,
                self.obj().height() as f32,
            ));
            snapshot.translate(&graphene::Point::new(
                self.curr_offset.get() as f32,
                0.0,
            )); // Apply horizontal translation for sliding effect
            self.parent_snapshot(snapshot);
            snapshot.pop();
        }
    }
}

glib::wrapper! {
    pub struct Marquee(ObjectSubclass<imp::Marquee>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
}

/// Label but with a marquee effect when allocated less than its natural width.

impl Marquee {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn label(&self) -> gtk::Label {
        self.imp().child.get()
    }

    pub fn set_should_run_and_check(&self, should_run: bool) {
        self.set_should_run(should_run);
        self.imp().check_animation();
    }
}

impl Default for Marquee {
    fn default() -> Self {
        glib::Object::new()
    }
}
