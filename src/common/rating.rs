use duplicate::duplicate;
use adw::prelude::*;
use gtk::{glib, subclass::prelude::*};
use std::cell::Cell;

const FULL: &'static str = "star-large-symbolic";
const HALF: &'static str = "star-outline-half-left-symbolic";
const NONE: &'static str = "star-outline-rounded-symbolic";

mod imp {
    use std::sync::OnceLock;

    use super::*;
    use glib::{clone, subclass::Signal, Properties};
    use gtk::CompositeTemplate;

    #[derive(Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::Rating)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/rating.ui")]
    pub struct Rating {
        #[template_child]
        pub s1: TemplateChild<gtk::Image>,
        #[template_child]
        pub s2: TemplateChild<gtk::Image>,
        #[template_child]
        pub s3: TemplateChild<gtk::Image>,
        #[template_child]
        pub s4: TemplateChild<gtk::Image>,
        #[template_child]
        pub s5: TemplateChild<gtk::Image>,

        #[property(get, set = Self::set_value)]
        pub value: Cell<i8>,
        #[property(get, set)]
        pub dim_inactive: Cell<bool>,
        #[property(get, set)]
        pub icon_size: Cell<u8>,
        #[property(get, set)]
        pub editable: Cell<bool>,
        pub preview_value: Cell<i8>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Rating {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaRating";
        type Type = super::Rating;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for Rating {
        fn constructed(&self) {
            self.parent_constructed();

            duplicate!{
                [name; [s1]; [s2]; [s3]; [s4]; [s5];]
                self.obj()
                    .bind_property("icon-size", &self.name.get(), "icon-size")
                    .transform_to(|_, code: u8| match code {
                        0 => Some(gtk::IconSize::Inherit.to_value()),
                        1 => Some(gtk::IconSize::Normal.to_value()),
                        2 => Some(gtk::IconSize::Large.to_value()),
                        _ => None
                    })
                    .sync_create()
                    .build();
            }


            self.update_stars(self.value.get());

            let hover_ctl = gtk::EventControllerMotion::new();
            hover_ctl.connect_enter(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, x, y| {
                    if this.dim_inactive.get() && this.obj().has_css_class("dim-label") {
                        this.obj().remove_css_class("dim-label");
                    }
                    if this.editable.get() {
                        this.on_movement(x, y);
                    }
                }
            ));

            hover_ctl.connect_motion(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, x, y| {
                    if this.editable.get() {
                        this.on_movement(x, y);
                    }
                }
            ));

            hover_ctl.connect_leave(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if this.dim_inactive.get() && !this.obj().has_css_class("dim-label") {
                        this.obj().add_css_class("dim-label");
                    }
                    if this.editable.get() {
                        // Revert to displaying actual value
                        this.update_stars(this.value.get());
                    }
                }
            ));

            let click_ctl = gtk::GestureClick::new();
            click_ctl.connect_released(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, _, _| {
                    if this.editable.get() {
                        this.obj().set_value(this.preview_value.get());
                        // Further yell to let parent widgets know this change is user-initiated
                        this.obj().emit_by_name::<()>("changed", &[]);
                    }
                }
            ));
            self.obj().add_controller(hover_ctl);
            self.obj().add_controller(click_ctl);
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("changed")
                        .build() 
                ]
            })
        }
    }

    impl WidgetImpl for Rating {}

    impl BoxImpl for Rating {}

    impl Rating {
        fn set_value(&self, new: i8) {
            let old = self.value.replace(new);
            if old != new {
                self.update_stars(new);
                self.obj().notify("value");
            }
        }

        fn on_movement(&self, x: f64, _y: f64) {
            let new = (x / self.obj().width() as f64 * 10.0).round() as i8;
            let old = self.preview_value.replace(new);
            if old != new {
                // Since on_movement is only called when we're in preview mode,
                // always use preview value here.
                self.update_stars(new);
            }
        }

        // Can either be true value or preview value
        fn update_stars(&self, to_value: i8) {
            if to_value >= 2 {
                self.s1.set_icon_name(Some(FULL));
            }
            else if to_value == 1 {
                self.s1.set_icon_name(Some(HALF));
            }
            else {
                self.s1.set_icon_name(Some(NONE));
            }
            if to_value >= 4 {
                self.s2.set_icon_name(Some(FULL));
            }
            else if to_value == 3 {
                self.s2.set_icon_name(Some(HALF));
            }
            else {
                self.s2.set_icon_name(Some(NONE));
            }
            if to_value >= 6 {
                self.s3.set_icon_name(Some(FULL));
            }
            else if to_value == 5 {
                self.s3.set_icon_name(Some(HALF));
            }
            else {
                self.s3.set_icon_name(Some(NONE));
            }
            if to_value >= 8 {
                self.s4.set_icon_name(Some(FULL));
            }
            else if to_value == 7 {
                self.s4.set_icon_name(Some(HALF));
            }
            else {
                self.s4.set_icon_name(Some(NONE));
            }
            if to_value == 10 {
                self.s5.set_icon_name(Some(FULL));
            }
            else if to_value == 9 {
                self.s5.set_icon_name(Some(HALF));
            }
            else {
                self.s5.set_icon_name(Some(NONE));
            }
        }
    }
}

glib::wrapper! {
    pub struct Rating(ObjectSubclass<imp::Rating>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for Rating {
    fn default() -> Self {
        glib::Object::new()
    }
}
