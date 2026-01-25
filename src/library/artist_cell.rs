use once_cell::sync::Lazy;
use std::{
    cell::{OnceCell, RefCell, Cell},
    rc::Rc,
};

use glib::{clone, Object, closure_local, signal::SignalHandlerId, ParamSpec, ParamSpecString, WeakRef};
use gtk::{CompositeTemplate, gdk, glib, prelude::*, subclass::prelude::*};

use crate::{
    cache::{Cache, CacheState, placeholders::EMPTY_ARTIST_STRING},
    common::Artist,
};

mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-cell.ui")]
    pub struct ArtistCell {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>, // Use high-resolution version
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        pub avatar_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub artist: WeakRef<Artist>,
        pub external: Cell<bool>
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
                let cache = self.cache.get().unwrap().get_cache_state();
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
    pub fn new(item: &gtk::ListItem, cache: Rc<Cache>, external: bool) -> Self {
        let res: Self = Object::builder().build();
        res.imp().external.set(external);
        res.imp()
            .cache
            .set(cache)
            .expect("ArtistCell cannot bind to cache");
        res.setup(item);
        let cache_state = res.imp().cache.get().unwrap().get_cache_state();
        let _ = res.imp().avatar_signal_ids.replace(Some((
            cache_state.connect_closure(
                "artist-avatar-set",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    res,
                    move |_: CacheState, name: String, _: gdk::Texture, thumb: gdk::Texture| {
                        if this.imp().artist.upgrade().is_some_and(|a| a.get_name() == name) {
                            this.update_avatar(Some(&thumb));
                        }
                    }
                ),
            ),
            cache_state.connect_closure(
                "artist-avatar-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    res,
                    move |_: CacheState, name: String| {
                        if this.imp().artist.upgrade().is_some_and(|a| a.get_name() == name) {
                            this.update_avatar(None);
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
            .chain_closure::<String>(closure_local!(
                |_: Option<glib::Object>, artist: Option<&str>| {
                    String::from(if artist.is_none_or(|a| a.is_empty()) {
                        *EMPTY_ARTIST_STRING
                    } else {
                        artist.unwrap()
                    })
                }
            ))
            .bind(self, "name", gtk::Widget::NONE);
    }

    fn update_avatar(&self, tex: Option<&gdk::Texture>) {
        self.imp().avatar.set_custom_image(tex);
    }

    pub fn get_name(&self) -> glib::GString {
        self.imp().name.label()
    }

    pub fn set_name(&self, name: &str) {
        self.imp().name.set_label(name);
        self.imp().avatar.set_text(Some(name));
    }

    pub fn bind(&self, artist: &Artist) {
        self.imp().artist.set(Some(artist));
        // Try to get from cache (or from disk asynchronously)
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            artist,
            async move {
                let res = this.imp().cache.get().unwrap().clone().get_artist_avatar(
                    artist.get_info(), true,
                    this.imp().external.get()
                ).await;
                // Check again as row might have been bound to a different playlist
                // while awaiting
                if this.imp().artist.upgrade().is_some_and(
                    |a| a.get_info().get_comp_id() == artist.get_info().get_comp_id()
                ) {
                    match res {
                        Ok(maybe_tex) => {
                            this.update_avatar(maybe_tex.as_ref());
                        }
                        Err(e) => {dbg!(e);}
                    }
                }
            }
        ));

    }

    pub fn unbind(&self) {
        self.imp().artist.set(None);
    }
}
