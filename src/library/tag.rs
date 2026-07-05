use gtk::{
    glib::{self, Object, ParamSpec, ParamSpecString},
    prelude::*,
    subclass::prelude::*,
};
use std::cell::{OnceCell, Cell};
use once_cell::sync::Lazy;

use crate::meta_providers::models;

mod imp {
    use super::*;
    
    #[derive(Default)]
    pub struct Tag {
        pub name: OnceCell<String>,
        // #[property(get, set)]
        pub count: Cell<i32>,
        // #[property(get)]
        pub link: OnceCell<String>,
        // #[property(get, set)]
        pub removable: Cell<bool>,
        // #[property(get, set)]
        pub set_by_user: Cell<bool>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for Tag {
        const NAME: &'static str = "EuphonicaTag";
        type Type = super::Tag;
        type ParentType = glib::Object;
    }

    // #[glib::derived_properties]
    impl ObjectImpl for Tag {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").read_only().build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "name" => obj.name().to_value(),
                _ => unimplemented!()
            }
        }
    }
}

glib::wrapper! {
    pub struct Tag(ObjectSubclass<imp::Tag>);
}

impl Tag {
    pub fn new(
        name: String,
        link: Option<String>,
        count: Option<i32>,
        removable: bool,
        set_by_user: bool,
    ) -> Self {
        let res: Self = Object::builder().build();
        res.imp().count.set(count.unwrap_or(1));
        res.imp().name.set(name);
        if let Some(link) = link {
            res.imp().link.set(link);
        }
        res.imp().removable.set(removable);
        res.imp().set_by_user.set(set_by_user);
        res
    }

    pub fn name(&self) -> &str {
        self.imp().name.get().unwrap()
    }

    pub fn count(&self) -> i32 {
        self.imp().count.get()
    }

    pub fn link(&self) -> Option<&str> {
        self.imp().link.get().map(|s| s.as_str())
    }

    pub fn removable(&self) -> bool {
        self.imp().removable.get()
    }

    pub fn set_by_user(&self) -> bool {
        self.imp().set_by_user.get()
    }

    pub fn to_meta(&self) -> models::Tag {
        models::Tag {
            url: self.link().map(|l| l.to_owned()),
            name: self.name().to_owned(),
            count: Some(self.count()),
            set_by_user: self.imp().set_by_user.get()
        }
    }
}

impl From<models::Tag> for Tag {
    fn from(value: models::Tag) -> Self {
        Self::new(
            value.name,
            value.url,
            value.count,
            true,  // 'false' is only used when representing in-file tags that can't be modified via MPD, such as genres
            value.set_by_user
        )
    }
}