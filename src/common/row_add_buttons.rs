use ::glib::clone;
use glib::Object;
use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*};
use std::cell::RefCell;

use crate::{common::Song, library::Library};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/row-add-buttons.ui")]
    pub struct RowAddButtons {
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        pub song: RefCell<Option<Song>>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for RowAddButtons {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaRowAddButtons";
        type Type = super::RowAddButtons;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for RowAddButtons {}

    // Trait shared by all widgets
    impl WidgetImpl for RowAddButtons {}

    // Trait shared by all boxes
    impl BoxImpl for RowAddButtons {}
}

glib::wrapper! {
    pub struct RowAddButtons(ObjectSubclass<imp::RowAddButtons>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl RowAddButtons {
    pub fn new(library: &Library) -> Self {
        let res: Self = Object::builder().build();

        res.imp().replace_queue.connect_clicked(clone!(
            #[weak]
            res,
            #[weak]
            library,
            #[upgrade_or]
            (),
            move |_| {
                if let Some(song) = res.imp().song.borrow().as_ref() {
                    library.queue_uri(song.get_uri(), true, true, false);
                }
            }
        ));

        res.imp().append_queue.connect_clicked(clone!(
            #[weak]
            res,
            #[weak]
            library,
            #[upgrade_or]
            (),
            move |_| {
                if let Some(song) = res.imp().song.borrow().as_ref() {
                    library.queue_uri(song.get_uri(), false, false, false);
                }
            }
        ));

        res
    }

    pub fn set_song(&self, song: Option<&Song>) {
        self.imp().song.replace(song.cloned());
    }
}
