use adw::prelude::*;
use glib::{Object, Properties, clone};
use gtk::{CompositeTemplate, glib, subclass::prelude::*};
use rustc_hash::FxHashMap;
use strum::IntoEnumIterator;

use crate::{
    preferences::output_config::config_row::ConfigRow,
    server::{
        AudioFormatConfig, MixerType, ReplayGainHandler,
        config::{OutputConfig, OutputType},
    },
};

use super::AudioFormatEntry;

mod imp {

    use std::{cell::RefCell, sync::OnceLock};

    use gtk::glib::{WeakRef, subclass::Signal};
    use strum::VariantNames;

    use crate::server::ReplayGainHandler;

    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::OutputRow)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/output-row.ui")]
    pub struct OutputRow {
        #[template_child]
        pub inner: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub header: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub common_expander: TemplateChild<adw::ExpanderRow>,
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
        pub force_format_entry: TemplateChild<AudioFormatEntry>,

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

            self.header.connect_changed(clone!(
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
    /// Remove everything between header entry and the common expander
    pub fn clear_plugin_specific_config(&self) {
        let listbox = self.imp().inner.get();
        let end = self.imp().common_expander.get();
        loop {
            if let Some(child) = self
                .imp()
                .header
                .get()
                .next_sibling()
                .and_downcast::<gtk::ListBoxRow>()
            {
                if child == end {
                    break;
                } else {
                    listbox.remove(&child);
                }
            } else {
                break;
            }
        }
    }
    /// Insert plugin-specific config rows between Output type and Force format area.
    pub fn insert_plugin_specific_config(&self, typ: &OutputType, existing: Vec<(String, String)>) {
        // Ensure existing custom rows have been cleared out first.
        // TODO: stop hardcoding the beginning idx (fragile once we add more stuff before config rows).
        let mut idx = 0;
        let listbox = self.imp().inner.get();
        let mut existing_map = FxHashMap::default();
        for (k, v) in existing {
            existing_map.insert(k, v);
        }
        for config_spec in typ.get_custom_config_spec() {
            idx += 1;
            let val = existing_map.get(config_spec.key).map(|s| s.as_str());
            let row = ConfigRow::from_config_spec(config_spec, val);
            listbox.insert(&row, idx);
        }
    }

    pub fn new(config: &OutputConfig) -> Self {
        let res: Self = Object::builder().build();
        res.imp().header.set_text(&config.name);
        // Prep output type dropdown
        res.imp()
            .output_type
            .set_selected(config.output_type as u32);
        res.imp().enabled.set_active(config.enabled);

        // Force output format section
        res.imp()
            .force_format
            .set_active(config.format.as_ref().is_some());
        let _force_format_spec = res.imp().force_format_entry.load(
            config
                .format
                .as_ref()
                .unwrap_or(&AudioFormatConfig::DEFAULT),
        );

        res.imp().send_tags.set_active(config.tags);
        res.imp().always_on.set_active(config.always_on);
        res.imp().always_off.set_active(config.always_off);
        res.imp().mixer_type.set_selected(config.mixer_type as u32);
        res.imp()
            .replaygain_handler
            .set_selected(config.replaygain_handler as u32);

        res.insert_plugin_specific_config(&config.output_type, config.additional_config.clone());
        res.imp().output_type.connect_selected_notify(clone!(
            #[weak(rename_to = this)]
            res,
            move |combo| {
                this.clear_plugin_specific_config();
                if let Some(new_type) = OutputType::from_repr(combo.selected() as usize) {
                    this.insert_plugin_specific_config(&new_type, Vec::with_capacity(0))
                }
            }
        ));

        // Needed to make nested ComboRows work
        res.imp().inner.connect_row_activated(|_lb, row| {
            if let Some(config_row) = row.downcast_ref::<ConfigRow>() {
                config_row.activate_inner();
            }
        });
        res
    }

    pub fn bind_parent(&self, parent: &gtk::ListBoxRow) {
        self.imp().parent.set(Some(parent));
    }

    pub fn name(&self) -> String {
        self.imp().header.text().to_string()
    }

    pub fn highlight_name_error(&self, is_error: bool) {
        if is_error {
            if !self.imp().header.has_css_class("error") {
                self.imp().header.set_css_classes(&["error"]);
            }
        } else {
            if self.imp().header.has_css_class("error") {
                self.imp().header.remove_css_class("error");
            }
        }
    }

    pub fn generate_config(&self) -> OutputConfig {
        let mut config = OutputConfig::default();
        config.output_type =
            OutputType::from_repr(self.imp().output_type.selected() as usize).unwrap();
        config.name = self.imp().header.text().to_string();
        if self.imp().force_format.is_active() {
            config.format = Some(self.imp().force_format_entry.generate_config());
        }
        config.tags = self.imp().send_tags.is_active();
        config.enabled = self.imp().enabled.is_active();
        config.always_off = self.imp().always_off.is_active();
        config.always_on = self.imp().always_on.is_active();
        config.mixer_type =
            MixerType::from_repr(self.imp().mixer_type.selected() as usize).unwrap_or_default();
        config.replaygain_handler =
            ReplayGainHandler::from_repr(self.imp().replaygain_handler.selected() as usize)
                .unwrap_or_default();
        let listbox = self.imp().inner.get();
        // Always-present header row is at 0. The first plugin-specific config row, if present, is at 1.
        // Loop until we hit the "Common settings" expander.
        let mut cur = listbox.row_at_index(1).and_downcast::<ConfigRow>();
        let mut additional_config = Vec::new();

        loop {
            if let Some(row) = cur {
                if let Some(config) = row.generate_config() {
                    additional_config.push(config);
                }
                cur = row.next_sibling().and_downcast::<ConfigRow>();
            } else {
                break;
            }
        }

        config.additional_config = additional_config
            .into_iter()
            .map(|p| (p.0.to_owned(), p.1))
            .collect();
        config
    }
}
