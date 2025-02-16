use adw::subclass::prelude::*;
use adw::prelude::*;
use gtk::{
    glib::{self, Value, Variant},
    CompositeTemplate
};

use crate::utils;

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/player.ui")]
    pub struct PlayerPreferences {
        #[template_child]
        pub use_album_art_as_bg: TemplateChild<adw::SwitchRow>,
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
        pub enable_mpris: TemplateChild<adw::SwitchRow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlayerPreferences {
        const NAME: &'static str = "EuphonicaPlayerPreferences";
        type Type = super::PlayerPreferences;
        type ParentType = adw::PreferencesPage;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for PlayerPreferences {}
    impl WidgetImpl for PlayerPreferences {}
    impl PreferencesPageImpl for PlayerPreferences {}
}

glib::wrapper! {
    pub struct PlayerPreferences(ObjectSubclass<imp::PlayerPreferences>)
        @extends adw::PreferencesPage,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Widget;
}

impl Default for PlayerPreferences {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlayerPreferences {
    pub fn setup(&self) {
        let imp = self.imp();
        // Populate with current gsettings values
        let settings = utils::settings_manager();
        // Set up player settings
        let use_album_art_as_bg = imp.use_album_art_as_bg.get();
        let bg_blur_radius = imp.bg_blur_radius.get();
        let bg_opacity = imp.bg_opacity.get();
        let bg_transition_duration = imp.bg_transition_duration.get();
        for widget in [&bg_blur_radius, &bg_opacity, &bg_transition_duration] {
            use_album_art_as_bg
                .bind_property(
                    "active",
                    widget,
                    "sensitive"
                )
                .sync_create()
                .build();
        }

        let player_settings = settings.child("player");
        player_settings
            .bind(
                "use-album-art-as-bg",
                &use_album_art_as_bg,
                "active"
            )
            .build();

        player_settings
            .bind(
                "bg-blur-radius",
                &bg_blur_radius.adjustment(),
                "value"
            )
            .build();

        player_settings
            .bind(
                "bg-opacity",
                &bg_opacity.adjustment(),
                "value"
            )
            .build();

        player_settings
            .bind(
                "bg-transition-duration-s",
                &bg_transition_duration.adjustment(),
                "value"
            )
            .build();

        let vol_knob_unit = imp.vol_knob_unit.get();
        let vol_knob_sensitivity = imp.vol_knob_sensitivity.get();
        player_settings
            .bind(
                "vol-knob-unit",
                &vol_knob_unit,
                "selected"
            )
            .mapping(
                |v: &Variant, _| { match v.get::<String>().unwrap().as_str() {
                    "percents" => Some(0.to_value()),
                    "decibels" => Some(1.to_value()),
                    _ => unreachable!()
                }}
            )
            .set_mapping(
                |v: &Value, _| { match v.get::<u32>().ok() {
                    Some(0) => Some("percents".to_variant()),
                    Some(1) => Some("decibels".to_variant()),
                    _ => unreachable!()
                }}
            )
            .build();

        player_settings
            .bind(
                "vol-knob-sensitivity",
                &vol_knob_sensitivity,
                "value"
            )
            .build();

        let enable_mpris = self.imp().enable_mpris.get();
        player_settings
            .bind(
                "enable-mpris",
                &enable_mpris,
                "active"
            )
            .build();
    }
}
