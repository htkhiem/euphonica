use glib::{Object, ParamSpec, ParamSpecString, clone};
use gtk::{CompositeTemplate, glib, prelude::*, subclass::prelude::*};
use once_cell::sync::Lazy;
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{Cache, placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER},
    common::INode,
};

use super::Library;

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/playlist-row.ui")]
    pub struct PlaylistRow {
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub thumbnail: TemplateChild<gtk::Image>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        // #[template_child]
        // pub count: TemplateChild<gtk::Label>,
        // #[template_child]
        // pub duration: TemplateChild<gtk::Label>,
        #[template_child]
        pub last_modified: TemplateChild<gtk::Label>,
        pub library: OnceCell<Library>,
        pub playlist: RefCell<Option<INode>>,
        pub is_dynamic: Cell<bool>,
        pub cache: OnceCell<Rc<Cache>>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPlaylistRow";
        type Type = super::PlaylistRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for PlaylistRow {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("last-modified").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "name" => self.name.label().to_value(),
                "last-modified" => self.last_modified.label().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "name" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(name) = value.get::<&str>() {
                        self.name.set_label(name);
                    }
                }
                "last-modified" => {
                    if let Ok(lm) = value.get::<&str>() {
                        self.last_modified.set_label(lm);
                    } else {
                        self.last_modified.set_label("");
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlaylistRow {}

    // Trait shared by all boxes
    impl BoxImpl for PlaylistRow {}
}

glib::wrapper! {
    pub struct PlaylistRow(ObjectSubclass<imp::PlaylistRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl PlaylistRow {
    pub fn new(is_dynamic: bool, library: Library, item: &gtk::ListItem, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.imp().is_dynamic.set(is_dynamic);
        res.setup(library, item, cache);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &gtk::ListItem, cache: Rc<Cache>) {
        self.imp()
            .cache
            .set(cache)
            .expect("PlaylistRow cannot bind to cache");
        let _ = self.imp().library.set(library);
        item.property_expression("item")
            .chain_property::<INode>("uri")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<INode>("last-modified")
            .bind(self, "last-modified", gtk::Widget::NONE);

        self.imp().replace_queue.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(#[weak] this, async move {
                    if let (Some(library), Some(playlist)) = (
                        this.imp().library.get(),
                        this.imp().playlist.borrow().as_ref(),
                    ) {
                        if let Err(e) = library.queue_playlist(playlist.get_uri().to_owned(), true, true).await {
                            dbg!(e);
                        }
                    }
                }));
            }
        ));

        self.imp().append_queue.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(#[weak] this, async move {
                    if let (Some(library), Some(playlist)) = (
                    this.imp().library.get(),
                    this.imp().playlist.borrow().as_ref(),
                ) {
                    if let Err(e) = library.queue_playlist(playlist.get_uri().to_owned(), false, false).await {
                        dbg!(e);
                    }
                }
                }));
            }
        ));
    }

    #[inline]
    fn clear_thumbnail(&self) {
        self.imp()
            .thumbnail
            .set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
    }

    fn uri(&self) -> Option<String> {
        self.imp().playlist.borrow().as_ref().map(|p| p.get_uri().to_owned())
    }

    pub fn bind(&self, playlist: &INode) {
        // Bind album art listener. Set once first (like sync_create)
        self.imp().playlist.replace(Some(playlist.clone()));
        self.clear_thumbnail();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            playlist,
            async move {
                let uri = playlist.get_uri().to_owned();
                let res = this.imp().cache.get().unwrap().get_playlist_cover(
                    uri.clone(),
                    this.imp().is_dynamic.get(),
                    true,
                ).await;
                // Check again as row might have been bound to a different playlist
                // while awaiting
                if this.uri().is_some_and(|curr_uri| curr_uri == uri) {
                    match res {
                        Ok(Some(tex)) => {
                            this.imp()
                                .thumbnail
                                .set_paintable(Some(&tex));
                        }
                        Ok(None) => {
                            this.clear_thumbnail();
                        }
                        Err(e) => {dbg!(e);}
                    }
                } else {
                    println!("PlaylistRow now bound to a different playlist, ignoring texture");
                }
            }
        ));
    }

    pub fn unbind(&self) {
        if let Some(_) = self.imp().playlist.take() {
            self.clear_thumbnail();
        }
    }
}
