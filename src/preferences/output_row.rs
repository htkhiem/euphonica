use std::ops::Index;

use adw::prelude::*;
use glib::{Object, Properties, clone};
use gtk::{CompositeTemplate, glib, subclass::prelude::*};
use strum::{EnumMessage, IntoEnumIterator};

use crate::{
    common::map_output_plugin_icon,
    preferences::ClientPreferences,
    server::{
        AudioFormatConfig, DsdMultiplier, MixerType, PcmBitDepth, PcmSampleRate,
        config::{OutputConfig, OutputType},
    },
    utils::meta_provider_settings,
};

mod imp {
    use std::cell::{Cell, RefCell};

    use adw::subclass::{action_row::ActionRowImpl, preferences_row::PreferencesRowImpl};
    use strum::VariantNames;

    use crate::server::{DsdMultiplier, PcmBitDepth, PcmSampleRate};

    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::OutputRow)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/output-row.ui")]
    pub struct OutputRow {
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name: TemplateChild<gtk::Entry>,
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,
        #[template_child]
        pub output_type: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub enabled: TemplateChild<gtk::Switch>,

        #[template_child]
        pub force_format: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub force_format_pcm_dsd: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub pcm_sr_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub pcm_bit_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub dsd_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub force_format_pcm_samplerate: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub force_format_pcm_bitdepth: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub force_format_dsd_preset: TemplateChild<gtk::DropDown>,

        #[template_child]
        pub force_format_channels: TemplateChild<adw::SpinRow>, // shared between PCM and DSD; set to 0 to disable coercing

        #[template_child]
        pub send_tags: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub always_on: TemplateChild<adw::SwitchRow>,
        #[template_child]
        pub always_off: TemplateChild<adw::SwitchRow>, // not contrary to always_on and does something else instead...nice naming

        #[template_child]
        pub mixer_type: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub replaygain_handler: TemplateChild<adw::ComboRow>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for OutputRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaOutputRow";
        type Type = super::OutputRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for OutputRow {
        fn constructed(&self) {
            self.parent_constructed();
            self.output_type
                .set_model(Some(&gtk::StringList::new(&OutputType::VARIANTS)));
            self.force_format_pcm_samplerate
                .set_model(Some(&gtk::StringList::new(PcmSampleRate::VARIANTS)));
            self.force_format_pcm_bitdepth
                .set_model(Some(&gtk::StringList::new(PcmBitDepth::VARIANTS)));
            self.force_format_dsd_preset
                .set_model(Some(&gtk::StringList::new(DsdMultiplier::VARIANTS)));

            self.output_type.connect_selected_item_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().update_icon();
                }
            ));

            self.force_format_pcm_dsd.connect_active_name_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |dropdown| {
                    let is_pcm = dropdown.active_name().unwrap().as_str() == "pcm";
                    this.pcm_sr_box.set_visible(is_pcm);
                    this.pcm_bit_box.set_visible(is_pcm);
                    this.dsd_box.set_visible(!is_pcm);
                }
            ));
        }
    }

    impl WidgetImpl for OutputRow {}

    impl BoxImpl for OutputRow {}
}

glib::wrapper! {
    pub struct OutputRow(ObjectSubclass<imp::OutputRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl OutputRow {
    pub fn new(config: &OutputConfig, controller: &ClientPreferences) -> Self {
        let res: Self = Object::builder().build();
        res.imp().name.set_text(&config.name);
        // Prep output type dropdown
        res.imp()
            .output_type
            .set_selected(config.output_type as u32);
        res.update_icon();
        res.imp().enabled.set_active(config.enabled);

        // Force output format section
        res.imp()
            .force_format
            .set_active(config.format.as_ref().is_some());
        let force_format_spec = config
            .format
            .as_ref()
            .unwrap_or(&AudioFormatConfig::DEFAULT);
        let channels;
        match force_format_spec {
            &AudioFormatConfig::Dsd(mul, ch) => {
                res.imp().force_format_pcm_dsd.set_active_name(Some("dsd"));
                // Thanks to using strum::VariantNames as stringlist these are guaranteed to be within the valid range
                res.imp().force_format_dsd_preset.set_selected(mul as u32);
                channels = ch;
            }
            &AudioFormatConfig::Pcm(rate, bits, ch) => {
                res.imp().force_format_pcm_dsd.set_active_name(Some("pcm"));
                res.imp()
                    .force_format_pcm_samplerate
                    .set_selected(rate as u32);
                res.imp()
                    .force_format_pcm_bitdepth
                    .set_selected(bits as u32);
                channels = ch;
            }
        }
        if let Some(channels) = channels {
            res.imp().force_format_channels.set_value(channels as f64);
        }

        res.imp().send_tags.set_active(config.tags);
        res.imp().always_on.set_active(config.always_on);
        res.imp().always_off.set_active(config.always_off);
        res.imp().mixer_type.set_selected(
            config
                .mixer_type
                .unwrap_or_else(|| MixerType::default_for(config.output_type)) as u32,
        );
        res.imp()
            .replaygain_handler
            .set_selected(config.replaygain_handler as u32);

        // TODO: OUTPUT TYPE-SPECIFIC CONFIGURATION

        // res.setup_actions(controller);
        res
    }

    pub fn update_icon(&self) {
        self.imp().icon.set_icon_name(Some(map_output_plugin_icon(
            &OutputType::from_repr(self.imp().output_type.selected() as usize)
                .map(|var| var.to_string())
                .unwrap_or(String::from("")),
        )));
    }

    pub fn generate_config(&self) -> OutputConfig {
        let mut config = OutputConfig::default();
        config.output_type =
            OutputType::from_repr(self.imp().output_type.selected() as usize).unwrap();
        config.name = self.imp().name.text().to_string();
        if self.imp().force_format.is_active() {
            let raw_val = self.imp()
                .force_format_channels
                .value();
            let channels = if (1.0..=128.0).contains(&raw_val) {
                Some(raw_val.round() as u8)
            } else {
                None
            };
            config.format = Some(
                if self
                    .imp()
                    .force_format_pcm_dsd
                    .active_name()
                    .is_some_and(|name| name.as_str() == "pcm")
                {
                    AudioFormatConfig::Pcm(
                        PcmSampleRate::from_repr(
                            self.imp().force_format_pcm_samplerate.selected() as usize
                        )
                        .unwrap_or_default(),
                        PcmBitDepth::from_repr(
                            self.imp().force_format_pcm_bitdepth.selected() as usize
                        )
                        .unwrap_or_default(),
                        channels,
                    )
                } else {
                    AudioFormatConfig::Dsd(
                        DsdMultiplier::from_repr(
                            self.imp().force_format_dsd_preset.selected() as usize
                        )
                        .unwrap_or_default(),
                        channels,
                    )
                },
            );
        }

        config
    }
}
