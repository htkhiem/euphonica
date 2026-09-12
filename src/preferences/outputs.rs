use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate,
    glib::{self, closure_local},
};
use rustc_hash::FxHashMap;
use std::cell::Cell;

use glib::Properties;

use crate::{
    preferences::output_row::OutputRow,
    server::config::{INTERNAL_FIFO_NAME, MpdConfig, OutputConfig},
};

mod imp {
    use super::*;

    #[derive(Debug, Default, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::AudioOutputs)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/outputs.ui")]
    pub struct AudioOutputs {
        #[template_child]
        pub error_banner: TemplateChild<adw::Banner>,
        #[template_child]
        pub listbox: TemplateChild<gtk::ListBox>,

        #[property(get)]
        pub is_valid: Cell<bool>,


    }

    #[glib::object_subclass]
    impl ObjectSubclass for AudioOutputs {
        const NAME: &'static str = "EuphonicaAudioOutputs";
        type Type = super::AudioOutputs;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for AudioOutputs {
        fn constructed(&self) {
            self.parent_constructed();


        }
    }
    impl WidgetImpl for AudioOutputs {}
    impl BoxImpl for AudioOutputs {}
}

glib::wrapper! {
    pub struct AudioOutputs(ObjectSubclass<imp::AudioOutputs>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for AudioOutputs {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AudioOutputs {
    pub fn init_from_config(&self, config: &MpdConfig) {
        let listbox = self.imp().listbox.get();
        listbox.remove_all();
        for output in config.audio_outputs.iter() {
            if &output.name != INTERNAL_FIFO_NAME {
                self.add(output, false);
            }
        }
        self.check();
    }

    /// Check for name collisions, including with the internal value.
    /// An empty name is acceptable though as long as it doesn't collide.
    fn check(&self) {
        let mut name_count: FxHashMap<String, usize> = FxHashMap::default();
        let _ = name_count.insert(INTERNAL_FIFO_NAME.to_string(), 1);
        if let Some(first) = self.imp().listbox.first_child() {
            let mut cursor: gtk::ListBoxRow = first.downcast().unwrap();
            loop {
                let child: OutputRow = cursor.child().and_downcast().unwrap();
                let name = child.name();
                name_count.entry(name).and_modify(|e| *e += 1).or_insert(1);
                if let Some(next) = cursor.next_sibling().and_downcast() {
                    cursor = next;
                } else {
                    break;
                }
            }
        } else {
            self.update_validity(true);
        }
        let mut duplicated = false;
        if let Some(first) = self.imp().listbox.first_child() {
            let mut cursor: gtk::ListBoxRow = first.downcast().unwrap();
            loop {
                let child: OutputRow = cursor.child().and_downcast().unwrap();
                let name = child.name();
                let collision = *name_count.get(&name).unwrap_or(&1) > 1;
                child.highlight_name_error(collision);
                duplicated = duplicated || collision;
                if let Some(next) = cursor.next_sibling().and_downcast() {
                    cursor = next;
                } else {
                    break;
                }
            }
        }
        self.update_validity(!duplicated);
    }

    pub fn add(&self, output: &OutputConfig, check_after_add: bool) {
        let output_row = OutputRow::new(output);
        let row = gtk::ListBoxRow::builder()
            .activatable(false)
            .child(&output_row)
            .build();
        output_row.bind_parent(&row);
        output_row.connect_closure(
            "delete-clicked",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: &OutputRow, idx: i32| {
                    if idx >= 0
                        && let Some(row) = this.imp().listbox.row_at_index(idx).as_ref()
                    {
                        this.imp().listbox.remove(row);
                    }
                }
            ),
        );
        output_row.connect_closure(
            "renamed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: &OutputRow| {
                    this.check();
                }
            ),
        );
        self.imp().listbox.append(&row);
        if check_after_add {
            self.check();
        }
    }

    /// Will always add a hidden FIFO output (first in list).
    pub fn get_config(&self) -> Vec<OutputConfig> {
        let mut res = vec![OutputConfig::internal_fifo()];
        if let Some(first) = self.imp().listbox.first_child() {
            let mut cursor: gtk::ListBoxRow = first.downcast().unwrap();
            loop {
                res.push(cursor.child().and_downcast::<OutputRow>().unwrap().generate_config());
                if let Some(next) = cursor.next_sibling().and_downcast() {
                    cursor = next;
                } else {
                    break;
                }
            }
        }
        res
    }

    fn update_validity(&self, new: bool) {
        let old = self.imp().is_valid.replace(new);
        if old != new {
            self.notify("is-valid");
        }
    }
}
