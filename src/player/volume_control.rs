use glib::{SignalHandlerId, WeakRef, clone, closure_local};
use gtk::{
    CompositeTemplate,
    glib::{self, Object, ParamSpec, ParamSpecBoolean, ParamSpecDouble, Variant, prelude::*},
    prelude::*,
    subclass::prelude::*,
};
use once_cell::sync::Lazy;
use std::cell::{Cell, RefCell};

use crate::{player::Player, utils::settings_manager};

fn convert_to_dbfs(percent: f64) -> Option<f64> {
    (percent > 0.0 && percent < 100.0).then(|| 10.0 * (percent / 100.0).log10())
}

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/volume-control.ui")]
    pub struct VolumeControl {
        #[template_child]
        pub mute_button: TemplateChild<gtk::Button>,
        #[template_child]
        pub scale: TemplateChild<gtk::Scale>,
        #[template_child]
        pub readout: TemplateChild<gtk::Label>,
        pub sensitivity: Cell<f64>,
        pub use_dbfs: Cell<bool>,
        pub value: Cell<f64>,
        pub syncing: Cell<bool>,
        pub volume_changed_id: RefCell<Option<SignalHandlerId>>,
        pub muted_id: RefCell<Option<SignalHandlerId>>,
        pub player: WeakRef<Player>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for VolumeControl {
        const NAME: &'static str = "EuphonicaVolumeControl";
        type Type = super::VolumeControl;
        type ParentType = gtk::Box;

        fn new() -> Self {
            Self {
                sensitivity: Cell::new(1.0),
                ..Default::default()
            }
        }

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for VolumeControl {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            let settings = settings_manager().child("ui");
            settings
                .bind("vol-knob-unit", obj.as_ref(), "use-dbfs")
                .get_only()
                .mapping(|value: &Variant, _| {
                    Some((value.get::<String>().unwrap().as_str() == "decibels").to_value())
                })
                .build();
            settings
                .bind("vol-knob-sensitivity", obj.as_ref(), "sensitivity")
                .mapping(|value: &Variant, _| Some(value.get::<f64>().unwrap().to_value()))
                .build();

            self.mute_button.connect_clicked(clone!(
                #[weak(rename_to = this)]
                obj,
                move |_| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        async move {
                            if let Some(player) = this.imp().player.upgrade()
                                && let Err(error) = player.toggle_mute().await
                            {
                                dbg!(error);
                            }
                        }
                    ));
                }
            ));

            self.scale.connect_value_changed(clone!(
                #[weak(rename_to = this)]
                obj,
                move |scale| {
                    if !this.imp().syncing.get() {
                        this.set_value(scale.value());
                    }
                }
            ));

            let scroll =
                gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
            scroll.set_propagation_phase(gtk::PropagationPhase::Capture);
            scroll.connect_scroll(clone!(
                #[weak(rename_to = this)]
                obj,
                #[upgrade_or]
                glib::Propagation::Proceed,
                move |_, dx, dy| {
                    if !this.imp().scale.is_sensitive() {
                        return glib::Propagation::Proceed;
                    }
                    let delta = if dy != 0.0 { -dy } else { dx };
                    if delta == 0.0 {
                        return glib::Propagation::Proceed;
                    }
                    let value = (this.value() + delta * this.sensitivity()).clamp(0.0, 100.0);
                    this.imp().scale.set_value(value);
                    glib::Propagation::Stop
                }
            ));
            self.scale.add_controller(scroll);
        }

        fn dispose(&self) {
            if let Some(player) = self.player.upgrade() {
                if let Some(id) = self.volume_changed_id.take() {
                    player.disconnect(id);
                }
                if let Some(id) = self.muted_id.take() {
                    player.disconnect(id);
                }
            }
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecDouble::builder("value")
                        .minimum(0.0)
                        .maximum(100.0)
                        .read_only()
                        .build(),
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
                    if let Ok(value) = value.get::<f64>()
                        && self.sensitivity.replace(value) != value
                    {
                        obj.notify("sensitivity");
                    }
                }
                "use-dbfs" => {
                    if let Ok(value) = value.get::<bool>()
                        && self.use_dbfs.replace(value) != value
                    {
                        obj.notify("use-dbfs");
                        self.update_readout();
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for VolumeControl {}
    impl BoxImpl for VolumeControl {}

    impl VolumeControl {
        pub fn update_readout(&self) -> bool {
            let Some(player) = self.player.upgrade() else {
                return false;
            };
            let muted = player.is_muted();
            let value = self.value.get();
            let text = if muted {
                "—".to_owned()
            } else if self.use_dbfs.get() {
                convert_to_dbfs(value)
                    .map(|dbfs| format!("{dbfs:.0} dBFS"))
                    .unwrap_or_else(|| {
                        if value > 0.0 {
                            "0 dBFS".to_owned()
                        } else {
                            "-∞ dBFS".to_owned()
                        }
                    })
            } else {
                format!("{value:.0}%")
            };
            self.readout.set_label(&text);
            self.scale.set_sensitive(!muted);
            self.mute_button.set_icon_name(if muted || value <= 0.0 {
                "audio-volume-muted-symbolic"
            } else if value < 33.0 {
                "audio-volume-low-symbolic"
            } else if value < 67.0 {
                "audio-volume-medium-symbolic"
            } else {
                "audio-volume-high-symbolic"
            });
            muted
        }
    }
}

glib::wrapper! {
    pub struct VolumeControl(ObjectSubclass<imp::VolumeControl>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for VolumeControl {
    fn default() -> Self {
        Self::new()
    }
}

impl VolumeControl {
    pub fn new() -> Self {
        Object::builder().build()
    }

    pub fn setup(&self, player: &Player) {
        if let Some(old_player) = self.imp().player.upgrade() {
            if let Some(id) = self.imp().volume_changed_id.take() {
                old_player.disconnect(id);
            }
            if let Some(id) = self.imp().muted_id.take() {
                old_player.disconnect(id);
            }
        }
        self.imp().player.set(Some(player));
        self.sync_value(player.mpd_volume());
        self.imp().update_readout();

        self.imp()
            .volume_changed_id
            .replace(Some(player.connect_closure(
                "volume-changed",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: &Player, value: i8| this.sync_value(value)
                ),
            )));
        self.imp()
            .muted_id
            .replace(Some(player.connect_notify_local(
                Some("is-muted"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| {
                        this.imp().update_readout();
                    }
                ),
            )));
    }

    pub fn sensitivity(&self) -> f64 {
        self.imp().sensitivity.get()
    }

    pub fn value(&self) -> f64 {
        self.imp().value.get()
    }

    pub fn set_value(&self, value: f64) {
        let value = value.clamp(0.0, 100.0);
        let Some(player) = self.imp().player.upgrade() else {
            return;
        };
        if self.imp().value.replace(value) != value {
            self.notify("value");
        }
        self.imp().update_readout();
        if !player.is_muted() {
            glib::spawn_future_local(async move {
                if let Err(error) = player.send_set_volume(value.round() as i8).await {
                    dbg!(error);
                }
            });
        }
    }

    pub fn sync_value(&self, value: i8) {
        if self.imp().value.get().round() as i8 != value {
            self.imp().value.set(value as f64);
            self.imp().syncing.set(true);
            self.imp().scale.set_value(value as f64);
            self.imp().syncing.set(false);
            self.notify("value");
            self.imp().update_readout();
        }
    }
}
