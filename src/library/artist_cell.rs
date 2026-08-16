use gtk::{
    CompositeTemplate, gdk,
    glib::{
        self, Object, ParamSpec, ParamSpecBoolean, ParamSpecString, WeakRef, closure_local,
    },
    prelude::*,
    subclass::prelude::*,
};
use once_cell::sync::Lazy;
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{BACKLOG_THRESHOLD, Cache, placeholders::EMPTY_ARTIST_STRING},
    common::{Artist, FIRST_ATTEMPT_INTERVAL_MS, RE_ATTEMPT_INTERVAL_MS},
    utils::settings_manager,
};

// Lazy loading - eager unloading logic (RAM price crisis edition):
// Prev implementation was painfully clunky. I've since discovered the map() and unmap() signals.
// These fire as soon as a widget comes within/out of 10px of the viewport, i.e. almost ideal
// for texture loading.
// HOWEVER, map() seems to ALWAYS FIRE ONCE upon widget creation (maybe due to every new widget
// being created somewhere within the viewport, then moved out? or something to do with filtering being iterative?).
// Afterwards for widgets that end up outside of the viewport, unmap() will be fired. To avoid flooding the cache
// controller on startup we add a 10ms delay on the map() signal. If unmap() comes in quick
// succession then we can just cancel the load before it happens.

mod imp {

    use crate::common::{Artist, CoverFan};

    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-cell.ui")]
    pub struct ArtistCell {
        #[template_child]
        pub layout_switcher: TemplateChild<adw::MultiLayoutView>,
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub covers: TemplateChild<CoverFan>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        pub artist: WeakRef<Artist>,
        pub cache: OnceCell<Rc<Cache>>,
        pub hires: Cell<bool>,
        // Handle to the texture loading process. Can be used to abort early if unmap() is called
        // in quick succession (such as the case described in the comment block above all this mess,
        // or during fast scrolling).
        pub texture_load_handle: RefCell<Option<glib::JoinHandle<()>>>,
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
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").build(),
                    ParamSpecBoolean::builder("hires").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "name" => self.name.label().to_value(),
                "hires" => self.hires.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "name" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.name.set_label(name);
                        // No need to notify anyone
                    }
                }
                "hires" => {
                    if let Ok(new) = value.get::<bool>() {
                        obj.set_hires(new);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for ArtistCell {}

    impl BoxImpl for ArtistCell {}
}

glib::wrapper! {
    pub struct ArtistCell(ObjectSubclass<imp::ArtistCell>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl ArtistCell {
    pub fn new(
        item: &gtk::ListItem,
        cache: Rc<Cache>,
    ) -> Self {
        let res: Self = Object::builder().build();
        res.imp()
            .cache
            .set(cache)
            .expect("ArtistCell cannot bind to cache");
        item.property_expression("item")
            .chain_property::<Artist>("name")
            .chain_closure::<String>(closure_local!(
                |_: Option<glib::Object>, name: Option<&str>| {
                    String::from(if name.is_none_or(|t| t.is_empty()) {
                        *EMPTY_ARTIST_STRING
                    } else {
                        name.unwrap()
                    })
                }
            ))
            .bind(&res, "name", gtk::Widget::NONE);
        let ui_settings = settings_manager().child("ui");
        ui_settings
            .bind("use-hires-for-album-cells", &res, "hires")
            .get_only()
            .build();

        // Set up dynamic texture loading/unloading
        res.connect_map(|res| {
            res.try_load_textures_with_backlog();
        });

        res.connect_unmap(|res| {
            res.unload_textures(!res.imp().hires.get());
        });

        res
    }

    fn artist(&self) -> Option<Artist> {
        self.imp().artist.upgrade()
    }

    #[inline]
    fn unload_textures(&self, use_thumbnail: bool) {
        eprintln!("Unloading");
        let imp = self.imp();
        if let Some(handle) = imp.texture_load_handle.take() {
            handle.abort();
        }
        imp.avatar.set_custom_image(Option::<&gdk::Texture>::None);
        imp.covers.clear_cover(0, use_thumbnail);
        imp.covers.clear_cover(1, use_thumbnail);
        imp.covers.clear_cover(2, use_thumbnail);
    }

    #[inline]
    fn set_cover_count(&self, count: u8) {
        self.imp().covers.set_cover_count(count);
        if count > 0 {
            self.imp().layout_switcher.set_layout_name("covers");
        } else {
            self.imp().layout_switcher.set_layout_name("avatar-only");
        }
    }

    #[inline]
    async fn load_textures(&self, use_thumbnail: bool) {
        // If use_thumbnail and NOT due to backlog, load thumbnail then exit
        // If use thumbnail DUE TO backlog, load thumbnail then loop
        if let Some(artist) = self.artist() {
            let cache = self.imp().cache.get().unwrap();
            match cache
                .clone()
                .get_artist_avatar(artist.get_info(), use_thumbnail, false)
                .await
            {
                Ok(maybe_tex) => {
                    self.imp().avatar.set_custom_image(maybe_tex.as_ref());
                }
                Err(e) => {
                    dbg!(e);
                }
            };

            let example_uris = &artist.get_info().example_uris;
            let cover_fan = self.imp().covers.get();
            self.set_cover_count(example_uris.len().min(3) as u8);
            for (i, uri) in example_uris.iter().take(3).enumerate() {
                match cache.clone().get_album_cover_lite(uri, use_thumbnail).await {
                    Ok(Some(tex)) => cover_fan.set_cover(i.min(3) as u8, &tex),
                    _ => cover_fan.clear_cover(i.min(3) as u8, use_thumbnail),
                };
            }
        }
    }

    /// Attempt to load textures. If hires is enabled, will load hires ones only if not backlogged.
    /// If backlogged, will attempt to load again after a short pause.
    /// Unlike AlbumCell, ArtistCells might get quite complex so spinners aren't feasible.
    fn try_load_textures_with_backlog(&self) {
        if let Some(handle) = self.imp().texture_load_handle.take() {
            handle.abort();
        }
        let this = self.clone();
        let _ = self
            .imp()
            .texture_load_handle
            .replace(Some(glib::spawn_future_local(async move {
                // Wait first to give unmap() a chance to cancel this outright
                glib::timeout_future(FIRST_ATTEMPT_INTERVAL_MS).await;

                let imp = this.imp();
                if !imp.hires.get() {
                    // If hires is disabled: just load thumbnail then bail
                    this.load_textures(true).await;
                } else {
                    // If hires: loop attempt to load until no longer backlogged
                    let mut loaded_thumb = false;
                    loop {
                        // If hires is requested, check backlog to decide resolution.
                        if imp.cache.get().unwrap().backlog() >= *BACKLOG_THRESHOLD {
                            if !loaded_thumb {
                                this.load_textures(true).await;
                                eprintln!("Backlogged. Loaded thumbnail");
                                loaded_thumb = true;
                            } else {
                                eprintln!("Backlogged.");
                            }
                        } else {
                            
                            this.load_textures(false).await;
                            eprintln!("Loaded hires");
                            return;
                        }
                        // Wait after each turn
                        glib::timeout_future(RE_ATTEMPT_INTERVAL_MS).await;
                    }
                }
            })));
    }

    pub fn bind(&self, artist: &Artist) {
        let imp = self.imp();
        imp.artist.set(Some(artist));
    }

    pub fn unbind(&self) {
        self.unload_textures(true);
        self.imp().artist.set(None);
    }

    pub fn hires(&self) -> bool {
        self.imp().hires.get()
    }

    pub fn set_hires(&self, new: bool) {
        let old = self.imp().hires.replace(new);
        if old != new {
            self.notify("hires");

            self.try_load_textures_with_backlog()
        }
    }
}
