use crate::{
    common::list_models::{DualStringList, DualStringObject},
    preferences::output_config::{audio_format_box::AudioFormatBox, path_row::PathRow},
    server::config::{ConfigValueType, ConfigValueTypeDiscriminants, OutputConfigSpec},
};
use adw::prelude::*;
use glib::Object;
use gtk::{
    glib::{self},
    subclass::prelude::*,
};
use std::cell::OnceCell;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct ConfigRow {
        pub enabled: OnceCell<gtk::Switch>, // not always populated
        pub inner_type: OnceCell<ConfigValueTypeDiscriminants>,
        pub key: OnceCell<&'static str>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ConfigRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaConfigRow";
        type Type = super::ConfigRow;
        type ParentType = gtk::ListBoxRow;
    }

    impl ObjectImpl for ConfigRow {
        fn constructed(&self) {
            self.parent_constructed();
            self.obj().add_css_class("padding-0");
        }
    }

    impl WidgetImpl for ConfigRow {}

    impl ListBoxRowImpl for ConfigRow {}
}

glib::wrapper! {
    pub struct ConfigRow(ObjectSubclass<imp::ConfigRow>)
    @extends gtk::ListBoxRow, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::Actionable, gtk::ConstraintTarget;
}

impl ConfigRow {
    pub fn from_config_spec(config_spec: OutputConfigSpec, value: Option<&str>) -> Self {
        let res: Self = Object::builder().build();

        let _ = res.imp().key.set(config_spec.key);
        let key = config_spec.key;
        let title = &config_spec.title;
        let subtitle = config_spec.subtitle.as_deref().unwrap_or("");

        let inner: gtk::Widget = match config_spec.value_type {
            ConfigValueType::Bool(default) => {
                let _ = res.imp().inner_type.set(ConfigValueTypeDiscriminants::Bool);
                adw::SwitchRow::builder()
                    .title(title)
                    .subtitle(subtitle)
                    .active(value.map_or(default, |val| val == "yes"))
                    .build()
                    .into()
            }
            ConfigValueType::Combo(options) => {
                let _ = res
                    .imp()
                    .inner_type
                    .set(ConfigValueTypeDiscriminants::Combo);
                let options = DualStringList::from_string_pairs(
                    &options
                        .iter()
                        .map(|p| (p.0.as_str(), p.1.as_str()))
                        .collect::<Vec<(&str, &str)>>(),
                );
                let prefix = gtk::Switch::builder().valign(gtk::Align::Center).build();
                let mut inner = adw::ComboRow::builder()
                    .title(title)
                    .subtitle(subtitle)
                    .model(&options)
                    .expression(&gtk::PropertyExpression::new(
                        DualStringObject::static_type(),
                        gtk::Expression::NONE,
                        "display",
                    ));
                if let Some(val) = value {
                    let init_idx = options.find(val);
                    if init_idx < options.n_items() {
                        inner = inner.selected(init_idx);
                        prefix.set_active(true);
                    }
                }
                let inner = inner.build();
                inner.add_prefix(&prefix);
                let _ = res.imp().enabled.set(prefix);
                inner.into()
            }
            ConfigValueType::Formats => {
                let _ = res
                    .imp()
                    .inner_type
                    .set(ConfigValueTypeDiscriminants::Formats);
                // Too complicated => custom widget lol
                AudioFormatBox::new(key, value.unwrap_or(""), title, subtitle).into()
            }
            ConfigValueType::Text => {
                let _ = res.imp().inner_type.set(ConfigValueTypeDiscriminants::Text);
                let prefix = gtk::Switch::builder()
                    .active(value.is_some())
                    .valign(gtk::Align::Center)
                    .build();
                let inner = adw::EntryRow::builder().title(title).build();
                inner.add_prefix(&prefix);
                if let Some(value) = value {
                    inner.set_text(value);
                }
                let _ = res.imp().enabled.set(prefix);
                inner.into()
            }
            ConfigValueType::Number(min, max, step, page, digits) => {
                let _ = res
                    .imp()
                    .inner_type
                    .set(ConfigValueTypeDiscriminants::Number);
                let prefix = gtk::Switch::builder()
                    .active(value.is_some())
                    .valign(gtk::Align::Center)
                    .build();
                let inner = adw::SpinRow::builder()
                    .title(title)
                    .subtitle(subtitle)
                    .adjustment(&gtk::Adjustment::new(
                        value
                            .map(|s| s.parse::<f64>().ok())
                            .flatten()
                            .unwrap_or_default(),
                        min,
                        max,
                        step,
                        page,
                        0.0,
                    ))
                    .digits(digits as u32)
                    .build();
                inner.add_prefix(&prefix);
                let _ = res.imp().enabled.set(prefix);
                inner.into()
            }
            ConfigValueType::Path => {
                // Too complicated => custom widget :)
                let _ = res.imp().inner_type.set(ConfigValueTypeDiscriminants::Path);
                PathRow::new(key, value, title).into()
            }
        };
        res.set_child(Some(&inner));
        // Forward activate signal so comborows work correctly.
        res
    }

    /// Needed for nested adw::ComboRows to function properly.
    pub fn activate_inner(&self) {
        if matches!(
            self.imp().inner_type.get().unwrap(),
            ConfigValueTypeDiscriminants::Combo
        ) {
            adw::prelude::ActionRowExt::activate(
                &self.child().unwrap().downcast::<adw::ComboRow>().unwrap(),
            );
        }
    }

    pub fn generate_config(&self) -> Option<(&'static str, String)> {
        let key: &'static str = *self.imp().key.get().unwrap();
        match self.imp().inner_type.get().unwrap() {
            &ConfigValueTypeDiscriminants::Bool => Some((
                key,
                if self
                    .child()
                    .unwrap()
                    .downcast::<adw::SwitchRow>()
                    .unwrap()
                    .is_active()
                {
                    "yes".to_owned()
                } else {
                    "no".to_owned()
                },
            )),
            &ConfigValueTypeDiscriminants::Combo => {
                if let (true, Some(val)) = (
                    self.imp().enabled.get().map_or(false, |r| r.is_active()),
                    self.child()
                        .unwrap()
                        .downcast::<adw::ComboRow>()
                        .unwrap()
                        .selected_item()
                        .and_downcast::<DualStringObject>(),
                ) {
                    Some((key, val.internal()))
                } else {
                    None
                }
            }
            &ConfigValueTypeDiscriminants::Formats => self
                .child()
                .unwrap()
                .downcast::<AudioFormatBox>()
                .unwrap()
                .generate_config(),
            &ConfigValueTypeDiscriminants::Number => {
                if self.imp().enabled.get().map_or(false, |r| r.is_active()) {
                    let inner = self.child().unwrap().downcast::<adw::SpinRow>().unwrap();
                    Some((
                        key,
                        format!("{0:.1$}", inner.value(), inner.digits() as usize),
                    ))
                } else {
                    None
                }
            }
            &ConfigValueTypeDiscriminants::Path => self
                .child()
                .unwrap()
                .downcast::<PathRow>()
                .unwrap()
                .generate_config(),
            &ConfigValueTypeDiscriminants::Text => {
                if self.imp().enabled.get().map_or(false, |r| r.is_active()) {
                    Some((
                        key,
                        self.child()
                            .unwrap()
                            .downcast::<adw::EntryRow>()
                            .unwrap()
                            .text()
                            .to_string(),
                    ))
                } else {
                    None
                }
            }
        }
    }
}
