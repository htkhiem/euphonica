use glib::{clone, closure_local, Object, SignalHandlerId, ParamSpec, ParamSpecString};
use gtk::{gdk, glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, Cache, CacheState},
    common::{inode::INodeInfo, INode}
};

use super::Library;

mod imp {
    use super::*;
    use once_cell::sync::Lazy;

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
        pub thumbnail_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub library: OnceCell<Library>,
        pub playlist: RefCell<Option<INode>>,
        pub cache: OnceCell<Rc<Cache>>
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
                    ParamSpecString::builder("last-modified").build()
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

        fn dispose(&self) {
            if let Some((set_id, clear_id)) = self.thumbnail_signal_ids.take() {
                let cache_state = self.cache.get().unwrap().get_cache_state();
                cache_state.disconnect(set_id);
                cache_state.disconnect(clear_id);
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
    pub fn new(library: Library, item: &gtk::ListItem, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, item, cache);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &gtk::ListItem, cache: Rc<Cache>) {
        let cache_state = cache.get_cache_state();
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

        let _ = self.imp().thumbnail_signal_ids.replace(Some((
            cache_state.connect_closure(
                "playlist-cover-downloaded",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, name: String, thumb: bool, tex: gdk::Texture| {
                        if !thumb {
                            return;
                        }
                        if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                            if name.as_str() == playlist.get_uri() {
                                this.update_thumbnail(&tex);
                            }
                        }
                    }
                ),
            ),
            cache_state.connect_closure(
                "playlist-cover-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, name: String| {
                        if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                            if name.as_str() == playlist.get_uri() {
                                this.clear_thumbnail();
                            }
                        }
                    }
                ),
            ),
        )));

        self.imp().replace_queue.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {

                if let (Some(library), Some(playlist)) = (this.imp().library.get(), this.imp().playlist.borrow().as_ref()) {
                    library.queue_playlist(playlist.get_uri(), true, true);
                }
            }
        ));

        self.imp().append_queue.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let (Some(library), Some(playlist)) = (this.imp().library.get(), this.imp().playlist.borrow().as_ref()) {
                    library.queue_playlist(playlist.get_uri(), false, false);
                }
            }
        ));
    }

    fn clear_thumbnail(&self) {
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
    }

    fn schedule_thumbnail(&self, playlist: &INodeInfo) {
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
        if let Some(tex) = self
            .imp()
            .cache
            .get()
            .unwrap()
            .clone()
            .load_cached_playlist_cover(&playlist.uri, true) {
                self.imp().thumbnail.set_paintable(Some(&tex));
            }
    }

    fn update_thumbnail(&self, tex: &gdk::Texture) {
        self.imp().thumbnail.set_paintable(Some(tex));
    }

    pub fn bind(&self, playlist: &INode) {
        // Bind album art listener. Set once first (like sync_create)
        self.imp().playlist.replace(Some(playlist.clone()));
        self.schedule_thumbnail(playlist.get_info());
    }

    pub fn unbind(&self) {
        if let Some(_) = self.imp().playlist.take() {
            self.clear_thumbnail();
        }
    }
}
