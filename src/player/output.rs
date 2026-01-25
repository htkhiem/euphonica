use glib::Object;
use gtk::{
    CompositeTemplate,
    glib::{self, clone},
    prelude::*,
    subclass::prelude::*,
};
use mpd::output::Output;

use super::Player;

fn map_icon_name(plugin_name: &str) -> &'static str {
    match plugin_name {
        "alsa" => "alsa-symbolic",
        "pulse" => "pulseaudio-symbolic",
        "pipewire" => "pipewire-symbolic",
        _ => "soundcard-symbolic",
    }
}

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/output.ui")]
    pub struct MpdOutput {
        #[template_child]
        pub icon_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub enable_output: TemplateChild<gtk::Switch>,
        #[template_child]
        pub options_preview: TemplateChild<gtk::Label>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for MpdOutput {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaMpdOutput";
        type Type = super::MpdOutput;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for MpdOutput {}

    // Trait shared by all widgets
    impl WidgetImpl for MpdOutput {}

    // Trait shared by all boxes
    impl BoxImpl for MpdOutput {}
}

glib::wrapper! {
    pub struct MpdOutput(ObjectSubclass<imp::MpdOutput>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl MpdOutput {
    fn set_dim(&self) {
        let icon = self.imp().icon.get();
        let label = self.imp().name.get();
        let is_dimmed = icon.has_css_class("dim-label");
        let is_enabled = self.imp().enable_output.is_active();
        if is_enabled && is_dimmed {
            icon.remove_css_class("dim-label");
            label.remove_css_class("dim-label");
        } else if !is_enabled && !is_dimmed {
            icon.add_css_class("dim-label");
            label.add_css_class("dim-label");
        }
    }

    pub fn update_state(&self, output: &Output) {
        // Get state
        let imp = self.imp();
        let name = imp.name.get();
        let icon = imp.icon.get();
        let enable_output = imp.enable_output.get();
        let options_preview = imp.options_preview.get();

        name.set_label(&output.name);
        if enable_output.is_active() != output.enabled {
            enable_output.set_active(output.enabled);
        }
        icon.set_icon_name(Some(map_icon_name(&output.plugin)));
        if !output.attributes.is_empty() {
            // Big TODO: editable runtime attributes
            let mut attribs: Vec<String> = Vec::with_capacity(output.attributes.len());
            for (k, v) in output.attributes.iter() {
                attribs.push(format!("<b>{k}</b>: {v}"));
            }

            options_preview.set_visible(true);
            options_preview.set_label(&attribs.join("\n"));
        } else {
            options_preview.set_label("");
            options_preview.set_visible(false);
        }
        self.set_dim();
    }

    pub fn from_output(output: &Output, player: &Player) -> Self {
        let res: Self = Object::builder().build();
        res.update_state(output);

        let id = output.id;
        res.imp().enable_output.connect_active_notify(clone!(
            #[weak] player,
            move |sw| {
                let active = sw.is_active();
                glib::spawn_future_local(clone!(
                    #[weak] player,
                    async move {
                        match player.set_output(id, active).await {
                            Ok(()) => {}
                            Err(e) => {dbg!(e);}
                        }
                    }
                ));
            }
        ));

        res
    }
}
