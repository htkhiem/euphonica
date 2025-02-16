use glib::{Object, Properties};
use gtk::{
    glib::{self, clone},
    prelude::*,
    subclass::prelude::*,
    CompositeTemplate,
};
use mpd::output::Output;
use std::cell::Cell;

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

    #[derive(Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::MpdOutput)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/output.ui")]
    pub struct MpdOutput {
        #[template_child]
        pub toggle_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub options: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub options_preview: TemplateChild<gtk::Label>,
        #[property(get, set)]
        pub enabled: Cell<bool>,
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

    #[glib::derived_properties]
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
        let is_enabled = self.imp().enabled.get();
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
        let options = imp.options.get();
        let options_preview = imp.options_preview.get();

        name.set_label(&output.name);
        icon.set_icon_name(Some(map_icon_name(&output.plugin)));
        let _ = self.imp().enabled.replace(output.enabled);
        if output.attributes.len() > 0 {
            // Big TODO: editable runtime attributes
            options.set_visible(true);
            let mut attribs: Vec<String> = Vec::with_capacity(output.attributes.len());
            for (k, v) in output.attributes.iter() {
                println!("<b>{}</b>: {}", k, v);
                attribs.push(format!("<b>{}</b>: {}", k, v));
            }

            options_preview.set_label(&attribs.join("\n"));
        } else {
            options.set_visible(false);
        }
        self.set_dim();
    }

    pub fn from_output(output: &Output, player: &Player) -> Self {
        let res: Self = Object::builder().build();
        res.update_state(output);

        let id = output.id;
        let toggle_btn = res.imp().toggle_btn.get();
        toggle_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            res,
            #[weak]
            player,
            move |_| {
                let was_enabled = this.imp().enabled.get();
                let _ = this.imp().enabled.replace(!was_enabled);
                this.set_dim();
                player.set_output(id, !was_enabled);
            }
        ));

        res
    }
}
