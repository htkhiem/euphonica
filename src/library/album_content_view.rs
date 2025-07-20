use adw::subclass::prelude::*;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{clone, closure_local, signal::SignalHandlerId, Binding};
use gtk::{gio, glib, prelude::*, BitsetIter, CompositeTemplate, ListItem, SignalListItemFactory};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};
use time::{format_description, Date};

use super::{AlbumSongRow, Library};
use crate::{
    cache::{placeholders::ALBUMART_PLACEHOLDER, Cache, CacheState},
    client::ClientState,
    common::{CoverSource, Album, AlbumInfo, Rating, Song},
    utils::format_secs_as_duration,
};

mod imp {
    use std::cell::Cell;

    use ashpd::desktop::file_chooser::SelectedFiles;
    use async_channel::Sender;

    use crate::{common::Rating, library::add_to_playlist::AddToPlaylistButton, utils};

    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-content-view.ui")]
    pub struct AlbumContentView {
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub collapse_infobox: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub cover: TemplateChild<gtk::Image>,
        #[template_child]
        pub content_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListView>,

        #[template_child]
        pub infobox_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist: TemplateChild<gtk::Label>,
        #[template_child]
        pub rating: TemplateChild<Rating>,
        #[template_child]
        pub rating_readout: TemplateChild<gtk::Label>,

        #[template_child]
        pub wiki_box: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub wiki_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub wiki_link: TemplateChild<gtk::LinkButton>,
        #[template_child]
        pub wiki_attrib: TemplateChild<gtk::Label>,

        #[template_child]
        pub release_date: TemplateChild<gtk::Label>,
        #[template_child]
        pub track_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub runtime: TemplateChild<gtk::Label>,

        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub replace_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub add_to_playlist: TemplateChild<AddToPlaylistButton>,
        #[template_child]
        pub sel_all: TemplateChild<gtk::Button>,
        #[template_child]
        pub sel_none: TemplateChild<gtk::Button>,

        pub song_list: gio::ListStore,
        pub sel_model: gtk::MultiSelection,

        pub library: OnceCell<Library>,
        pub album: RefCell<Option<Album>>,
        pub bindings: RefCell<Vec<Binding>>,
        pub cover_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub selecting_all: Cell<bool>, // Enables queuing the entire album efficiently
        pub filepath_sender: OnceCell<Sender<String>>,
        pub cover_source: Cell<CoverSource>
    }

    impl Default for AlbumContentView {
        fn default() -> Self {
            Self {
                cover: TemplateChild::default(),
                title: TemplateChild::default(),
                artist: TemplateChild::default(),
                rating: TemplateChild::default(),
                rating_readout: TemplateChild::default(),
                release_date: TemplateChild::default(),
                track_count: TemplateChild::default(),
                infobox_spinner: TemplateChild::default(),
                infobox_revealer: TemplateChild::default(),
                collapse_infobox: TemplateChild::default(),
                wiki_box: TemplateChild::default(),
                wiki_text: TemplateChild::default(),
                wiki_link: TemplateChild::default(),
                wiki_attrib: TemplateChild::default(),
                runtime: TemplateChild::default(),
                content_spinner: TemplateChild::default(),
                content: TemplateChild::default(),
                song_list: gio::ListStore::new::<Song>(),
                sel_model: gtk::MultiSelection::new(Option::<gio::ListStore>::None),
                replace_queue: TemplateChild::default(),
                append_queue: TemplateChild::default(),
                replace_queue_text: TemplateChild::default(),
                append_queue_text: TemplateChild::default(),
                add_to_playlist: TemplateChild::default(),
                sel_all: TemplateChild::default(),
                sel_none: TemplateChild::default(),
                library: OnceCell::new(),
                album: RefCell::new(None),
                bindings: RefCell::new(Vec::new()),
                cover_signal_id: RefCell::new(None),
                cache: OnceCell::new(),
                selecting_all: Cell::new(true), // When nothing is selected, default to select-all
                filepath_sender: OnceCell::new(),
                cover_source: Cell::default()
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumContentView {
        const NAME: &'static str = "EuphonicaAlbumContentView";
        type Type = super::AlbumContentView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            // klass.set_css_name("albumview");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for AlbumContentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.sel_model.set_model(Some(&self.song_list.clone()));
            self.content.set_model(Some(&self.sel_model));

            // Change button labels depending on selection state
            self.sel_model.connect_selection_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |sel_model, _, _| {
                    // TODO: this can be slow, might consider redesigning
                    let n_sel = sel_model.selection().size();
                    if n_sel == 0 || (n_sel as u32) == sel_model.model().unwrap().n_items() {
                        this.selecting_all.replace(true);
                        this.replace_queue_text.set_label("Play all");
                        this.append_queue_text.set_label("Queue all");
                    } else {
                        // TODO: l10n
                        this.selecting_all.replace(false);
                        this.replace_queue_text
                            .set_label(format!("Play {}", n_sel).as_str());
                        this.append_queue_text
                            .set_label(format!("Queue {}", n_sel).as_str());
                    }
                }
            ));

            let sel_model = self.sel_model.clone();
            self.sel_all.connect_clicked(clone!(
                #[weak]
                sel_model,
                move |_| {
                    sel_model.select_all();
                }
            ));
            self.sel_none.connect_clicked(clone!(
                #[weak]
                sel_model,
                move |_| {
                    sel_model.unselect_all();
                }
            ));

            // Rating readout
            self.rating
                .bind_property(
                    "value", &self.rating_readout.get(), "label"
                )
                .transform_to(|_, r: i8| {
                    // TODO: l10n
                    if r < 0 { Some("Unrated".to_value()) }
                    else { Some(format!("{:.1}", r as f32 / 2.0).to_value()) }
                })
                .sync_create()
                .build();

            // Edit actions
            let obj = self.obj();
            let action_clear_rating = ActionEntry::builder("clear-rating")
                .activate(clone!(
                    #[strong]
                    obj,
                    move |_, _, _| {
                        if let (Some(album), Some(library)) = (
                            obj.imp().album.borrow().as_ref(),
                            obj.imp().library.get()
                        ) {
                            library.rate_album(album, None);
                            obj.imp().rating.set_value(-1);
                        }
                    }
                ))
                .build();
            let action_set_album_art = ActionEntry::builder("set-album-art")
                .activate(clone!(
                    #[strong]
                    obj,
                    move |_, _, _| {
                        if let Some(sender) = obj.imp().filepath_sender.get() {
                            let sender = sender.clone();
                            utils::tokio_runtime().spawn(async move {
                                let maybe_files = SelectedFiles::open_file()
                                    .title("Select a new album art")
                                    .modal(true)
                                    .multiple(false)
                                    .send()
                                    .await
                                    .expect("ashpd file open await failure")
                                    .response();

                                if let Ok(files) = maybe_files {
                                    let uris = files.uris();
                                    if uris.len() > 0 {
                                        let _ = sender.send_blocking(uris[0].to_string());
                                    }
                                }
                                else {
                                    println!("{:?}", maybe_files);
                                }
                            });
                        }
                    }
                ))
                .build();
            let action_clear_album_art = ActionEntry::builder("clear-album-art")
                .activate(clone!(
                    #[strong]
                    obj,
                    move |_, _, _| {
                        if let (Some(album), Some(library)) = (
                            obj.imp().album.borrow().as_ref(),
                            obj.imp().library.get()
                        ) {
                            library.clear_cover(album.get_folder_uri());
                        }
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([
                action_clear_rating,
                action_set_album_art,
                action_clear_album_art
            ]);
            self.obj().insert_action_group("album-content-view", Some(&actions));
        }
    }

    impl WidgetImpl for AlbumContentView {}
}

glib::wrapper! {
    pub struct AlbumContentView(ObjectSubclass<imp::AlbumContentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AlbumContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl AlbumContentView {
    fn get_library(&self) -> Option<&Library> {
        self.imp().library.get()
    }

    fn update_meta(&self, album: &Album) {
        let cache = self.imp().cache.get().unwrap().clone();
        let wiki_box = self.imp().wiki_box.get();
        let wiki_text = self.imp().wiki_text.get();
        let wiki_link = self.imp().wiki_link.get();
        let wiki_attrib = self.imp().wiki_attrib.get();
        if let Some(meta) = cache.load_cached_album_meta(album.get_info()) {
            if let Some(wiki) = meta.wiki {
                wiki_box.set_visible(true);
                wiki_text.set_label(&wiki.content);
                if let Some(url) = wiki.url.as_ref() {
                    wiki_link.set_visible(true);
                    wiki_link.set_uri(url);
                } else {
                    wiki_link.set_visible(false);
                }
                wiki_attrib.set_label(&wiki.attribution);
            } else {
                wiki_box.set_visible(false);
            }
            let infobox_spinner = self.imp().infobox_spinner.get();
            if infobox_spinner.visible_child_name().unwrap() != "content" {
                infobox_spinner.set_visible_child_name("content");
            }
        } else {
            wiki_box.set_visible(false);
        }
    }

    /// Set a user-selected path as the new local cover.
    pub fn set_cover(&self, path: &str) {
        if let (Some(album), Some(library)) = (
            self.imp().album.borrow().as_ref(),
            self.imp().library.get()
        ) {
            library.set_cover(album.get_folder_uri(), path);
        }
    }

    pub fn setup(&self, library: Library, client_state: ClientState, cache: Rc<Cache>) {
        let cache_state = cache.get_cache_state();
        self.imp()
           .cache
           .set(cache)
           .expect("AlbumContentView cannot bind to cache");
        self.imp()
            .add_to_playlist
            .setup(library.clone(), self.imp().sel_model.clone());
        self.imp().library.set(library).expect("Could not register album content view with library controller");
        cache_state.connect_closure(
            "album-art-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, uri: String| {
                    if let Some(album) = this.imp().album.borrow().as_ref() {
                        if album.get_folder_uri() == &uri {
                            // Force update since we might have been using an embedded cover
                            // temporarily
                            this.imp().cover_source.set(CoverSource::Folder);
                            this.update_cover(album.get_info());
                        } else if this.imp().cover_source.get() != CoverSource::Folder {
                            if album.get_example_uri() == &uri {
                                this.imp().cover_source.set(CoverSource::Embedded);
                                this.update_cover(album.get_info());
                            }
                        }
                    }
                }
            ),
        );
        cache_state.connect_closure(
            "album-art-cleared",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, uri: String| {
                    if let Some(album) = this.imp().album.borrow().as_ref() {
                        match this.imp().cover_source.get() {
                            CoverSource::Folder => {
                                if album.get_folder_uri() == &uri {
                                    this.imp().cover_source.set(CoverSource::None);
                                    this.update_cover(album.get_info());
                                }
                            }
                            CoverSource::Embedded => {
                                if album.get_example_uri() == &uri {
                                    this.imp().cover_source.set(CoverSource::None);
                                    this.update_cover(album.get_info());
                                }
                            }
                            _ => {}
                        }
                    }
                }
            ),
        );

        self.imp()
            .rating
            .connect_closure( 
                "changed",
                false,
                closure_local!(
                    #[strong(rename_to = this)]
                    self,
                    move |rating: Rating| {
                        if let (Some(album), Some(library)) = (
                            this.imp().album.borrow().as_ref(),
                            this.get_library()
                        ) {
                            let rating_val = rating.value();
                            let rating_opt = if rating_val > 0 { Some(rating_val)} else { None };
                            album.set_rating(rating_opt);
                            library.rate_album(album, rating_opt);
                        }
                    }
                )
            );

        cache_state.connect_closure(
            "album-meta-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, folder_uri: String| {
                    if let Some(album) = this.imp().album.borrow().as_ref() {
                        if folder_uri == album.get_folder_uri() {
                            this.update_meta(album);
                        }
                    }
                }
            ),
        );
        client_state.connect_closure(
            "album-songs-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, tag: String, songs: glib::BoxedAnyObject| {
                    if let Some(album) = this.imp().album.borrow().as_ref() {
                        if album.get_title() == tag {
                            this.add_songs(songs.borrow::<Vec<Song>>().as_ref());
                        }
                    }
                }
            ),
        );
        let infobox_revealer = self.imp().infobox_revealer.get();
        let collapse_infobox = self.imp().collapse_infobox.get();
        collapse_infobox
            .bind_property("active", &infobox_revealer, "reveal-child")
            .transform_to(|_, active: bool| Some(!active))
            .transform_from(|_, active: bool| Some(!active))
            .bidirectional()
            .sync_create()
            .build();

        infobox_revealer
            .bind_property("child-revealed", &collapse_infobox, "icon-name")
            .transform_to(|_, revealed| {
                if revealed {
                    return Some("up-symbolic");
                }
                Some("down-symbolic")
            })
            .sync_create()
            .build();

        let replace_queue_btn = self.imp().replace_queue.get();
        replace_queue_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let (Some(album), Some(library)) = (
                    this.imp().album.borrow().as_ref(),
                    this.get_library()
                ) {
                    if this.imp().selecting_all.get() {
                        library.queue_album(album.clone(), true, true, None);
                    } else {
                        let store = &this.imp().song_list;
                        // Get list of selected songs
                        let sel = &this.imp().sel_model.selection();
                        let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                        let (iter, first_idx) = BitsetIter::init_first(sel).unwrap();
                        songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                        iter.for_each(|idx| {
                            songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                        });
                        library.queue_songs(&songs, true, true);
                    }
                }
            }
        ));
        let append_queue_btn = self.imp().append_queue.get();
        append_queue_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                if let (Some(album), Some(library)) = (
                    this.imp().album.borrow().as_ref(),
                    this.get_library()
                ) { 
                    if this.imp().selecting_all.get() {
                        library.queue_album(album.clone(), false, false, None);
                    } else {
                        let store = &this.imp().song_list;
                        // Get list of selected songs
                        let sel = &this.imp().sel_model.selection();
                        let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                        let (iter, first_idx) = BitsetIter::init_first(sel).unwrap();
                        songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                        iter.for_each(|idx| {
                            songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                        });
                        library.queue_songs(&songs, false, false);
                    }
                }
            }
        ));

        // Set up channel for listening to album art dialog
        // It is in these situations that Rust's lack of a standard async library bites hard.
        let (sender, receiver) = async_channel::unbounded::<String>();
        let _ = self.imp().filepath_sender.set(sender);
        glib::MainContext::default().spawn_local(clone!(
            #[strong(rename_to = this)]
            self,
            async move {
                use futures::prelude::*;
                // Allow receiver to be mutated, but keep it at the same memory address.
                // See Receiver::next doc for why this is needed.
                let mut receiver = std::pin::pin!(receiver);

                while let Some(path) = receiver.next().await {
                    this.set_cover(&path);
                }
            }
        ));

        // Set up factory
        let factory = SignalListItemFactory::new();

        // Create an empty `AlbumSongRow` during setup
        factory.connect_setup(clone!(
            #[strong(rename_to = this)]
            self,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = AlbumSongRow::new(
                    this.get_library().expect("Error: album content view not connected to library").clone(),
                    &item
                );
                item.set_child(Some(&row));
            }
        ));
        // Tell factory how to bind `AlbumSongRow` to one of our Album GObjects
        factory.connect_bind(move |_, list_item| {
            // Get `Song` from `ListItem` (that is, the data side)
            let item: Song = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Song>()
                .expect("The item has to be a common::Song.");

            // Get `AlbumSongRow` from `ListItem` (the UI widget)
            let child: AlbumSongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumSongRow>()
                .expect("The child has to be an `AlbumSongRow`.");

            // Within this binding fn is where the cached album art texture gets used.
            child.bind(&item);
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `AlbumSongRow` from `ListItem` (the UI widget)
            let child: AlbumSongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumSongRow>()
                .expect("The child has to be an `AlbumSongRow`.");
            child.unbind();
        });

        // Set the factory of the list view
        self.imp().content.set_factory(Some(&factory));

        // Setup click action
        self.imp().content.connect_activate(clone!(
            #[strong(rename_to = this)]
            self,
            move |_, position| {
                if let (Some(album), Some(library)) = (
                    this.imp().album.borrow().as_ref(),
                    this.get_library()
                ) {
                    library.queue_album(album.clone(), true, true, Some(position as u32));
                }
            }
        ));
    }

    fn update_cover(&self, info: &AlbumInfo) {
        let mut set: bool = false;
        match self.imp().cover_source.get() {
            // No scheduling (already called by the outside AlbumCell)
            CoverSource::Unknown => {
                // Schedule when in this mode
                if let Some((tex, is_embedded)) = self
                    .imp()
                    .cache
                    .get()
                    .unwrap()
                    .load_cached_folder_cover(info, false, true, true) {
                        self.imp().cover.set_paintable(Some(&tex));
                        self.imp().cover_source.set(
                            if is_embedded {CoverSource::Embedded} else {CoverSource::Folder}
                        );
                        set = true;
                    }
            }
            CoverSource::Folder => {
                if let Some((tex, _)) = self
                    .imp()
                    .cache
                    .get()
                    .unwrap()
                    .load_cached_folder_cover(info, false, false, false) {
                        self.imp().cover.set_paintable(Some(&tex));
                        set = true;
                    }
            }
            CoverSource::Embedded => {
                if let Some((tex, _)) = self
                    .imp()
                    .cache
                    .get()
                    .unwrap()
                    .load_cached_embedded_cover_for_album(info, false, false, false) {
                        self.imp().cover.set_paintable(Some(&tex));
                        set = true;
                    }
            }
            CoverSource::None => {
                self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
                set = true;
            }
        }
        if !set {
            self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
        }
    }

    pub fn bind(&self, album: Album) {
        let title_label = self.imp().title.get();
        let artist_label = self.imp().artist.get();
        let rating = self.imp().rating.get();
        let release_date_label = self.imp().release_date.get();
        let mut bindings = self.imp().bindings.borrow_mut();

        let title_binding = album
            .bind_property("title", &title_label, "label")
            .sync_create()
            .build();
        // Save binding
        bindings.push(title_binding);

        let artist_binding = album
            .bind_property("artist", &artist_label, "label")
            .sync_create()
            .build();
        // Save binding
        bindings.push(artist_binding);

        let rating_binding = album
            .bind_property("rating", &rating, "value")
            .sync_create()
            .build();
        // Save binding
        bindings.push(rating_binding);

        self.update_meta(&album);
        let release_date_binding = album
            .bind_property("release_date", &release_date_label, "label")
            .transform_to(|_, boxed_date: glib::BoxedAnyObject| {
                let format = format_description::parse("[year]-[month]-[day]")
                    .ok()
                    .unwrap();
                if let Some(release_date) = boxed_date.borrow::<Option<Date>>().as_ref() {
                    return release_date.format(&format).ok();
                }
                Some("-".to_owned())
            })
            .sync_create()
            .build();
        // Save binding
        bindings.push(release_date_binding);

        let release_date_viz_binding = album
            .bind_property("release_date", &release_date_label, "visible")
            .transform_to(|_, boxed_date: glib::BoxedAnyObject| {
                if boxed_date.borrow::<Option<Date>>().is_some() {
                    return Some(true);
                }
                Some(false)
            })
            .sync_create()
            .build();
        // Save binding
        bindings.push(release_date_viz_binding);

        let info = album.get_info();
        self.imp().cover_source.set(CoverSource::Unknown);
        self.update_cover(info);
        self.imp().album.borrow_mut().replace(album);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
        if let Some(id) = self.imp().cover_signal_id.take() {
            if let Some(cache) = self.imp().cache.get() {
                cache.get_cache_state().disconnect(id);
            }
        }
        if let Some(album) = self.imp().album.take() {
            self.imp().cover_source.set(CoverSource::None);
            self.update_cover(album.get_info());
        }

        
        // Unset metadata widgets
        self.imp().wiki_box.set_visible(false);
        self.imp().song_list.remove_all();
        let content_spinner = self.imp().content_spinner.get();
        if content_spinner.visible_child_name().unwrap() != "spinner" {
            content_spinner.set_visible_child_name("spinner");
        }
        let infobox_spinner = self.imp().infobox_spinner.get();
        if infobox_spinner.visible_child_name().unwrap() != "spinner" {
            infobox_spinner.set_visible_child_name("spinner");
        }
    }

    fn add_songs(&self, songs: &[Song]) {
        let content_spinner = self.imp().content_spinner.get();
        if content_spinner.visible_child_name().unwrap() != "content" {
            content_spinner.set_visible_child_name("content");
        }
        self.imp().song_list.extend_from_slice(songs);
        self.imp()
            .track_count
            .set_label(&self.imp().song_list.n_items().to_string());
        self.imp().runtime.set_label(&format_secs_as_duration(
            self.imp()
                .song_list
                .iter()
                .map(|item: Result<Song, _>| {
                    if let Ok(song) = item {
                        return song.get_duration();
                    }
                    0
                })
                .sum::<u64>() as f64,
        ));
    }
}
