use cairo::LineCap;
use glib::{
    prelude::*, subclass::prelude::*, ParamSpec, ParamSpecBoolean, ParamSpecDouble,
    clone, Object
};
use gtk::{
    cairo,
    prelude::*, subclass::prelude::*,
    CompositeTemplate
};
use std::{cell::Cell, f64::consts::PI};

fn convert_to_dbfs(pct: f64) -> Result<f64, ()> {
    // Accepts 0-100
    if pct > 0.0 && pct < 100.0 {
        return Ok(10.0 * (pct / 100.0).log10());
    }
    Err(())
}

mod imp {
    use super::*;
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/volume-knob.ui")]
    pub struct VolumeKnob {
        #[template_child]
        pub draw_area: TemplateChild<gtk::DrawingArea>,
        #[template_child]
        pub readout: TemplateChild<gtk::Label>,
        #[template_child]
        pub unit: TemplateChild<gtk::Label>,
        #[template_child]
        pub readout_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub knob_btn: TemplateChild<gtk::ToggleButton>,
        // Stored here & bound to the settings manager so we can avoid having
        // to query the setting on every frame while scrolling.
        pub sensitivity: Cell<f64>,
        pub use_dbfs: Cell<bool>,
        // 0 to 100. Full precision for smooth scrolling effect.
        pub value: Cell<f64>,
        pub is_muted: Cell<bool>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for VolumeKnob {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaVolumeKnob";
        type Type = super::VolumeKnob;
        type ParentType = gtk::Box;

        fn new() -> Self {
            Self {
                draw_area: TemplateChild::default(),
                readout: TemplateChild::default(),
                unit: TemplateChild::default(),
                readout_stack: TemplateChild::default(),
                knob_btn: TemplateChild::default(),
                use_dbfs: Cell::new(false),
                sensitivity: Cell::new(1.0),
                value: Cell::new(0.0),
                is_muted: Cell::new(false),
            }
        }

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for VolumeKnob {
        fn constructed(&self) {
            self.parent_constructed();
            // Bind readouts
            let obj_ = self.obj();
            let obj = obj_.as_ref();
            let knob_btn = self.knob_btn.get();
            let readout_stack = self.readout_stack.get();
            knob_btn
                .bind_property("active", &readout_stack, "visible-child-name")
                .transform_to(|_, active: bool| {
                    if active {
                        return Some("mute");
                    }
                    Some("readout")
                })
                .sync_create()
                .build();

            knob_btn
                .bind_property("active", obj, "is-muted")
                .sync_create()
                .build();

            obj.update_readout();
            obj.connect_notify_local(Some("value"), |this, _| {
                this.update_readout();
            });
            let unit = self.unit.get();
            obj.bind_property("use-dbfs", &unit, "label")
                .transform_to(|_, use_dbfs: bool| {
                    if use_dbfs {
                        return Some("dBFS");
                    }
                    Some("%")
                })
                .sync_create()
                .build();
            // Draw curve from 0 to current volume level
            // (goes from 7:30 to 4:30 CW, which is -270deg to 45deg for cairo_arc).
            // Currently hardcoding diameter to 96px.
            let draw_area = self.draw_area.get();
            draw_area.set_draw_func(clone!(
                #[weak(rename_to = this)]
                obj,
                move |da, cr, w, h| {
                    let fg = da.color();
                    cr.set_source_rgb(fg.red() as f64, fg.green() as f64, fg.blue() as f64);
                    // Match seekbar thickness
                    cr.set_line_width(4.0);
                    cr.set_line_cap(LineCap::Round);
                    // Starting
                    // At 0 => 5pi/4
                    let angle = -1.25 * PI + 1.5 * PI * this.imp().value.get() / 100.0;
                    // u w0t m8
                    cr.arc(w as f64 / 2.0, h as f64 / 2.0, 50.0, -1.25 * PI, angle);
                    let _ = cr.stroke();
                }
            ));

            // Enable scrolling to change volume
            // TODO: Implement vertical dragging & keyboard controls
            // TODO: Let user control scroll sensitivity
            let scroll_ctl = gtk::EventControllerScroll::default();
            scroll_ctl.set_flags(gtk::EventControllerScrollFlags::VERTICAL);
            scroll_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
            scroll_ctl.connect_scroll(clone!(
                #[weak(rename_to = this)]
                obj,
                #[upgrade_or]
                glib::signal::Propagation::Proceed,
                move |_, _, dy| {
                    let new_vol = this.imp().value.get() - dy * this.sensitivity();
                    if (0.0..=100.0).contains(&new_vol) {
                        this.set_value(new_vol);
                    }
                    this.imp().draw_area.queue_draw();
                    glib::signal::Propagation::Proceed
                }
            ));
            obj.add_controller(scroll_ctl);

            // Update level arc upon changing foreground colour, for example when switching dark/light mode
            obj.connect_notify_local(Some("color"), |this, _| {
                this.imp().draw_area.queue_draw();
            });
        }
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    // Only modifiable via internal setter
                    ParamSpecDouble::builder("value").read_only().build(),
                    ParamSpecDouble::builder("sensitivity").build(),
                    ParamSpecBoolean::builder("is-muted").build(),
                    ParamSpecBoolean::builder("use-dbfs").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "value" => self.value.get().to_value(),
                "is-muted" => self.is_muted.get().to_value(),
                "sensitivity" => self.sensitivity.get().to_value(),
                "use-dbfs" => self.use_dbfs.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "sensitivity" => {
                    if let Ok(s) = value.get::<f64>() {
                        // No checks performed here (UI widget should be a GtkScale).
                        let old_sensitivity = self.sensitivity.replace(s);
                        if old_sensitivity != s {
                            obj.notify("sensitivity");
                        }
                    }
                }
                "is-muted" => {
                    if let Ok(is_muted) = value.get::<bool>() {
                        let was_muted = self.is_muted.replace(is_muted);
                        if was_muted != is_muted {
                            obj.notify("is-muted");
                        }
                    }
                }
                "use-dbfs" => {
                    if let Ok(b) = value.get::<bool>() {
                        let old_use_dbfs = self.use_dbfs.replace(b);
                        if old_use_dbfs != b {
                            obj.notify("use-dbfs");
                            obj.notify("value"); // Fire this too to redraw the readout
                        }
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for VolumeKnob {}

    // Trait shared by all boxes
    impl BoxImpl for VolumeKnob {}
}

glib::wrapper! {
    pub struct VolumeKnob(ObjectSubclass<imp::VolumeKnob>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for VolumeKnob {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumeKnob {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn sensitivity(&self) -> f64 {
        self.imp().sensitivity.get()
    }

    pub fn use_dbfs(&self) -> bool {
        self.imp().use_dbfs.get()
    }

    pub fn value(&self) -> f64 {
        self.imp().value.get()
    }

    pub fn is_muted(&self) -> bool {
        self.imp().is_muted.get()
    }

    pub fn set_value(&self, val: f64) {
        let old_val = self.imp().value.replace(val);
        if old_val != val {
            self.notify("value");
        }
    }

    pub fn sync_value(&self, new_rounded: i8) {
        // Set volume based on rounded i8 value silently.
        // Useful for syncing to external changes.
        // Will only update our full-precision value when it's "different" enough.
        let old_rounded = self.imp().value.get().round() as i8;
        if old_rounded != new_rounded {
            let _ = self.imp().value.replace(new_rounded as f64);
            self.notify("value");
            // Will not emit a signal (doing so would result in an infinite loop
            // between parent widget and this one).
        }
    }

    fn update_readout(&self) {
        let readout = self.imp().readout.get();
        let val = self.imp().value.get();
        if self.imp().use_dbfs.get() {
            if let Ok(dbfs) = convert_to_dbfs(val) {
                readout.set_label(&format!("{:.2}", dbfs));
            } else if val > 0.0 {
                readout.set_label("0");
            } else {
                readout.set_label("-∞");
            }
        } else {
            readout.set_label(&format!("{:.0}", val));
        }
    }
}
