use glib::{Object, Properties};
use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate, Image, Label};
use std::cell::RefCell;

mod imp {
    use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/sidebar-button.ui")]
    #[properties(wrapper_type = super::SidebarButton)]
    pub struct SidebarButton {
        #[template_child]
        pub label_widget: TemplateChild<Label>,
        #[template_child]
        pub icon_widget: TemplateChild<Image>,
        #[property(get, set)]
        pub label: RefCell<String>,
        #[property(get, set)]
        pub icon_name: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SidebarButton {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaSidebarButton";
        type Type = super::SidebarButton;
        type ParentType = gtk::ToggleButton;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for SidebarButton {
        fn constructed(&self) {
            self.parent_constructed();

            // `SYNC_CREATE` ensures that the label will be immediately set
            let obj = self.obj();
            obj.bind_property("label", &obj.imp().label_widget.get(), "label")
                .sync_create()
                .build();

            obj.bind_property("icon_name", &obj.imp().icon_widget.get(), "icon-name")
                .sync_create()
                .build();
        }
    }

    impl WidgetImpl for SidebarButton {}

    impl ButtonImpl for SidebarButton {}

    impl ToggleButtonImpl for SidebarButton {}
}

glib::wrapper! {
    pub struct SidebarButton(ObjectSubclass<imp::SidebarButton>)
    @extends gtk::ToggleButton, gtk::Button, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl SidebarButton {
    pub fn new(label: &str, icon_name: &str) -> Self {
        Object::builder()
            .property("label", label)
            .property("icon_name", icon_name)
            .build()
    }
}
