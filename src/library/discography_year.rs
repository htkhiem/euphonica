use derivative::Derivative;
use glib::Object;
use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::OnceCell,
    rc::Rc,
};

use crate::{
    EuphonicaWindow, cache::Cache, common::{Album, Song}, library::discography_album::DiscographyAlbum, utils::settings_manager,
};

use super::Library;

// Wrapper around the common row object to implement song thumbnail fetch logic.
mod imp {
    use super::*;

    #[derive(Derivative, CompositeTemplate)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/discography-year.ui")]
    pub struct DiscographyYear {
        #[template_child]
        pub release_year: TemplateChild<gtk::Label>,
        #[template_child]
        pub toggle_collapse: TemplateChild<gtk::Button>,
        #[template_child]
        pub collapse_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub albums_box: TemplateChild<gtk::ListBox>,

        pub year: OnceCell<i32>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for DiscographyYear {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaDiscographyYear";
        type Type = super::DiscographyYear;
        type ParentType = gtk::Grid;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for DiscographyYear {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            let revealer = self.revealer.get();
            revealer.bind_property(
                "child-revealed",
                &self.collapse_icon.get(),
                "icon-name"
            ).transform_to(|_, is_revealed| {
                Some(
                    if is_revealed {
                        "up-symbolic"
                    } else {
                        "down-symbolic"
                    }.to_value()
                )
            }).sync_create().build();

            revealer.set_reveal_child(
                !settings_manager()
                    .child("ui")
                    .boolean("artist-collapse-discography-years-on-load"),
            );

            self.toggle_collapse.connect_clicked(move |_| {
                revealer.set_reveal_child(!revealer.is_child_revealed());
            });
        }
    }

    impl WidgetImpl for DiscographyYear {}

    impl GridImpl for DiscographyYear {}
}

// Common row widget for displaying a single song, used across the UI.
glib::wrapper! {
    pub struct DiscographyYear(ObjectSubclass<imp::DiscographyYear>)
    @extends gtk::Grid, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl DiscographyYear {
    pub fn new(
        year: Option<i32>,  // will default to "unknown release year"
        albums_with_songs: Vec<(Option<Album>, Vec<Song>)>,
        cache: Rc<Cache>,
        library: &Library,
        window: Option<&EuphonicaWindow>
    ) -> Self {
        let res: Self = Object::builder().build();
        if let Some(year) = year {
            let _ = res.imp().year.set(year);
            res.imp().release_year.set_label(&year.to_string());  
        }
        for (idx, (maybe_album, songs)) in albums_with_songs.into_iter().enumerate() {
            res.imp().albums_box.append(
                &DiscographyAlbum::new(
                    maybe_album, &songs, cache.clone(), library, window
                )
            );
            res.imp().albums_box.row_at_index(idx as i32).unwrap().set_activatable(false);
        }

        res
    }

    pub fn year(&self) -> Option<i32> {
        // If null, caller should treat as "unknown release year"
        self.imp().year.get().copied()
    }

    pub fn albums_box(&self) -> gtk::ListBox {
        self.imp().albums_box.get()
    }
}
