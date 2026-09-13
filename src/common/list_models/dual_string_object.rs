use gtk::{
    glib::{self, Object, Properties},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::RefCell;

mod imp {
    use super::*;

    /// Bit like gtk::StringObject but containing separate strings for display and for internal use.
    #[derive(Properties, Default)]
    #[properties(wrapper_type = super::DualStringObject)]
    pub struct DualStringObject {
        #[property(get, set)]
        display: RefCell<String>,
        #[property(get, set)]
        internal: RefCell<String>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DualStringObject {
        const NAME: &'static str = "EuphonicaDualStringObject";
        type Type = super::DualStringObject;
    }

    #[glib::derived_properties]
    impl ObjectImpl for DualStringObject {}
}

// The public wrapper
glib::wrapper! {
    pub struct DualStringObject(ObjectSubclass<imp::DualStringObject>);
}

impl DualStringObject {
    pub fn new(display: &str, internal: &str) -> Self {
        Object::builder()
            .property("display", display)
            .property("internal", internal)
            .build()
    }
}
