use glib::clone;
use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*};

use crate::common::dynamic_playlist::Ordering;

mod imp {
    use once_cell::sync::OnceCell;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/ordering-button.ui")]
    pub struct OrderingButton {
        #[template_child]
        pub label: TemplateChild<gtk::Label>,
        pub ordering: OnceCell<Ordering>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for OrderingButton {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaOrderingButton";
        type Type = super::OrderingButton;
        type ParentType = gtk::Button;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for OrderingButton {}

    impl WidgetImpl for OrderingButton {}

    impl ButtonImpl for OrderingButton {}
}

glib::wrapper! {
    pub struct OrderingButton(ObjectSubclass<imp::OrderingButton>)
    @extends gtk::Button, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl OrderingButton {
    pub fn new(ordering: Ordering, wrap_box: &adw::WrapBox) -> Self {
        let res: Self = glib::Object::builder().build();
        let _ = res.imp().ordering.set(ordering);
        res.imp().label.set_label(ordering.readable_name());
        res.connect_clicked(clone!(
            #[weak]
            wrap_box,
            move |btn| {
                wrap_box.remove(btn);
            }
        ));

        res
    }

    pub fn ordering(&self) -> Ordering {
        *self.imp().ordering.get().unwrap()
    }
}
