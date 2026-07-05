use derivative::Derivative;
use gtk::{
    CompositeTemplate, Image, Label, gdk,
    glib::{
        self, Object, ParamSpec, ParamSpecBoolean, ParamSpecChar, ParamSpecInt, ParamSpecString,
        WeakRef, clone, closure_local, signal::SignalHandlerId,
    },
    prelude::*,
    subclass::prelude::*,
};
use once_cell::sync::Lazy;
use std::{
    cell::{Cell, OnceCell, RefCell},
    f32::consts::PI,
    rc::Rc,
};

use crate::{
    cache::{
        BACKLOG_THRESHOLD, Cache, CacheState,
        placeholders::{EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING},
    },
    common::{
        Album, Artist, PictureStack, Rating,
        marquee::{Marquee, MarqueeWrapMode},
    },
    utils::settings_manager,
    window::EuphonicaWindow,
};

// As soon as a cell comes within this close of the render area, treat it as
// visible & load album art early to avoid showing loading spinners.
static WING_DEPTH: f64 = 512.0;
static MIN_ART_SIZE: i32 = 100;
static FAN_ANGLE: f32 = 11.25;

// Design:
// If no example album is given, simply draw a centered avatar.
// If one album is given: draw that album's cover behind the avatar, now reduced to half-size and aligned to the bottom middle.
// If two: rotate the covers 12.5deg to either sides; the album on the right is drawn on top (like a fan of playing cards).
// If three or more: same as above, with an unrotated middle album cover. Only show up to three cover arts.

mod imp {
    use gtk::graphene;

    use crate::{cache::placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, common::Artist};

    use super::*;

    #[derive(CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-cell.ui")]
    pub struct ArtistCell {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub cover1: TemplateChild<PictureStack>, // left
        #[template_child]
        pub cover2: TemplateChild<PictureStack>, // mid
        #[template_child]
        pub cover3: TemplateChild<PictureStack>, // right
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
        type ParentType = gtk::Widget;

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

        fn constructed(&self) {
            self.parent_constructed();

            // self.obj()
            //     .bind_property("rating", &self.rating.get(), "value")
            //     .sync_create()
            //     .build();

            // self.obj()
            //     .bind_property("rating", &self.rating.get(), "visible")
            //     .transform_to(|_, r: i8| Some(r >= 0))
            //     .sync_create()
            //     .build();

            // self.obj()
            //     .bind_property("image-size", &self.cover.get(), "size")
            //     .sync_create()
            //     .build();

            self.cover1.set_is_thumbnail(true);
            self.cover2.set_is_thumbnail(true);
            self.cover3.set_is_thumbnail(true);

            self.cover1.clear();
            self.cover2.clear();
            self.cover3.clear();
        }

        // fn properties() -> &'static [ParamSpec] {
        //     static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
        //         vec![
        //             ParamSpecString::builder("name").build(),
        //             ParamSpecObject::builder::<glib::BoxedAnyObject>("example-uris").build()
        //         ]
        //     });
        //     PROPERTIES.as_ref()
        // }

        // fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
        //     match pspec.name() {
        //         "title" => self.title.label().to_value(),
        //         "artist" => self.artist.label().to_value(),
        //         "quality-grade" => self.quality_grade.icon_name().to_value(),
        //         "rating" => self.rating_val.get().to_value(),
        //         "image-size" => self.image_size.get().to_value(),
        //         "hires" => self.hires.get().to_value(),
        //         _ => unimplemented!(),
        //     }
        // }

        // fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
        //     let obj = self.obj();
        //     match pspec.name() {
        //         "title" => {
        //             if let Ok(title) = value.get::<&str>() {
        //                 self.title.label().set_label(title);
        //                 obj.notify("title");
        //             }
        //         }
        //         "artist" => {
        //             if let Ok(artist) = value.get::<&str>() {
        //                 self.artist.set_label(artist);
        //                 obj.notify("artist");
        //             }
        //         }
        //         "quality-grade" => {
        //             if let Ok(icon_name) = value.get::<&str>() {
        //                 self.quality_grade.set_icon_name(Some(icon_name));
        //                 self.quality_grade.set_visible(true);
        //             } else {
        //                 self.quality_grade.set_icon_name(None);
        //                 self.quality_grade.set_visible(false);
        //             }
        //         }
        //         "rating" => {
        //             if let Ok(new) = value.get::<i8>() {
        //                 let old = self.rating_val.replace(new);
        //                 if old != new {
        //                     obj.notify("rating");
        //                 }
        //             }
        //         }
        //         "image-size" => {
        //             if let Ok(new) = value.get::<i32>() {
        //                 obj.set_image_size(new);
        //             }
        //         }
        //         "hires" => {
        //             if let Ok(new) = value.get::<bool>() {
        //                 obj.set_hires(new);
        //             }
        //         }
        //         _ => unimplemented!(),
        //     }
        // }
    }

    impl WidgetImpl for ArtistCell {
        fn request_mode(&self) -> gtk::SizeRequestMode {
            gtk::SizeRequestMode::HeightForWidth
        }

        fn measure(&self, orientation: gtk::Orientation, for_size: i32) -> (i32, i32, i32, i32) {
            // The widget contains the fancy drawings (always square overall), plus a label at the bottom.
            if orientation == gtk::Orientation::Horizontal {
                (MIN_ART_SIZE, MIN_ART_SIZE, -1, -1)
            } else {
                // Ensure we request enough vertical space for a square art area at the
                // given width.
                // Calculating the actual total height is rather involved due to gaps and
                // the like. Instead of re-implementing the sum, we simply calculate the
                // "as usual" height of the art area when allocated using GTK4 rules to
                // its width and adjust the total accordingly.
                // Return order reminder: min, natural, min baseline, natural baseline.
                let label_height = self
                    .name
                    .get()
                    .measure(gtk::Orientation::Vertical, for_size);
                (
                    for_size + label_height.0,
                    for_size + label_height.1,
                    label_height.2,
                    label_height.3,
                )
            }
        }

        fn size_allocate(&self, w: i32, h: i32, baseline: i32) {
            // Depending on how many example arts we're given
            let edge = w.min(h);
            if let Some(artist) = self.artist.upgrade() {
                // Actual draw pos will depend on transformation of the snapshot coordinate system
                match artist.get_info().example_uris.len() {
                    0 | 1 => {
                        // Sole art => 85% short edge
                        let art_edge = (edge as f32 * 0.85).floor() as i32;
                        self.cover1.get().size_allocate(
                            &gtk::Allocation::new(0, 0, art_edge, art_edge),
                            baseline,
                        );
                    }
                    2 => {
                        let art_edge = (edge as f32 * 0.72).floor() as i32;
                        self.cover1.get().size_allocate(
                            &gtk::Allocation::new(0, 0, art_edge, art_edge),
                            baseline,
                        );
                        self.cover3.get().size_allocate(
                            &gtk::Allocation::new(0, 0, art_edge, art_edge),
                            baseline,
                        );
                    }
                    _ => {
                        let art_edge = (edge as f32 * 0.72).floor() as i32;
                        self.cover1.get().size_allocate(
                            &gtk::Allocation::new(0, 0, art_edge, art_edge),
                            baseline,
                        );
                        self.cover2.get().size_allocate(
                            &gtk::Allocation::new(0, 0, art_edge, art_edge),
                            baseline,
                        );
                        self.cover3.get().size_allocate(
                            &gtk::Allocation::new(0, 0, art_edge, art_edge),
                            baseline,
                        );
                    }
                };
            }
            self.avatar.get().size_allocate(
                &gtk::Allocation::new(
                    (0.25 * edge as f32).floor() as i32,
                    edge / 2,
                    edge / 2,
                    edge / 2,
                ),
                baseline,
            );
            // TODO: allocate name label
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            if let Some(artist) = self.artist.upgrade() {
                let obj = self.obj();
                let w = obj.width() as f32;
                let h = obj.height() as f32;
                let edge = w.min(h);
                // Actual draw pos will depend on transformation of the snapshot coordinate system
                // Left art's top-left corner's y-offset versus origin
                let rads = FAN_ANGLE / 180.0 * PI;
                let a = 0.72 * edge * rads.sin();
                // Right art's top-left corner's x-offset versus origin
                let b = edge * (1.0 - 0.72 * rads.cos());
                // Middle art's top-left corner's x-offset versus origin (when all 3 are shown)
                let c = edge * (1.0 - 0.72) / 2.0;
                // eprintln!("a={}, b={}, c={}", a, b, c);
                let n_ex = artist.get_info().example_uris.len();
                match n_ex {
                    0 | 1 => {
                        // Sole art
                        snapshot.translate(&graphene::Point::new(-edge * (1.0 - 0.85) / 2.0, 0.0));
                        obj.snapshot_child(&self.cover1.get(), snapshot);
                        // Back to old 0.0
                        snapshot.translate(&graphene::Point::new(edge * (1.0 - 0.85) / 2.0, 0.0));
                    }
                    2 => {
                        // Cover 1 to the left and behind
                        snapshot.translate(&graphene::Point::new(0.0, a));
                        snapshot.rotate(-FAN_ANGLE);
                        obj.snapshot_child(&self.cover1.get(), snapshot);
                        snapshot.rotate(FAN_ANGLE);

                        // Cover 3 to the right and in front
                        snapshot.translate(&graphene::Point::new(b, -a));
                        snapshot.rotate(FAN_ANGLE);
                        obj.snapshot_child(&self.cover3.get(), snapshot);
                        snapshot.rotate(-FAN_ANGLE);
                        snapshot.translate(&graphene::Point::new(-b, 0.0));
                    }
                    _ => {
                        // Cover 1 to the left and behind
                        snapshot.translate(&graphene::Point::new(0.0, a));
                        snapshot.rotate(-FAN_ANGLE);
                        obj.snapshot_child(&self.cover1.get(), snapshot);
                        snapshot.rotate(FAN_ANGLE);

                        // Cover 2 in the middle and unrotated
                        snapshot.translate(&graphene::Point::new(c, -a));
                        obj.snapshot_child(&self.cover2.get(), snapshot);

                        // Cover 3 to the right and in front
                        snapshot.translate(&graphene::Point::new(b - c, 0.0));
                        snapshot.rotate(FAN_ANGLE);
                        obj.snapshot_child(&self.cover3.get(), snapshot);
                        snapshot.rotate(-FAN_ANGLE);
                        snapshot.translate(&graphene::Point::new(-b, 0.0));
                    }
                };
            }
            // obj.snapshot_child(&self.inner.get(), snapshot);
        }
    }
}

glib::wrapper! {
    pub struct ArtistCell(ObjectSubclass<imp::ArtistCell>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl ArtistCell {
    pub fn new(
        item: &gtk::ListItem,
        cache: Rc<Cache>,
        window: Option<crate::window::EuphonicaWindow>,
        viewport: Option<gtk::GridView>,
    ) -> Self {
        let res: Self = Object::builder().build();
        let cache_state = cache.get_cache_state();
        res.imp()
            .cache
            .set(cache)
            .expect("ArtistCell cannot bind to cache");
        // item.property_expression("item")
        //     .chain_property::<Artist>("name")
        //     .chain_closure::<String>(closure_local!(
        //         |_: Option<glib::Object>, title: Option<&str>| {
        //             String::from(if title.is_none_or(|t| t.is_empty()) {
        //                 *EMPTY_ARTIST_STRING
        //             } else {
        //                 title.unwrap()
        //             })
        //         }
        //     ))
        //     .bind(&res, "name", gtk::Widget::NONE);
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
        // if let Some(window) = window {
        //     let handler = window.connect_closure(
        //         "check-visible",
        //         false,
        //         closure_local!(
        //             #[weak(rename_to = this)]
        //             res,
        //             move |_: &EuphonicaWindow| {
        //                 let imp = this.imp();
        //                 let is_visible = this.should_load_texture();
        //                 let was_visible = imp.should_load_texture.replace(is_visible);

        //                 if is_visible {
        //                     // Also go through this if visibility status didn't change,
        //                     // but album art load hasn't been attempted yet.
        //                     if was_visible != is_visible || imp.deferred.get() {
        //                         imp.deferred.set(false);
        //                         if imp.album.upgrade().is_some() {
        //                             glib::idle_add_local_once(clone!(
        //                                 #[weak]
        //                                 this,
        //                                 move || {
        //                                     this.update_cover(true);
        //                                 }
        //                             ));
        //                         }
        //                     } else if imp.deferred_hires.get() {
        //                         this.try_upgrade_hires();
        //                     }
        //                 } else if was_visible != is_visible {
        //                     imp.cover.clear();
        //                 }
        //             }
        //         ),
        //     );
        //     res.imp().check_visible_handler.replace(Some(handler));
        // }

        res.imp().obj_ready.set(true);

        res
    }

    fn artist(&self) -> Option<Artist> {
        self.imp().artist.upgrade()
    }

    // fn update_cover(&self, show_spinner: bool) {
    //     let imp = self.imp();

    //     // If hires is requested, check backlog to decide resolution.
    //     if imp.hires.get() {
    //         let backlog = imp.cache.get().unwrap().backlog();
    //         if backlog >= *BACKLOG_THRESHOLD {
    //             // Too many pending tasks; defer hires and load thumbnail instead.
    //             imp.deferred_hires.set(true);
    //         }
    //     } else {
    //         imp.deferred_hires.set(false);
    //     }

    //     let thumbnail_for_fetch = !imp.hires.get() || imp.deferred_hires.get();
    //     if show_spinner {
    //         imp.cover.show_spinner();
    //     }
    //     glib::spawn_future_local(clone!(
    //         #[weak(rename_to = this)]
    //         self,
    //         async move {
    //             if let Some(album) = this.album() {
    //                 let res = this
    //                     .imp()
    //                     .cache
    //                     .get()
    //                     .unwrap()
    //                     .clone()
    //                     .get_album_cover(album.get_info(), thumbnail_for_fetch)
    //                     .await;
    //                 // Check again as cell might have been bound to a different album
    //                 // while awaiting
    //                 if this.album().is_some_and(|a| {
    //                     a.get_info().get_comp_id() == album.get_info().get_comp_id()
    //                 }) {
    //                     match res {
    //                         Ok(Some(tex)) => {
    //                             this.imp().cover.show(&tex);
    //                         }
    //                         Ok(None) => {
    //                             this.imp().cover.clear();
    //                         }
    //                         Err(e) => {
    //                             this.imp().cover.clear();
    //                             eprintln!("Failed to read cover for album `{}` (URI `{}`):\n{:?}", album.get_title(), album.get_folder_uri(), e);
    //                         }
    //                     }
    //                 }
    //             }
    //         }
    //     ));
    // }

    // fn try_upgrade_hires(&self) {
    //     let imp = self.imp();
    //     if imp.deferred_hires.get() && self.should_load_texture() {
    //         let backlog = imp.cache.get().unwrap().backlog();
    //         if backlog < *BACKLOG_THRESHOLD {
    //             imp.deferred_hires.set(false);
    //             self.update_cover(false);
    //         }
    //     }
    // }

    // fn should_load_texture(&self) -> bool {
    //     if !self.imp().obj_ready.get() {
    //         return false;
    //     }
    //     // The road to this whole mess lies undocumented in the GTK source code.
    //     // Nice use of my 2 evenings.
    //     match self.imp().viewport.get().and_then(|w| w.upgrade()) {
    //         Some(vp) => {
    //             if let Some(bounds) = self.compute_bounds(&vp) {
    //                 let cell_x = bounds.x() as f64;
    //                 let cell_y = bounds.y() as f64;
    //                 let cell_w = bounds.width() as f64;
    //                 let cell_h = bounds.height() as f64;
    //                 if cell_w == 0.0 && cell_h == 0.0 {
    //                     // If the bounds are the zero rectangle then we can bail early.
    //                     return false;
    //                 }
    //                 let vis_w = vp.width().max(0) as f64;
    //                 let vis_h = vp.height().max(0) as f64;
    //                 // Note: compute_bounds() on a viewport-like widget will return coordinates
    //                 // in a rather weird way: always by the viewport's location within the window,
    //                 // with scrolling affecting the positions of the widgets therein. In other words,
    //                 // within this coordinate system, the rendered area's top left corner is always at
    //                 // (0, 0) and the ArtistCell's location might be in the negative.
    //                 ((cell_x <= vis_w + WING_DEPTH && cell_x >= -WING_DEPTH)
    //                     || (cell_x + cell_w <= vis_w + WING_DEPTH
    //                         && cell_x + cell_w >= -WING_DEPTH))
    //                     && ((cell_y <= vis_h + WING_DEPTH && cell_y >= -WING_DEPTH)
    //                         || (cell_y + cell_h <= vis_h + WING_DEPTH
    //                             && cell_y + cell_h >= -WING_DEPTH))
    //             } else {
    //                 false // we're in a GridView; don't load until given a bound
    //             }
    //         }
    //         None => true,
    //     }
    // }

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
        self.imp().cover1.clear();
        self.imp().cover2.clear();
        self.imp().cover3.clear();
        self.imp().artist.set(None);
        self.imp().deferred.set(false);
        self.imp().deferred_hires.set(false);
    }

    // pub fn image_size(&self) -> i32 {
    //     self.imp().image_size.get()
    // }

    // pub fn set_image_size(&self, new: i32) {
    //     let old = self.imp().image_size.replace(new);
    //     if old != new {
    //         self.notify("image-size");
    //     }
    // }

    pub fn hires(&self) -> bool {
        self.imp().hires.get()
    }

    pub fn set_hires(&self, new: bool) {
        let old = self.imp().hires.replace(new);
        if old != new {
            self.imp().cover1.set_is_thumbnail(!new);
            self.imp().cover2.set_is_thumbnail(!new);
            self.imp().cover3.set_is_thumbnail(!new);
            self.notify("hires");
            if new {
                self.imp().deferred_hires.set(false);
                // if self.should_load_texture() {
                //     self.update_cover(true);
                // }
            } else {
                self.imp().deferred_hires.set(false);
            }
        }
    }
}
