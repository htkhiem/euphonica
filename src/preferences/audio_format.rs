use adw::prelude::*;
use glib::{Object, Properties, clone};
use gtk::{CompositeTemplate, glib, subclass::prelude::*};

use crate::{server::{AudioFormatConfig, DsdMultiplier, PcmBitDepth, PcmSampleRate}, utils::meta_provider_settings};

use super::IntegrationsPreferences;

mod imp {
    use std::cell::{Cell, RefCell};

    use adw::subclass::{action_row::ActionRowImpl, preferences_row::PreferencesRowImpl};
    use strum::VariantNames;

    use crate::server::{DsdMultiplier, PcmBitDepth, PcmSampleRate};

    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::AudioFormatEntry)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/audio-format.ui")]
    pub struct AudioFormatEntry {
        #[template_child]
        pub pcm_dsd_toggle: TemplateChild<adw::ToggleGroup>,
        #[template_child]
        pub pcm_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub dsd_box: TemplateChild<gtk::Box>,
        #[template_child]
        pub pcm_samplerate: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub pcm_bitdepth: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub dsd_preset: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub dop: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub channels: TemplateChild<gtk::SpinButton>, // shared between PCM and DSD; set to 0 to disable coercing
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for AudioFormatEntry {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaAudioFormatEntry";
        type Type = super::AudioFormatEntry;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for AudioFormatEntry {
        fn constructed(&self) {
            self.parent_constructed();
            self.pcm_samplerate
                .set_model(Some(&gtk::StringList::new(PcmSampleRate::VARIANTS)));
            self.pcm_bitdepth
                .set_model(Some(&gtk::StringList::new(PcmBitDepth::VARIANTS)));
            self.dsd_preset
                .set_model(Some(&gtk::StringList::new(DsdMultiplier::VARIANTS)));

            let is_pcm = self.pcm_dsd_toggle.active_name().unwrap().as_str() == "pcm";
            self.pcm_box.set_visible(is_pcm);
            self.dsd_box.set_visible(!is_pcm);
            self.pcm_dsd_toggle.connect_active_name_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |dropdown| {
                    let is_pcm = dropdown.active_name().unwrap().as_str() == "pcm";
                    this.pcm_box.set_visible(is_pcm);
                    this.dsd_box.set_visible(!is_pcm);
                }
            ));
        }
    }

    impl WidgetImpl for AudioFormatEntry {}

    impl BoxImpl for AudioFormatEntry {}
}

glib::wrapper! {
    pub struct AudioFormatEntry(ObjectSubclass<imp::AudioFormatEntry>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AudioFormatEntry {
    pub fn load(&self, config: &AudioFormatConfig) {
        let channels;
        match config {
            &AudioFormatConfig::Dsd(mul, ch, dop) => {
                self.imp().pcm_dsd_toggle.set_active_name(Some("dsd"));
                // Thanks to using strum::VariantNames as stringlist these are guaranteed to be within the valid range
                self.imp().dsd_preset.set_selected(mul as u32);
                channels = ch;
            }
            &AudioFormatConfig::Pcm(rate, bits, ch) => {
                self.imp().pcm_dsd_toggle.set_active_name(Some("pcm"));
                self.imp().pcm_samplerate.set_selected(rate as u32);
                self.imp().pcm_bitdepth.set_selected(bits as u32);
                channels = ch;
            }
        }
        if let Some(channels) = channels {
            self.imp().channels.set_value(channels as f64);
        }
    }

    pub fn generate_config(&self) -> AudioFormatConfig {
        let raw_channels = self.imp().channels.value();
        let channels = if (1.0..=128.0).contains(&raw_channels) {
            Some(raw_channels.round() as u8)
        } else {
            None
        };
        if self
            .imp()
            .pcm_dsd_toggle
            .active_name()
            .is_some_and(|name| name.as_str() == "pcm")
        {
            AudioFormatConfig::Pcm(
                PcmSampleRate::from_repr(self.imp().pcm_samplerate.selected() as usize)
                    .unwrap_or_default(),
                PcmBitDepth::from_repr(self.imp().pcm_bitdepth.selected() as usize)
                    .unwrap_or_default(),
                channels,
            )
        } else {
            AudioFormatConfig::Dsd(
                DsdMultiplier::from_repr(self.imp().dsd_preset.selected() as usize)
                    .unwrap_or_default(),
                channels,
                self.imp().dop.is_active(),
            )
        }
    }
}
