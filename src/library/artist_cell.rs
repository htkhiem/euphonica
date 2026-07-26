use gtk::{
    CompositeTemplate, gdk,
    glib::{
        self, Object, ParamSpec, ParamSpecBoolean, ParamSpecString,
        WeakRef, clone, closure_local, signal::SignalHandlerId,
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
    cache::{
        BACKLOG_THRESHOLD, Cache,
        placeholders::EMPTY_ARTIST_STRING,
    },
    common::{
        WING_DEPTH, Artist,
    },
    utils::settings_manager,
    window::EuphonicaWindow,
};

// As soon as a cell comes within this close of the render area, treat it as
// visible & load album art early to avoid showing loading spinners.
mod imp {
    

    use crate::common::{Artist, CoverFan};

    use super::*;

    #[derive(CompositeTemplate, Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-cell.ui")]
    pub struct ArtistCell {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub covers: TemplateChild<CoverFan>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        pub artist: WeakRef<Artist>,
        pub cache: OnceCell<Rc<Cache>>,
        pub hires: Cell<bool>,
        // Stored GridView for visibility checks in bind() and the signal handler.
        pub viewport: OnceCell<WeakRef<gtk::GridView>>,
        // Weak reference to the window for connecting to the check-visible signal.
        pub window: WeakRef<EuphonicaWindow>,
        // Signal handler ID for disconnecting from the window's check-visible signal.
        pub check_visible_handler: RefCell<Option<SignalHandlerId>>,
        // Tracks whether the cell is currently within the viewport, used
        // to detect visibility transitions (entering/exiting the viewport).
        pub should_load_texture: Cell<bool>,
        // Use this to block loading images immediately upon construction when hires
        // is set to true (as that would trigger the setter before a viewport is set)
        pub obj_ready: Cell<bool>,
        // Set to true when the bind-time visibility check was skipped due to
        // backpressure.
        pub deferred: Cell<bool>,
        // Set to true when hires was deferred due to high backlog; triggers an
        // upgrade to hires once the backlog drops below the threshold.
        pub deferred_hires: Cell<bool>,
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

            if let (Some(handler), Some(window)) =
                (self.check_visible_handler.take(), self.window.upgrade())
            {
                window.disconnect(handler);
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
        window: Option<crate::window::EuphonicaWindow>,
        viewport: Option<gtk::GridView>,
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

        // Set up dynamic texture loading if a GridView is available.
        if let Some(vp) = viewport {
            let weak_vp = WeakRef::new();
            weak_vp.set(Some(&vp));
            let _ = res.imp().viewport.set(weak_vp);
        }

        // Store weak reference to the window for signal connection.
        res.imp().window.set(window.as_ref());

        // Connect to the window's check-visible signal for visibility checks.
        if let Some(window) = window {
            let handler = window.connect_closure(
                "check-visible",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    res,
                    move |_: &EuphonicaWindow| {
                        let imp = this.imp();
                        let is_visible = this.should_load_texture();
                        let was_visible = imp.should_load_texture.replace(is_visible);

                        if is_visible {
                            // Also go through this if visibility status didn't change,
                            // but album art load hasn't been attempted yet.
                            if was_visible != is_visible || imp.deferred.get() {
                                imp.deferred.set(false);
                                if imp.artist.upgrade().is_some() {
                                    glib::idle_add_local_once(clone!(
                                        #[weak]
                                        this,
                                        move || {
                                            this.update_textures();
                                        }
                                    ));
                                }
                            } else if imp.deferred_hires.get() {
                                this.try_upgrade_hires();
                            }
                        } else if was_visible != is_visible {
                            this.unload_textures();
                        }
                    }
                ),
            );
            res.imp().check_visible_handler.replace(Some(handler));
        }

        res.imp().obj_ready.set(true);

        res
    }

    fn artist(&self) -> Option<Artist> {
        self.imp().artist.upgrade()
    }

    #[inline]
    fn unload_textures(&self) {
        let imp = self.imp();
        let thumb = !imp.hires.get();
        imp.avatar.set_custom_image(Option::<&gdk::Texture>::None);
        imp.covers.clear_cover(0, thumb);
        imp.covers.clear_cover(1, thumb);
        imp.covers.clear_cover(2, thumb);
    }

    /// Unlike AlbumCell, ArtistCells might get quite complex so spinners aren't feasible
    fn update_textures(&self) {
        let imp = self.imp();

        // If hires is requested, check backlog to decide resolution.
        if imp.hires.get() {
            let backlog = imp.cache.get().unwrap().backlog();
            if backlog >= *BACKLOG_THRESHOLD {
                // Too many pending tasks; defer hires and load thumbnail instead.
                imp.deferred_hires.set(true);
            }
        } else {
            imp.deferred_hires.set(false);
        }

        let thumbnail_for_fetch = !imp.hires.get() || imp.deferred_hires.get();
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Some(artist) = this.artist() {
                    let cache = this.imp().cache.get().unwrap();
                    match cache.clone().get_artist_avatar(
                        artist.get_info(),
                        thumbnail_for_fetch,
                        false,
                    ).await {
                        Ok(maybe_tex) => {
                            this.imp().avatar.set_custom_image(maybe_tex.as_ref());
                        }
                        Err(e) => {
                            dbg!(e);
                        }
                    };

                    let example_uris = &artist.get_info().example_uris;
                    let cover_fan = this.imp().covers.get();
                    cover_fan.set_cover_count(example_uris.len().min(3) as u8);
                    for (i, uri) in example_uris.iter().enumerate() {
                        match cache.clone().get_album_cover_lite(uri, thumbnail_for_fetch).await {
                            Ok(Some(tex)) => cover_fan.set_cover(i.min(3) as u8, &tex),
                            _ => cover_fan.clear_cover(i.min(3) as u8, thumbnail_for_fetch)
                        };
                        if i >= 3 {
                            break;
                        }
                    }
                }
            }
        ));
    }

    fn try_upgrade_hires(&self) {
        let imp = self.imp();
        if imp.deferred_hires.get() && self.should_load_texture() {
            let backlog = imp.cache.get().unwrap().backlog();
            if backlog < *BACKLOG_THRESHOLD {
                imp.deferred_hires.set(false);
                self.update_textures();
            }
        }
    }

    fn should_load_texture(&self) -> bool {
        if !self.imp().obj_ready.get() {
            return false;
        }
        // The road to this whole mess lies undocumented in the GTK source code.
        // Nice use of my 2 evenings.
        match self.imp().viewport.get().and_then(|w| w.upgrade()) {
            Some(vp) => {
                if let Some(bounds) = self.compute_bounds(&vp) {
                    let cell_x = bounds.x() as f64;
                    let cell_y = bounds.y() as f64;
                    let cell_w = bounds.width() as f64;
                    let cell_h = bounds.height() as f64;
                    if cell_w == 0.0 && cell_h == 0.0 {
                        // If the bounds are the zero rectangle then we can bail early.
                        return false;
                    }
                    let vis_w = vp.width().max(0) as f64;
                    let vis_h = vp.height().max(0) as f64;
                    // Note: compute_bounds() on a viewport-like widget will return coordinates
                    // in a rather weird way: always by the viewport's location within the window,
                    // with scrolling affecting the positions of the widgets therein. In other words,
                    // within this coordinate system, the rendered area's top left corner is always at
                    // (0, 0) and the ArtistCell's location might be in the negative.
                    ((cell_x <= vis_w + WING_DEPTH && cell_x >= -WING_DEPTH)
                        || (cell_x + cell_w <= vis_w + WING_DEPTH
                            && cell_x + cell_w >= -WING_DEPTH))
                        && ((cell_y <= vis_h + WING_DEPTH && cell_y >= -WING_DEPTH)
                            || (cell_y + cell_h <= vis_h + WING_DEPTH
                                && cell_y + cell_h >= -WING_DEPTH))
                } else {
                    false // we're in a GridView; don't load until given a bound
                }
            }
            None => true,
        }
    }

    pub fn bind(&self, artist: &Artist) {
        let imp = self.imp();
        imp.artist.set(Some(artist));
        imp.deferred.set(false);
        imp.deferred_hires.set(false);
        // Only check this eagerly if backpressure is low
        let backlog = imp.cache.get().unwrap().backlog();
        if backlog < *BACKLOG_THRESHOLD {
            // if self.should_load_texture() {
            //     self.update_cover(true);
            // }
        } else {
            imp.deferred.set(true);
        }
    }

    pub fn unbind(&self) {
        let cover_fan = self.imp().covers.get();
        // For unbound (out-of-view) cells we don't need the high res placeholders
        cover_fan.clear_cover(0, true);
        cover_fan.clear_cover(1, true);
        cover_fan.clear_cover(2, true);
        self.imp().artist.set(None);
        self.imp().avatar.set_custom_image(Option::<&gdk::Texture>::None);
        self.imp().deferred.set(false);
        self.imp().deferred_hires.set(false);
    }

    pub fn hires(&self) -> bool {
        self.imp().hires.get()
    }

    pub fn set_hires(&self, new: bool) {
        let old = self.imp().hires.replace(new);
        if old != new {
            self.notify("hires");
            if new {
                self.imp().deferred_hires.set(false);
                if self.should_load_texture() {
                    self.update_textures();
                }
            } else {
                self.imp().deferred_hires.set(false);
            }
        }
    }
}
