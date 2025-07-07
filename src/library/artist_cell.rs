use glib::{closure_local, signal::SignalHandlerId, Object};
use gtk::{gdk, glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{Cache, CacheState},
    common::{Artist, ArtistInfo},
};

mod imp {
    use super::*;
    use glib::{ParamSpec, ParamSpecString};
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-cell.ui")]
    pub struct ArtistCell {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>, // Use high-resolution version
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        pub avatar_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub artist: RefCell<Option<Artist>>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ArtistCell {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaArtistCell";
        type Type = super::ArtistCell;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ArtistCell {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> =
                Lazy::new(|| vec![ParamSpecString::builder("name").build()]);
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "name" => obj.get_name().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "name" => {
                    if let Ok(name) = value.get::<&str>() {
                        obj.set_name(name);
                        obj.notify("name");
                    }
                }
                _ => unimplemented!(),
            }
        }

        fn dispose(&self) {
            if let Some((update_id, clear_id)) = self.avatar_signal_ids.take() {
                let cache = self
                    .cache
                    .get()
                    .unwrap()
                    .get_cache_state();
                cache.disconnect(update_id);
                cache.disconnect(clear_id);
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for ArtistCell {}

    // Trait shared by all boxes
    impl BoxImpl for ArtistCell {}
}

glib::wrapper! {
    pub struct ArtistCell(ObjectSubclass<imp::ArtistCell>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl ArtistCell {
    pub fn new(item: &gtk::ListItem, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.imp()
            .cache
            .set(cache)
            .expect("ArtistCell cannot bind to cache");
        res.setup(item);
        let cache_state = res.imp()
                .cache
                .get()
                .unwrap()
                .get_cache_state();
        let _ = res.imp().avatar_signal_ids.replace(Some((
            cache_state
                .connect_closure(
                    "artist-avatar-downloaded",
                    false,
                    closure_local!(
                        #[weak(rename_to = this)]
                        res,
                        move |_: CacheState, name: String| {
                            if let Some(artist) = this.imp().artist.borrow().as_ref() {
                                if artist.get_name() == &name {
                                    this.update_artist_avatar(artist.get_info());
                                }
                            }
                        }
                    ),
                ),
            cache_state
               .connect_closure(
                   "artist-avatar-cleared",
                   false,
                   closure_local!(
                       #[weak(rename_to = this)]
                       res,
                       move |_: CacheState, tag: String| {
                           if let Some(artist) = this.imp().artist.borrow().as_ref() {
                               if artist.get_name() == &tag {
                                   this.imp().avatar.set_custom_image(Option::<gdk::Texture>::None.as_ref());
                               }
                           }
                       }
                   ),
               ),
        )));
        res
    }

    #[inline(always)]
    pub fn setup(&self, item: &gtk::ListItem) {
        item.property_expression("item")
            .chain_property::<Artist>("name")
            .bind(self, "name", gtk::Widget::NONE);
    }

    fn update_artist_avatar(&self, info: &ArtistInfo) {
        self.imp().avatar.set_custom_image(
            self.imp()
                .cache
                .get()
                .unwrap()
                .load_cached_artist_avatar(info, false)
                .as_ref(),
        );
    }

    pub fn get_name(&self) -> glib::GString {
        self.imp().name.label()
    }

    pub fn set_name(&self, name: &str) {
        self.imp().name.set_label(name);
        self.imp().avatar.set_text(Some(name));
    }

    pub fn bind(&self, artist: &Artist) {
        let _ = self.imp().artist.replace(Some(artist.clone()));
        // Get state
        // Set once first (like sync_create)
        self.update_artist_avatar(artist.get_info());
    }

    pub fn unbind(&self) {
        self.imp().artist.replace(None).unwrap();
    }
}
