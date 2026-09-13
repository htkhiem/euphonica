use super::DualStringObject;
use gtk::{
    gio::{self, prelude::*, subclass::prelude::*},
    glib::{self},
};
use std::cell::RefCell;

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct DualStringList {
        pub items: RefCell<Vec<DualStringObject>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DualStringList {
        const NAME: &'static str = "EuphonicaDualStringList";
        type Type = super::DualStringList;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for DualStringList {}

    // from gio
    impl ListModelImpl for DualStringList {
        fn item_type(&self) -> glib::Type {
            DualStringObject::static_type()
        }

        fn n_items(&self) -> u32 {
            self.items.borrow().len() as u32
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            self.items
                .borrow()
                .get(position as usize)
                // Lightweight cuz gobject ~ refcount clone
                .map(|o| o.clone().upcast::<glib::Object>())
        }
    }
}

glib::wrapper! {
    pub struct DualStringList(ObjectSubclass<imp::DualStringList>)
        @implements gio::ListModel;
}

impl DualStringList {
    pub fn new() -> Self {
        glib::Object::builder().build()
    }

    /// String order: display and internal
    pub fn from_string_pairs(pairs: &[(&str, &str)]) -> Self {
        let res: Self = glib::Object::builder().build();
        let objs = pairs
            .iter()
            .map(|(display, internal)| DualStringObject::new(*display, *internal))
            .collect();
        res.imp().items.replace(objs);
        res
    }

    pub fn append(&self, display: &str, internal: &str) {
        let obj = DualStringObject::new(display, internal);
        let imp = glib::subclass::prelude::ObjectSubclassIsExt::imp(self);

        let position = imp.items.borrow().len() as u32;
        imp.items.borrow_mut().push(obj);

        // Notify the UI that 1 item was added at `position`
        self.items_changed(position, 0, 1);
    }

    // Optional helper to get an item back cleanly typed
    pub fn item(&self, position: u32) -> Option<DualStringObject> {
        self.imp().item(position).and_downcast::<DualStringObject>()
    }
}
