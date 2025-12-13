use glib::{Object, clone};
use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*};
use std::cell::Cell;

use crate::{common::QualityGrade, utils};

use super::Player;

mod imp {
    use std::cell::OnceCell;

    use crate::utils::format_secs_as_duration;
    use glib::{ParamSpec, ParamSpecDouble};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/seekbar.ui")]
    pub struct Seekbar {
        #[template_child]
        pub seekbar: TemplateChild<gtk::Scale>,
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
        pub player: OnceCell<Player>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for Seekbar {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaSeekbar";
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
            // Capture mouse button release action for seekbar
            // Funny story: gtk::Scale has its own GestureClick controller which eats up mouse button events.
            // Workaround: capture mouse button release event at a higher level in capture phase, using a bool
            // set by the seekbar's change-value signal to determine whether it is related to the seekbar or not.
            let seekbar_gesture = gtk::GestureClick::new();
            seekbar_gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
            seekbar_gesture.connect_released(clone!(
                #[weak(rename_to = this)]
                self,
                move |gesture, _, _, _| {
                    gesture.set_state(gtk::EventSequenceState::None); // allow propagating to seekbar
                    if this.seekbar_clicked.get() {
                        if let Some(player) = this.player.get() {
                            player.send_seek(this.seekbar.value());
                        }
                        this.seekbar_clicked.replace(false);
                    }
                }
            ));
            obj.add_controller(seekbar_gesture);

            self.seekbar.connect_change_value(clone!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                glib::signal::Propagation::Proceed,
                move |_, _, _| {
                    // Only emit this once
                    if !this.seekbar_clicked.get() {
                        let _ = this.seekbar_clicked.replace(true);
                    }
                    glib::signal::Propagation::Proceed
                }
            ));

            self.seekbar
                .adjustment()
                .bind_property("value", &self.elapsed.get(), "label")
                .transform_to(|_, pos| Some(format_secs_as_duration(pos)))
                .sync_create()
                .build();

            self.seekbar
                .adjustment()
                .bind_property("upper", &self.duration.get(), "label")
                .transform_to(|_, dur: f64| {
                    if dur > 0.0 {
                        return Some(format_secs_as_duration(dur));
                    }
                    Some("--:--".to_owned())
                })
                .sync_create()
                .build();
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecDouble::builder("position").build(),
                    ParamSpecDouble::builder("duration").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
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

    impl WidgetImpl for Seekbar {}

    impl BoxImpl for Seekbar {}
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
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn position(&self) -> f64 {
        self.imp().seekbar.value()
    }

    /// Will have no effect while seekbar is being held by the user
    pub fn set_position(&self, new: f64) {
        if !self.imp().seekbar_clicked.get() {
            self.imp().seekbar.set_value(new);
        }
    }

    pub fn duration(&self) -> f64 {
        self.imp().seekbar.adjustment().upper()
    }

    pub fn set_duration(&self, new: f64) {
        self.imp().seekbar.set_range(0.0, new);
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

        let _ = self.imp().player.set(player.clone());
    }
}
