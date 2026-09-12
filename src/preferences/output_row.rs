use adw::prelude::*;
use glib::{Object, Properties, clone};
use gtk::{CompositeTemplate, glib, subclass::prelude::*};
use strum::{EnumMessage, IntoEnumIterator};

use crate::{
    common::map_output_plugin_icon,
    server::{
        AudioFormatConfig, DsdMultiplier, MixerType, PcmBitDepth, PcmSampleRate, ReplayGainHandler,
        config::{OutputConfig, OutputType},
    },
};

mod imp {

    use std::{cell::RefCell, sync::OnceLock};

    use gtk::glib::{WeakRef, subclass::Signal};
    use strum::VariantNames;

    use crate::server::{DsdMultiplier, PcmBitDepth, PcmSampleRate, ReplayGainHandler};

    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::OutputRow)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/output-row.ui")]
    pub struct OutputRow {
        #[template_child]
        pub header: TemplateChild<adw::ExpanderRow>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,
        #[template_child]
        pub output_type: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub enabled: TemplateChild<adw::SwitchRow>,

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

        pub check_delay: RefCell<Option<glib::SourceId>>, // for check signal debounce
        pub parent: WeakRef<gtk::ListBoxRow>,             // for firing the delete signal
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for OutputRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaOutputRow";
        type Type = super::OutputRow;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for OutputRow {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
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

            // Name is already bound in .ui file. Just the output type requires custom logic.
            self.output_type
                .bind_property("selected", &self.header.get(), "subtitle")
                .transform_to(|_, idx: u32| Some(OutputType::VARIANTS[idx as usize].to_value()))
                .sync_create()
                .build();

            self.output_type.connect_selected_item_notify(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().update_icon();
                }
            ));

            self.name.connect_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let Some(id) = this.check_delay.take() {
                        id.remove();
                    }

                    let _ = this
                        .check_delay
                        .replace(Some(glib::source::timeout_add_local_once(
                            core::time::Duration::from_millis(200),
                            clone!(
                                #[weak]
                                this,
                                move || {
                                    this.obj().emit_by_name::<()>("renamed", &[]);
                                    let _ = this.check_delay.take();
                                }
                            ),
                        )));
                }
            ));

            let is_pcm = self.force_format_pcm_dsd.active_name().unwrap().as_str() == "pcm";
            self.pcm_sr_box.set_visible(is_pcm);
            self.pcm_bit_box.set_visible(is_pcm);
            self.dsd_box.set_visible(!is_pcm);
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

            self.mixer_type
                .set_model(Some(&gtk::StringList::new(MixerType::VARIANTS)));

            self.replaygain_handler
                .set_model(Some(&gtk::StringList::new(ReplayGainHandler::VARIANTS)));

            self.remove.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().emit_by_name::<()>(
                        "delete-clicked",
                        &[&this
                            .parent
                            .upgrade()
                            .map(|row| row.index())
                            .unwrap_or(-1)
                            .to_value()],
                    );
                }
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("delete-clicked")
                        .param_types([i32::static_type()])
                        .build(),
                    Signal::builder("renamed").build(),
                ]
            })
        }
    }

    impl WidgetImpl for OutputRow {}
}

glib::wrapper! {
    pub struct OutputRow(ObjectSubclass<imp::OutputRow>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl OutputRow {
    pub fn new(config: &OutputConfig) -> Self {
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
        res.imp().mixer_type.set_selected(config.mixer_type as u32);
        res.imp()
            .replaygain_handler
            .set_selected(config.replaygain_handler as u32);

        // TODO: OUTPUT TYPE-SPECIFIC CONFIGURATION
        res
    }

    pub fn bind_parent(&self, parent: &gtk::ListBoxRow) {
        self.imp().parent.set(Some(parent));
    }

    pub fn update_icon(&self) {
        self.imp().icon.set_icon_name(Some(map_output_plugin_icon(
            &OutputType::from_repr(self.imp().output_type.selected() as usize)
                .map(|var| var.get_serializations()[0])
                .unwrap_or(""),
        )));
    }

    pub fn name(&self) -> String {
        self.imp().name.text().to_string()
    }

    pub fn highlight_name_error(&self, is_error: bool) {
        if is_error {
            if !self.imp().icon.has_css_class("error") {
                self.imp().icon.set_css_classes(&["error"]);
            }
            if !self.imp().name.has_css_class("error") {
                self.imp().name.set_css_classes(&["error"]);
            }
        } else {
            if self.imp().icon.has_css_class("error") {
                self.imp().icon.remove_css_class("error");
            }
            if self.imp().name.has_css_class("error") {
                self.imp().name.remove_css_class("error");
            }
        }
    }

    pub fn generate_config(&self) -> OutputConfig {
        let mut config = OutputConfig::default();
        config.output_type =
            OutputType::from_repr(self.imp().output_type.selected() as usize).unwrap();
        config.name = self.imp().name.text().to_string();
        if self.imp().force_format.is_active() {
            let raw_val = self.imp().force_format_channels.value();
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
            config.tags = self.imp().send_tags.is_active();
            config.always_off = self.imp().always_off.is_active();
            config.always_on = self.imp().always_on.is_active();
            config.mixer_type =
                MixerType::from_repr(self.imp().mixer_type.selected() as usize).unwrap_or_default();
            config.replaygain_handler =
                ReplayGainHandler::from_repr(self.imp().replaygain_handler.selected() as usize)
                    .unwrap_or_default();
        }

        config
    }
}
