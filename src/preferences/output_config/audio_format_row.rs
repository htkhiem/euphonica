use adw::prelude::*;
use glib::{Object, clone};
use gtk::{CompositeTemplate, glib, subclass::prelude::*};

use crate::server::AudioFormatConfig;

mod imp {
    use std::sync::OnceLock;

    use gtk::glib::{WeakRef, subclass::Signal};

    use crate::preferences::AudioFormatEntry;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/preferences/audio-format-row.ui")]
    pub struct AudioFormatRow {
        #[template_child]
        pub order: TemplateChild<gtk::Label>,
        #[template_child]
        pub entry: TemplateChild<AudioFormatEntry>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,

        pub parent: WeakRef<gtk::ListBoxRow>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for AudioFormatRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaAudioFormatRow";
        type Type = super::AudioFormatRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AudioFormatRow {
        fn constructed(&self) {
            self.parent_constructed();
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
                ]
            })
        }
    }

    impl WidgetImpl for AudioFormatRow {}

    impl BoxImpl for AudioFormatRow {}
}

glib::wrapper! {
    pub struct AudioFormatRow(ObjectSubclass<imp::AudioFormatRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AudioFormatRow {
    pub fn from_format(config: &AudioFormatConfig) -> Self {
        let res: Self = Object::builder().build();
        res.imp().entry.load(config);
        res
    }

    pub fn bind_parent(&self, parent: &gtk::ListBoxRow) {
        self.imp().parent.set(Some(parent));
    }

    /// GtkListBoxRow doesn't have an "index" property so we can't bind.
    /// We'll rely on an external signal to update the index.
    pub fn update_index(&self) {
        if let Some(parent) = self.imp().parent.upgrade() {
            self.imp().order.set_label(&parent.index().to_string());
        }
    }

    pub fn generate_config(&self) -> AudioFormatConfig {
        self.imp().entry.generate_config()
    }
}
