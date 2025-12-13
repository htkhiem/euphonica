use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate,
    glib::{self, Value, Variant},
};

use crate::{common::marquee::MarqueeWrapMode, utils};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/ui.ui")]
    pub struct UIPreferences {
        #[template_child]
        pub recent_playlists_count: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub max_columns: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub auto_accent: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub title_wrap_mode: TemplateChild<adw::ComboRow>,

        #[template_child]
        pub use_album_art_as_bg: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub bg_blur_radius: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub bg_opacity: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub bg_transition_duration: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub vol_knob_unit: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub vol_knob_sensitivity: TemplateChild<adw::SpinRow>,

        #[template_child]
        pub use_visualizer: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub visualizer_min_hz: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub visualizer_max_hz: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub visualizer_smoothing: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub visualizer_bottom_opacity: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub visualizer_top_opacity: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub visualizer_use_log_bins: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub visualizer_scale: TemplateChild<adw::SpinRow>,
        #[template_child]
        pub visualizer_use_splines: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub visualizer_stroke_width: TemplateChild<adw::SpinRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for UIPreferences {
        const NAME: &'static str = "EuphonicaUIPreferences";
        type Type = super::UIPreferences;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for UIPreferences {}
    impl WidgetImpl for UIPreferences {}
    impl PreferencesPageImpl for UIPreferences {}
}

glib::wrapper! {
    pub struct UIPreferences(ObjectSubclass<imp::UIPreferences>)
        @extends adw::PreferencesPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Widget;
}

impl Default for UIPreferences {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl UIPreferences {
    pub fn setup(&self) {
        let imp = self.imp();
        // Populate with current gsettings values
        let settings = utils::settings_manager();
        let player_settings = settings.child("player");
        let ui_settings = settings.child("ui");
        // Set up UI settings
        let recent_playlists_count = imp.recent_playlists_count.get();
        ui_settings
            .bind("recent-playlists-count", &recent_playlists_count, "value")
            .build();

        let max_columns = imp.max_columns.get();
        ui_settings
            .bind("max-columns", &max_columns, "value")
            .build();

        let auto_accent = imp.auto_accent.get();
        ui_settings
            .bind("auto-accent", &auto_accent, "active")
            .build();
        let title_wrap_mode = imp.title_wrap_mode.get();
        ui_settings
            .bind("title-wrap-mode", &title_wrap_mode, "selected")
            .mapping(|v: &Variant, _| {
                Some(
                    MarqueeWrapMode::try_from(v.get::<String>().unwrap().as_str())
                        .unwrap_or_default()
                        .as_idx()
                        .to_value(),
                )
            })
            .set_mapping(|v: &Value, _| {
                Some(
                    MarqueeWrapMode::try_from(v.get::<u32>().unwrap())
                        .unwrap_or_default()
                        .into(),
                )
            })
            .build();
        let use_album_art_as_bg = imp.use_album_art_as_bg.get();
        let bg_blur_radius = imp.bg_blur_radius.get();
        let bg_opacity = imp.bg_opacity.get();
        let bg_transition_duration = imp.bg_transition_duration.get();
        ui_settings
            .bind(
                "use-album-art-as-bg",
                &use_album_art_as_bg,
                "enable-expansion",
            )
            .build();

        ui_settings
            .bind("bg-blur-radius", &bg_blur_radius.adjustment(), "value")
            .build();

        ui_settings
            .bind("bg-opacity", &bg_opacity.adjustment(), "value")
            .build();

        ui_settings
            .bind(
                "bg-transition-duration-s",
                &bg_transition_duration.adjustment(),
                "value",
            )
            .build();

        let vol_knob_unit = imp.vol_knob_unit.get();
        let vol_knob_sensitivity = imp.vol_knob_sensitivity.get();
        ui_settings
            .bind("vol-knob-unit", &vol_knob_unit, "selected")
            .mapping(|v: &Variant, _| match v.get::<String>().unwrap().as_str() {
                "percents" => Some(0.to_value()),
                "decibels" => Some(1.to_value()),
                _ => unreachable!(),
            })
            .set_mapping(|v: &Value, _| match v.get::<u32>().ok() {
                Some(0) => Some("percents".to_variant()),
                Some(1) => Some("decibels".to_variant()),
                _ => unreachable!(),
            })
            .build();

        ui_settings
            .bind("vol-knob-sensitivity", &vol_knob_sensitivity, "value")
            .build();

        ui_settings
            .bind(
                "use-visualizer",
                &imp.use_visualizer.get(),
                "enable-expansion",
            )
            .build();

        player_settings
            .bind(
                "visualizer-spectrum-min-hz",
                &imp.visualizer_min_hz.get(),
                "value",
            )
            .build();

        player_settings
            .bind(
                "visualizer-spectrum-max-hz",
                &imp.visualizer_max_hz.get(),
                "value",
            )
            .build();

        // Constrain min and max hz to never flip around
        imp.visualizer_min_hz
            .bind_property("value", &imp.visualizer_max_hz.adjustment(), "lower")
            .sync_create()
            .build();
        imp.visualizer_max_hz
            .bind_property("value", &imp.visualizer_min_hz.adjustment(), "upper")
            .sync_create()
            .build();

        player_settings
            .bind(
                "visualizer-spectrum-curr-step-weight",
                &imp.visualizer_smoothing.get(),
                "value",
            )
            .mapping(|variant, _| Some((1.0 - variant.get::<f64>().unwrap()).to_value()))
            .set_mapping(|val, _| Some((1.0 - val.get::<f64>().unwrap()).to_variant()))
            .build();

        ui_settings
            .bind(
                "visualizer-bottom-opacity",
                &imp.visualizer_bottom_opacity.get(),
                "value",
            )
            .build();

        ui_settings
            .bind(
                "visualizer-top-opacity",
                &imp.visualizer_top_opacity.get(),
                "value",
            )
            .build();

        player_settings
            .bind(
                "visualizer-spectrum-use-log-bins",
                &imp.visualizer_use_log_bins.get(),
                "active",
            )
            .build();

        ui_settings
            .bind(
                "visualizer-use-splines",
                &imp.visualizer_use_splines.get(),
                "active",
            )
            .build();

        ui_settings
            .bind(
                "visualizer-stroke-width",
                &imp.visualizer_stroke_width.get(),
                "value",
            )
            .build();

        ui_settings
            .bind("visualizer-scale", &imp.visualizer_scale.get(), "value")
            .build();
    }
}
