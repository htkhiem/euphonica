use derivative::Derivative;
use gio::{ActionEntry, Menu, SimpleActionGroup};
use glib::{Object, SignalHandlerId, WeakRef, clone, closure_local};
use gtk::{CompositeTemplate, gio, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    EuphonicaWindow,
    cache::{BACKLOG_THRESHOLD, Cache},
    common::{Album, ContentStack, ImageStack, RowAddButtons, Song, SongRow, WING_DEPTH},
    utils::format_secs_as_duration,
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
        pub inner: TemplateChild<gtk::Box>,
        #[template_child]
        pub cover: TemplateChild<ImageStack>,
        #[template_child]
        pub title_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub title_arrow: TemplateChild<gtk::Image>,

        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub replace_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub queue_split_button: TemplateChild<adw::SplitButton>,
        #[template_child]
        pub queue_split_button_content: TemplateChild<adw::ButtonContent>,
        #[template_child]
        pub add_to_playlist: TemplateChild<AddToPlaylistButton>,
        #[template_child]
        pub sel_all: TemplateChild<gtk::Button>,
        #[template_child]
        pub sel_none: TemplateChild<gtk::Button>,

        #[template_child]
        pub content_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListBox>,

        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        pub cache: OnceCell<Rc<Cache>>,
        pub library: WeakRef<Library>,
        pub album: OnceCell<Album>,
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
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
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

            // ListBox native selection: connect to selected-rows-changed signal
            let content = self.content.get();
            content.connect_selected_rows_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().on_selection_changed();
                }
            ));

            self.title_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let (Some(album), Some(win)) = (this.album.get(), this.window.upgrade()) {
                        win.goto_album(album);
                    }
                }
            ));

            // Select-all / clear-selection buttons
            self.sel_all.connect_clicked(clone!(
                #[weak]
                content,
                move |_| {
                    content.select_all();
                }
            ));
            self.sel_none.connect_clicked(clone!(
                #[weak]
                content,
                move |_| {
                    content.unselect_all();
                }
            ));

            // Insert-queue action for the split button menu
            let obj = self.obj();
            let action_insert_queue = ActionEntry::builder("insert-queue")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(async move {
                            obj.queue_selected(false, false, true).await;
                        });
                    }
                ))
                .build();
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_insert_queue]);
            self.obj()
                .insert_action_group("discography-album", Some(&actions));
        }
    }

    impl WidgetImpl for DiscographyAlbum {}

    impl BoxImpl for DiscographyAlbum {}
}

// Common row widget for displaying a single song, used across the UI.
glib::wrapper! {
    pub struct DiscographyAlbum(ObjectSubclass<imp::DiscographyAlbum>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl DiscographyAlbum {
    pub fn new(
        album: Option<Album>, // pass None for untagged songs
        songs: &[Song],
        cache: Rc<Cache>,
        library: &Library,
        window: Option<&EuphonicaWindow>,
        viewport: Option<&gtk::ScrolledWindow>,
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().cache.set(cache);
        res.imp().library.set(Some(library));
        if let Some(album) = album {
            res.imp().title.set_label(album.get_title());
            let _ = res.imp().album.set(album);
        } else {
            // TODO: translations
            res.imp().title.set_label("Untagged");
            res.imp().cover.set_visible(false);
            res.imp().title_btn.set_sensitive(false);
            res.imp().title_arrow.set_visible(false);
        }

        res.imp().song_list.extend_from_slice(songs);
        if !songs.is_empty() {
            res.imp().content_stack.show_content();
        } else {
            res.imp().content_stack.show_placeholder();
        }

        // Set up AddToPlaylistButton with ListBox row selection.
        res.imp()
            .add_to_playlist
            .bind_listbox(library, &res.imp().content, &res.imp().song_list);

        // Replace queue button (play selected / all)
        res.imp().replace_queue.connect_clicked(clone!(
            #[weak(rename_to = this)]
            res,
            move |_| {
                glib::spawn_future_local(async move {
                    this.queue_selected(true, true, false).await;
                });
            }
        ));

        // Queue split button (queue selected / all)
        res.imp().queue_split_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            res,
            #[upgrade_or]
            (),
            move |_| {
                glib::spawn_future_local(async move {
                    this.queue_selected(false, false, false).await;
                });
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
                                if imp.album.get().is_some() {
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

        // Set up ListBox — bind directly to song_list (no MultiSelection needed)
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

        res
    }

    fn library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn album(&self) -> Option<&Album> {
        self.imp().album.get()
    }

    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().queue_split_button.set_sensitive(!queuing);
    }

    fn selected_songs(&self) -> Vec<Song> {
        let content = self.imp().content.get();
        let n_sel = content.selected_rows().len();
        let store = &self.imp().song_list;
        let total = store.n_items() as i32;

        let mut songs: Vec<Song> = Vec::with_capacity(total as usize);
        if n_sel == 0 || n_sel as i32 == total {
            for idx in 0..total {
                songs.push(store.item(idx as u32).and_downcast::<Song>().unwrap());
            }
        } else {
            for row in content.selected_rows() {
                songs.push(
                    store
                        .item(row.index() as u32)
                        .and_downcast::<Song>()
                        .unwrap(),
                );
            }
        }
        songs
    }

    async fn queue_selected(&self, replace: bool, play: bool, next: bool) {
        if let Some(library) = self.library() {
            self.set_is_queuing(true);
            let content = self.imp().content.get();
            let n_sel = content.selected_rows().len();
            let store = &self.imp().song_list;
            let total = store.n_items() as i32;
            if next {
                // Handled specially
                if let Err(e) = library.insert_songs_next(&self.selected_songs()).await {
                    dbg!(e);
                }
            } else if let (true, Some(album)) = (n_sel == 0 || n_sel as i32 == total, self.album())
            {
                // If has album & all tracks are selected, use queue_album.
                if let Err(e) = library
                    .queue_album(album.clone(), replace, play, None)
                    .await
                {
                    dbg!(e);
                }
            } else {
                // Catch-all: manually queue each song.
                if let Err(e) = library
                    .queue_songs(&self.selected_songs(), replace, play)
                    .await
                {
                    dbg!(e);
                }
            }
            self.set_is_queuing(false);
        }
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

    pub fn on_selection_changed(&self) {
        let content = self.imp().content.get();
        let n_sel = content.selected_rows().len();
        let total = self.imp().song_list.n_items() as i32;

        if n_sel == 0 || n_sel as i32 == total {
            self.imp().replace_queue_text.set_label("Play all");
            self.imp().queue_split_button_content.set_label("Queue all");
            let queue_split_menu = Menu::new();
            queue_split_menu.append(
                Some("Queue all next"),
                Some("discography-album.insert-queue"),
            );
            self.imp()
                .queue_split_button
                .set_menu_model(Some(&queue_split_menu));
        } else {
            self.imp()
                .replace_queue_text
                .set_label(format!("Play {n_sel}").as_str());
            self.imp()
                .queue_split_button_content
                .set_label(format!("Queue {n_sel}").as_str());
            let queue_split_menu = Menu::new();
            queue_split_menu.append(
                Some(format!("Queue {n_sel} next").as_str()),
                Some("discography-album.insert-queue"),
            );
            self.imp()
                .queue_split_button
                .set_menu_model(Some(&queue_split_menu));
        }
    }

    pub fn set_narrow(&self, narrow: bool) {
        // Kinda like Adwaita breakpoints but implemented ourselves to reduce nesting.
        if narrow {
            self.imp().inner.set_orientation(gtk::Orientation::Vertical);
        } else {
            self.imp()
                .inner
                .set_orientation(gtk::Orientation::Horizontal);
        }
    }
}
