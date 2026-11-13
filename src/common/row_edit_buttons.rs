use gtk::{
    CompositeTemplate,
    glib::{self, Object},
    prelude::*,
    subclass::prelude::*,
};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/row-edit-buttons.ui")]
    pub struct RowEditButtons {
        #[template_child]
        pub reorder_buttons: TemplateChild<gtk::Box>,
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for RowEditButtons {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaRowEditButtons";
        type Type = super::RowEditButtons;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for RowEditButtons {}

    // Trait shared by all widgets
    impl WidgetImpl for RowEditButtons {}

    // Trait shared by all boxes
    impl BoxImpl for RowEditButtons {}
}

glib::wrapper! {
    pub struct RowEditButtons(ObjectSubclass<imp::RowEditButtons>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RowEditButtons {
    pub fn reorder_buttons(&self) -> gtk::Box {
        self.imp().reorder_buttons.get()
    }

    pub fn new(
        on_raise_clicked: impl Fn(&gtk::Button) + 'static,
        on_lower_clicked: impl Fn(&gtk::Button) + 'static,
        on_remove_clicked: impl Fn(&gtk::Button) + 'static,
    ) -> Self {
        let res: Self = Object::builder().build();
        res.imp().raise.connect_clicked(on_raise_clicked);
        res.imp().lower.connect_clicked(on_lower_clicked);
        res.imp().remove.connect_clicked(on_remove_clicked);
        res
    }
}
