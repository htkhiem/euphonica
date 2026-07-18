use derivative::Derivative;
use gtk::{CompositeTemplate, glib, gio, prelude::*, subclass::prelude::*};
use glib::{
    Object, SignalHandlerId,
    WeakRef, clone, closure_local,
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    EuphonicaWindow, cache::{BACKLOG_THRESHOLD, Cache}, common::{Album, ContentStack, ImageStack, RowAddButtons, Song, SongRow, WING_DEPTH}, utils::format_secs_as_duration,
};

use super::{Library, add_to_playlist::AddToPlaylistButton};

// Wrapper around the common row object to implement song thumbnail fetch logic.
mod imp {
    use super::*;

    #[derive(Derivative, CompositeTemplate)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/discography-album.ui")]
    pub struct DiscographyAlbum {
        #[template_child]
        pub cover: TemplateChild<ImageStack>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,

        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub add_to_playlist: TemplateChild<AddToPlaylistButton>,

        #[template_child]
        pub content_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListBox>,

        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        pub cache: OnceCell<Rc<Cache>>,
        pub library: WeakRef<Library>,
        pub album: WeakRef<Album>,
        // Stored ref to the ScrolledWindow in artist_content_view for visibility checks.
        pub viewport: WeakRef<gtk::ScrolledWindow>,
        // Weak reference to the window for connecting to the check-visible signal.
        pub window: WeakRef<EuphonicaWindow>,
        // Signal handler ID for disconnecting from the window's check-visible signal.
        pub check_visible_handler: RefCell<Option<SignalHandlerId>>,
        // Tracks whether this album content box is currently within the viewport, used
        // to detect visibility transitions (entering/exiting the viewport).
        pub should_load_texture: Cell<bool>,
        // Set to true when the bind-time visibility check was skipped due to
        // backpressure.
        pub deferred: Cell<bool>,
        // Set to true when the construction-time visibility check was skipped due to
        // backpressure.
        pub deferred_hires: Cell<bool>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for DiscographyAlbum {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaDiscographyAlbum";
        type Type = super::DiscographyAlbum;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for DiscographyAlbum {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
        }
        // fn properties() -> &'static [ParamSpec] {
        //     static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
        //         vec![
        //             ParamSpecBoolean::builder("playing-indicator-visible").build(),
        //             ParamSpecBoolean::builder("is-playing").build(),
        //             ParamSpecBoolean::builder("index-visible").build(),
        //             ParamSpecString::builder("index").build(),
        //             ParamSpecBoolean::builder("thumbnail-visible").build(),
        //             ParamSpecString::builder("name").build(),
        //             ParamSpecString::builder("quality-grade").build(),
        //             ParamSpecString::builder("first-attrib-icon-name").build(),
        //             ParamSpecString::builder("second-attrib-icon-name").build(),
        //             ParamSpecString::builder("third-attrib-icon-name").build(),
        //             ParamSpecString::builder("first-attrib-text").build(),
        //             ParamSpecString::builder("second-attrib-text").build(),
        //             ParamSpecString::builder("third-attrib-text").build(),
        //             ParamSpecObject::builder::<gtk::Widget>("end-widget").build(),
        //         ]
        //     });
        //     PROPERTIES.as_ref()
        // }

        // fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
        //     match pspec.name() {
        //         "playing-indicator-visible" => self.playing_indicator.is_visible().to_value(),
        //         "is-playing" => self.playing_indicator.is_child_revealed().to_value(),
        //         "index-visible" => self.thumbnail.is_visible().to_value(),
        //         "index" => self.index.label().to_value(),
        //         "thumbnail-visible" => self.thumbnail.is_visible().to_value(),
        //         "name" => self.name.label().label().to_value(),
        //         "quality-grade" => self.quality_grade.icon_name().to_value(),
        //         "first-attrib-icon-name" => self.first_attrib_icon.icon_name().to_value(),
        //         "second-attrib-icon-name" => self.second_attrib_icon.icon_name().to_value(),
        //         "third-attrib-icon-name" => self.third_attrib_icon.icon_name().to_value(),
        //         "first-attrib-text" => self.first_attrib_text.label().to_value(),
        //         "second-attrib-text" => self.second_attrib_text.label().to_value(),
        //         "third-attrib-text" => self.third_attrib_text.label().to_value(),
        //         "end-widget" => self.center_box.end_widget().to_value(),
        //         _ => unimplemented!(),
        //     }
        // }

        // fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
        //     let obj = self.obj();
        //     match pspec.name() {
        //         "playing-indicator-visible" => {
        //             if let Ok(vis) = value.get::<bool>() {
        //                 self.playing_indicator.set_visible(vis);
        //             }
        //         }
        //         "is-playing" => {
        //             if let Ok(vis) = value.get::<bool>() {
        //                 self.playing_indicator.set_reveal_child(vis);
        //             }
        //         }
        //         "index-visible" => {
        //             if let Ok(vis) = value.get::<bool>() {
        //                 self.index.set_visible(vis);
        //             }
        //         }
        //         "index" => {
        //             if let Ok(idx) = value.get::<&str>() {
        //                 self.index.set_label(idx);
        //             }
        //         }
        //         "thumbnail-visible" => {
        //             if let Ok(vis) = value.get::<bool>() {
        //                 self.thumbnail.set_visible(vis);
        //             }
        //         }
        //         "name" => {
        //             if let Ok(name) = value.get::<&str>() {
        //                 self.name.label().set_label(name);
        //             }
        //         }
        //         "quality-grade" => {
        //             let maybe_icon = value.get::<&str>();
        //             self.quality_grade.set_visible(maybe_icon.is_ok());
        //             self.quality_grade.set_icon_name(maybe_icon.ok());
        //         }
        //         "first-attrib-icon-name" => {
        //             obj.set_first_attrib_icon_name(value.get::<&str>().ok());
        //         }
        //         "second-attrib-icon-name" => {
        //             obj.set_second_attrib_icon_name(value.get::<&str>().ok());
        //         }
        //         "third-attrib-icon-name" => {
        //             obj.set_third_attrib_icon_name(value.get::<&str>().ok());
        //         }
        //         "first-attrib-text" => {
        //             obj.set_first_attrib_text(value.get::<&str>().ok());
        //         }
        //         "second-attrib-text" => {
        //             obj.set_second_attrib_text(value.get::<&str>().ok());
        //         }
        //         "third-attrib-text" => {
        //             obj.set_third_attrib_text(value.get::<&str>().ok());
        //         }
        //         "end-widget" => {
        //             obj.set_end_widget(value.get::<gtk::Widget>().ok().as_ref());
        //         }
        //         _ => unimplemented!(),
        //     }
        // }
    }

    impl WidgetImpl for DiscographyAlbum {}
}

// Common row widget for displaying a single song, used across the UI.
glib::wrapper! {
    pub struct DiscographyAlbum(ObjectSubclass<imp::DiscographyAlbum>)
    @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl DiscographyAlbum {
    pub fn new(
        album: &Album,
        cache: Rc<Cache>,
        library: &Library,
        window: Option<&EuphonicaWindow>,
        viewport: Option<&gtk::ScrolledWindow>,
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().cache.set(cache);
        res.imp().library.set(Some(library));
        res.imp().album.set(Some(album));
        res.imp().title.set_label(album.get_title());

        res.imp().replace_queue.connect_clicked(clone!(
            #[weak]
            res,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    res,
                    async move {
                        if let (Some(album), Some(library)) = (res.album(), res.library()) {
                            res.set_is_queuing(true);
                            if let Err(e) =
                                library.queue_album(album.clone(), true, true, None).await
                            {
                                dbg!(e);
                            }
                            res.set_is_queuing(false);
                        }
                    }
                ));
            }
        ));

        res.imp().append_queue.connect_clicked(clone!(
            #[weak]
            res,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    res,
                    async move {
                        if let (Some(album), Some(library)) = (res.album(), res.library()) {
                            res.set_is_queuing(true);
                            if let Err(e) =
                                library.queue_album(album.clone(), false, false, None).await
                            {
                                dbg!(e);
                            }
                            res.set_is_queuing(false);
                        }
                    }
                ));
            }
        ));

        // Set up dynamic texture loading if a GridView is available.
        res.imp().viewport.set(viewport);

        // Store weak reference to the window for signal connection.
        res.imp().window.set(window);

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
                                if imp.album.upgrade().is_some() {
                                    glib::idle_add_local_once(clone!(
                                        #[weak]
                                        this,
                                        move || {
                                            this.update_cover(true);
                                        }
                                    ));
                                }
                            } else if imp.deferred_hires.get() {
                                this.try_upgrade_hires();
                            }
                        } else if was_visible != is_visible {
                            imp.cover.clear();
                        }
                    }
                ),
            );
            res.imp().check_visible_handler.replace(Some(handler));
        }

        // Set up ListBox
        res.imp().content.bind_model(
            Some(&res.imp().song_list),
            clone!(
                #[weak]
                library,
                #[upgrade_or]
                SongRow::new(None, None).into(),
                move |song_obj| {
                    let song = song_obj
                        .downcast_ref::<Song>()
                        .expect("Must be a common::Song");
                    let row = SongRow::new(None, None);
                    row.set_index_visible(true);
                    row.set_index(&song.get_track().to_string());
                    row.set_thumbnail_visible(false);

                    row.set_name(song.get_name());
                    row.set_first_attrib_icon_name(Some("music-artist-symbolic"));
                    row.set_first_attrib_text(song.get_artist_tag());

                    row.set_second_attrib_icon_name(Some("hourglass-symbolic"));
                    row.set_second_attrib_text(Some(&format_secs_as_duration(
                        song.get_duration() as f64
                    )));

                    row.set_quality_grade(song.get_quality_grade());
                    let end_widget = RowAddButtons::new(&library);
                    end_widget.set_song(Some(song));
                    row.set_end_widget(Some(&end_widget.into()));
                    row.into()
                }
            ),
        );

        // Now trigger album content loading
        glib::spawn_future_local(clone!(
            #[weak]
            res,
            #[strong]
            album,
            async move {
                let library = res.imp().library.upgrade().unwrap();
                // Important, MPD-side content first
                let stack = res.imp().content_stack.get();
                stack.show_spinner();
                let song_list = res.imp().song_list.clone();
                song_list.remove_all();
                match library
                    .get_album_songs(&album, &mut |songs| {
                        song_list.extend_from_slice(&songs);
                    })
                    .await
                {
                    Ok(()) => {
                        if song_list.n_items() > 0 {
                            stack.show_content();
                        } else {
                            stack.show_placeholder();
                        }
                    }
                    Err(e) => {
                        dbg!(e);
                    }
                };
            }
        ));

        res
    }

    fn library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn album(&self) -> Option<Album> {
        self.imp().album.upgrade()
    }

    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().append_queue.set_sensitive(!queuing);
    }

    fn update_cover(&self, show_spinner: bool) {
        let imp = self.imp();

        // If hires is requested, check backlog to decide resolution.
        let backlog = imp.cache.get().unwrap().backlog();
        if backlog >= *BACKLOG_THRESHOLD {
            // Too many pending tasks; defer hires and load thumbnail instead.
            imp.deferred_hires.set(true);
        }

        let fetch_thumbnail_first = imp.deferred_hires.get();
        if show_spinner {
            imp.cover.show_spinner();
        }
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Some(album) = this.album() {
                    let res = this
                        .imp()
                        .cache
                        .get()
                        .unwrap()
                        .clone()
                        .get_album_cover(album.get_info(), fetch_thumbnail_first)
                        .await;
                    // Check again as cell might have been bound to a different album
                    // while awaiting
                    if this.album().is_some_and(|a| {
                        a.get_info().get_comp_id() == album.get_info().get_comp_id()
                    }) {
                        match res {
                            Ok(Some(tex)) => {
                                this.imp().cover.show(&tex);
                            }
                            Ok(None) => {
                                this.imp().cover.clear();
                            }
                            Err(e) => {
                                this.imp().cover.clear();
                                eprintln!(
                                    "Failed to read cover for album `{}` (URI `{}`):\n{:?}",
                                    album.get_title(),
                                    album.get_folder_uri(),
                                    e
                                );
                            }
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
                self.update_cover(false);
            }
        }
    }

    fn should_load_texture(&self) -> bool {
        // The road to this whole mess lies undocumented in the GTK source code.
        // Nice use of my 2 evenings.
        match self.imp().viewport.upgrade() {
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
                    // (0, 0) and the AlbumCell's location might be in the negative.
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

    /// Actually populate with cover and content.
    /// Called "load" instead of "bind" as this widget is meant to be used in a ListBox, which doesn't have
    /// the bind-unbind concept as in ListViews (album info already bound at construction time).
    pub fn load(&self) {
        let imp = self.imp();
        imp.deferred.set(false);
        imp.deferred_hires.set(false);
        // Only check this eagerly if backpressure is low
        let backlog = imp.cache.get().unwrap().backlog();
        if backlog < *BACKLOG_THRESHOLD {
            if self.should_load_texture() {
                self.update_cover(true);
            }
        } else {
            imp.deferred.set(true);
        }
    }

    /// Unloads the album art to reduce memory pressure.
    /// Album content stays loaded though as unloading them screws up the content height and thus the scroll pos;
    /// they also don't cost much in perf or memory, unlike the album art.
    pub fn unload_cover(&self) {
        self.imp().cover.clear();
        self.imp().deferred.set(false);
        self.imp().deferred_hires.set(false);
    }
}
