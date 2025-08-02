use glib::{closure_local, signal::SignalHandlerId, Object, ParamSpec, ParamSpecString, clone};
use gtk::{gdk, glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{Cache, CacheState},
    common::{Artist, ArtistInfo}, window::EuphonicaWindow,
};

mod imp {
    use super::*;
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-tag.ui")]
    pub struct ArtistTag {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>, // Use high-resolution version
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        pub avatar_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub artist: OnceCell<Artist>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for ArtistTag {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaArtistTag";
        type Type = super::ArtistTag;
        type ParentType = gtk::Button;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ArtistTag {
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

    impl WidgetImpl for ArtistTag {}

    impl ButtonImpl for ArtistTag {}
}

glib::wrapper! {
    pub struct ArtistTag(ObjectSubclass<imp::ArtistTag>)
    @extends gtk::Button, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Actionable;
}

impl ArtistTag {
    pub fn new(artist: Artist, cache: Rc<Cache>, window: &EuphonicaWindow) -> Self {
        let res: Self = Object::builder().build();
        let cache_state = cache.get_cache_state();
        res.imp()
            .cache
            .set(cache)
            .expect("ArtistTag cannot bind to cache");

        res.imp().name.set_label(artist.get_name());

        res.imp()
            .artist
            .set(artist)
            .expect("ArtistTag cannot bind to Artist object");

        let _ = res.imp().avatar_signal_ids.replace(Some((
            cache_state
                .connect_closure(
                    "artist-avatar-downloaded",
                    false,
                    closure_local!(
                        #[weak(rename_to = this)]
                        res,
                        move |_: CacheState, name: String, thumb: bool, tex: gdk::Texture| {
                            if !thumb {
                                return;
                            }
                            if this.imp().artist.get().unwrap().get_name() == &name {
                                this.imp().avatar.set_custom_image(Some(&tex));
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
                           if this.imp().artist.get().unwrap().get_name() == &tag {
                               this.imp().avatar.set_custom_image(Option::<gdk::Texture>::None.as_ref());
                           }
                       }
                   ),
               ),
        )));
        res.update_artist_avatar(res.imp().artist.get().unwrap().get_info());

        res.connect_clicked(clone!(
            #[weak(rename_to = this)]
            res,
            #[weak]
            window,
            move |_| {
                window.goto_artist(this.imp().artist.get().unwrap());
            }
        ));

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
                .clone()
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
}
