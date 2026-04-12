use gtk::{
    gsk,
    graphene,
    cairo as cr,
    glib::{
        self, Object, ParamSpec, ParamSpecBoolean, ParamSpecDouble, clone, prelude::*, subclass::prelude::*,
    },
    CompositeTemplate, prelude::*, subclass::prelude::*};
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
    use gtk::gdk;
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/volume-knob.ui")]
    pub struct VolumeKnob {
        // Stored here & bound to the settings manager so we can avoid having
        // to query the setting on every frame while scrolling.
        pub sensitivity: Cell<f64>,
        pub use_dbfs: Cell<bool>,
        // 0 to 100. Full precision for smooth scrolling effect.
        pub value: Cell<f64>
        // Active state means muted
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for VolumeKnob {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaVolumeKnob";
        type Type = super::VolumeKnob;
        type ParentType = gtk::ToggleButton;

        fn new() -> Self {
            Self {
                use_dbfs: Cell::new(false),
                sensitivity: Cell::new(1.0),
                value: Cell::new(0.0)
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
            let obj_ = self.obj();
            let obj = obj_.as_ref();
            self.update_readout();
            obj.connect_notify_local(Some("value"), |this, _| {
                this.imp().update_readout();
            });
            obj.connect_notify_local(Some("active"), |this, _| {
                this.imp().update_readout();
            });

            // Enable scrolling to change volume
            // TODO: Implement angular dragging
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
                    this.queue_draw();
                    glib::signal::Propagation::Proceed
                }
            ));
            obj.add_controller(scroll_ctl);
        }
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    // Only modifiable via internal setter
                    ParamSpecDouble::builder("value").read_only().build(),
                    ParamSpecDouble::builder("sensitivity").build(),
                    ParamSpecBoolean::builder("use-dbfs").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "value" => self.value.get().to_value(),
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
    impl WidgetImpl for VolumeKnob {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let (w, h) = (self.obj().width(), self.obj().height());
            let centre = (w as f64 / 2.0, h as f64 / 2.0);
            let min_dim = w.min(h) as f64;
            let bounds = graphene::Rect::new(
                0.0, 0.0, w as f32, h as f32
            );
            // Fade out the previous gradient conically 
            snapshot.push_mask(gsk::MaskMode::Alpha);
            // Conic gradient goes clockwise. Rotation=0 means starting at 12 o'clock.
            // Our knob starts at 6 o'clock so we'll use a 180-deg rotation.
            snapshot.append_conic_gradient(
                &bounds, 
                &graphene::Point::new(centre.0 as f32, centre.1 as f32), 
                180.0, 
                &[
                    gsk::ColorStop::new(0.0, gdk::RGBA::BLACK.with_alpha(0.0)),
                    // Full opacity at the current vol level's angle
                    gsk::ColorStop::new(self.value.get() as f32 / 100.0, gdk::RGBA::BLACK),
                ]
            );
            snapshot.pop();
            
            let cr = snapshot.append_cairo(&bounds);
            let fg = self.obj().color();
            // New design: piechart-like mask + glowy radial gradient.
            // Also use a conical fading effect to more clearly indicate that this is a twistable
            // and not just a button with fancy gradients when it's turned to 100%.
            // Rendering model is a hybrid of Cairo (CPU) and GSK (maybe GPU) due to:
            // - Cairo drawing partial circular arcs in a very straightforward way (GSK doesn't), but
            // - GSK knowing what a conical gradient is.
            cr.move_to(centre.0, centre.1 + min_dim);
            cr.line_to(centre.0, centre.1);
            cr.arc_negative(
                centre.0, 
                centre.1, 
                min_dim / 2.0 - 1.0, 
                PI / 2.0 + 2.0 * PI * self.value.get() / 100.0, PI / 2.0
            );
            cr.close_path();
            let radial = cr::RadialGradient::new(
                // Outer circle
                centre.0, 
                centre.1, 
                min_dim / 2.0 - 1.0,

                // Inner circle
                centre.0, 
                centre.1, 
                min_dim / 2.0 - 10.0  // How far inward the gradient will extend
            );
            radial.add_color_stop_rgba(
                0.0, fg.red() as f64, fg.green() as f64, fg.blue() as f64, 1.0
            );
            radial.add_color_stop_rgba(
                1.0, fg.red() as f64, fg.green() as f64, fg.blue() as f64, 0.0
            );
            cr.set_source(radial);
            cr.fill();
            snapshot.pop();
            
            self.parent_snapshot(snapshot);
        }
    }

    impl ButtonImpl for VolumeKnob {}

    impl ToggleButtonImpl for VolumeKnob {}

    impl VolumeKnob {
        pub fn update_readout(&self) {
            let obj = self.obj();
            let val = self.value.get();
            if obj.is_active() {
                obj.set_label("—");  // can you believe a human copypasted an em dash here
            } else {
                if self.use_dbfs.get() {
                    if let Ok(dbfs) = convert_to_dbfs(val) {
                        obj.set_label(&format!("{dbfs:.0}"));
                    } else if val > 0.0 {
                        obj.set_label("0");
                    } else {
                        obj.set_label("-∞");
                    }
                } else {
                    obj.set_label(&format!("{val:.0}"));
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct VolumeKnob(ObjectSubclass<imp::VolumeKnob>)
    @extends gtk::ToggleButton, gtk::Button, gtk::Widget,
    @implements gtk::Accessible, gtk::Actionable, gtk::Buildable, gtk::ConstraintTarget;
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
            self.imp().update_readout();
            self.queue_draw();
            // Will not notify to prevent signal loop
        }
    }
}
