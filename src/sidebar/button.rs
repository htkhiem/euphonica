use glib::{Object, Properties};
use gtk::{CompositeTemplate, Label, glib, prelude::*, subclass::prelude::*};
use std::cell::RefCell;

mod imp {
    use gtk::glib::WeakRef;

use super::*;

    #[derive(Properties, Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/sidebar-button.ui")]
    #[properties(wrapper_type = super::SidebarButton)]
    pub struct SidebarButton {
        #[template_child]
        pub label_widget: TemplateChild<Label>,
        #[template_child]
        pub prefix_box: TemplateChild<gtk::Box>,
        #[property(get, set)]
        pub label: RefCell<String>,
        #[property(get = Self::get_prefix_child, set = Self::set_prefix_child)]
        pub prefix_child: WeakRef<gtk::Widget>
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
        }
    }

    impl WidgetImpl for SidebarButton {}

    impl ButtonImpl for SidebarButton {}

    impl ToggleButtonImpl for SidebarButton {}

    impl SidebarButton {
        fn set_prefix_child(&self, prefix: gtk::Widget) {
            let prefix_box = self.prefix_box.get();
            while let Some(child) = prefix_box.first_child() {
                child.unparent();
            }
            prefix_box.append(&prefix);
            self.prefix_child.set(Some(&prefix));
        }

        fn get_prefix_child(&self) -> Option<gtk::Widget> {
            self.prefix_child.upgrade()
        }
    }
}

glib::wrapper! {
    pub struct SidebarButton(ObjectSubclass<imp::SidebarButton>)
    @extends gtk::ToggleButton, gtk::Button, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl SidebarButton {
    pub fn new(label: &str) -> Self {
        Object::builder().property("label", label).build()
    }
}
