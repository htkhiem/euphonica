use ::glib::clone;
use glib::{self, Object, SignalHandlerId};
use gtk::{CompositeTemplate, prelude::*, subclass::prelude::*};
use std::cell::RefCell;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/row-edit-buttons.ui")]
    pub struct RowEditButtons {
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,
        pub signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId, SignalHandlerId)>>,
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
    pub fn new<F1, F2, F3>(
        item: &gtk::ListItem,
        on_raise_clicked: F1,
        on_lower_clicked: F2,
        on_remove_clicked: F3,
    ) -> Self
    where
        F1: Fn(u32) + 'static,
        F2: Fn(u32) + 'static,
        F3: Fn(u32) + 'static,
    {
        let res: Self = Object::builder().build();
        res.imp().raise.connect_clicked(clone!(
            #[weak]
            item,
            #[upgrade_or]
            (),
            move |_| {
                on_raise_clicked(item.position());
            }
        ));

        res.imp().lower.connect_clicked(clone!(
            #[weak]
            item,
            #[upgrade_or]
            (),
            move |_| {
                on_lower_clicked(item.position());
            }
        ));

        res.imp().remove.connect_clicked(clone!(
            #[weak]
            item,
            #[upgrade_or]
            (),
            move |_| {
                on_remove_clicked(item.position());
            }
        ));

        res
    }
}
