use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use derivative::Derivative;
use duplicate::duplicate;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{Binding, WeakRef, clone, closure_local, signal::SignalHandlerId, subclass::Signal};
use gtk::{CompositeTemplate, ListItem, SignalListItemFactory, gdk, gio, glib, prelude::*};
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
    sync::OnceLock,
};

use super::{AlbumCell, Library};
use crate::{
    cache::{Cache, CacheState, placeholders::EMPTY_ARTIST_STRING},
    common::{Album, Artist, ContentView, RowAddButtons, Song, SongRow},
    library::add_to_playlist::AddToPlaylistButton,
    utils::{format_secs_as_duration, settings_manager, tokio_runtime},
};

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-content-view.ui")]
    pub struct ArtistContentView {
        #[template_child]
        pub inner: TemplateChild<ContentView>,
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub song_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_count: TemplateChild<gtk::Label>,

        #[template_child]
        pub infobox_spinner: TemplateChild<gtk::Stack>,

        #[template_child]
        pub bio_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub bio_link: TemplateChild<gtk::LinkButton>,
        #[template_child]
        pub bio_attrib: TemplateChild<gtk::Label>,
        // #[template_child]
        // pub runtime: TemplateChild<gtk::Label>,
        //
        #[template_child]
        pub all_songs_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub subview_stack: TemplateChild<gtk::Stack>,

        // All songs sub-view
        #[template_child]
        pub song_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub song_subview: TemplateChild<gtk::ListView>,
        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        #[derivative(Default(value = "gtk::MultiSelection::new(Option::<gio::ListStore>::None)"))]
        pub song_sel_model: gtk::MultiSelection,
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

        // Discography sub-view
        #[template_child]
        pub album_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_subview: TemplateChild<gtk::GridView>,
        #[derivative(Default(value = "gio::ListStore::new::<Album>()"))]
        pub album_list: gio::ListStore,

        pub library: WeakRef<Library>,
        pub artist: RefCell<Option<Artist>>,
        pub bindings: RefCell<Vec<Binding>>,
        pub avatar_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        #[derivative(Default(value = "Cell::new(true)"))]
        pub selecting_all: Cell<bool>, // Enables queuing all songs from this artist efficiently
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArtistContentView {
        const NAME: &'static str = "EuphonicaArtistContentView";
        type Type = super::ArtistContentView;
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

    impl ObjectImpl for ArtistContentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            // Set up song subview
            self.song_sel_model.set_model(Some(&self.song_list.clone()));
            self.song_subview.set_model(Some(&self.song_sel_model));

            // Change button labels depending on selection state
            self.song_sel_model.connect_selection_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, _| this.on_song_selection_changed()
            ));

            let song_sel_model = self.song_sel_model.clone();
            self.sel_all.connect_clicked(clone!(
                #[weak]
                song_sel_model,
                move |_| {
                    song_sel_model.select_all();
                }
            ));
            self.sel_none.connect_clicked(clone!(
                #[weak]
                song_sel_model,
                move |_| {
                    song_sel_model.unselect_all();
                }
            ));

            // Set up album subview
            let album_sel_model = gtk::SingleSelection::new(Some(self.album_list.clone()));
            self.album_subview.set_model(Some(&album_sel_model));
            self.album_subview.connect_activate(clone!(
                #[weak(rename_to = this)]
                self,
                move |view, position| {
                    let model = view.model().expect("The model has to exist.");
                    let album = model
                        .item(position)
                        .and_downcast::<Album>()
                        .expect("The item has to be a `common::Album`.");

                    this.obj()
                        .emit_by_name::<()>("album-clicked", &[&album.to_value()]);
                }
            ));

            self.all_songs_btn
                .bind_property("active", &self.subview_stack.get(), "visible-child-name")
                .transform_to(|_, active| {
                    if active {
                        Some("songs")
                    } else {
                        Some("albums")
                    }
                })
                .sync_create()
                .build();

            // Edit actions
            let obj = self.obj();
            let action_set_avatar = ActionEntry::builder("set-avatar")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        let (sender, receiver) = oneshot::channel();
                        tokio_runtime().spawn(async move {
                            let maybe_files = SelectedFiles::open_file()
                                .title("Select a new avatar")
                                .modal(true)
                                .multiple(false)
                                .send()
                                .await
                                .expect("ashpd file open await failure")
                                .response();

                            sender.send(
                                if let Ok(files) = maybe_files {
                                    let uris = files.uris();
                                    if !uris.is_empty() {
                                        Some(uris[0].to_string())
                                    } else {
                                        None
                                    }
                                } else {
                                    println!("{maybe_files:?}");
                                    None
                                }
                            );
                        });
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let Some(tag) = receiver.await.expect("Broken oneshot receiver") {
                                    obj.set_avatar(tag);
                                }
                            }
                        ));
                    }
                ))
                .build();
            let action_clear_avatar = ActionEntry::builder("clear-avatar")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let (Some(artist), Some(cache)) = (
                                    obj.imp().artist.borrow().as_ref(),
                                    obj.imp().cache.get(),
                                ) {
                                    cache.clear_artist_avatar(artist.get_name().to_owned()).await;
                                }
                            }
                        ));
                    }
                ))
                .build();

            let action_refetch_metadata = ActionEntry::builder("refetch-metadata")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                obj.schedule_avatar().await;
                                obj.update_meta(true).await;
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([
                action_set_avatar,
                action_clear_avatar,
                action_refetch_metadata,
            ]);
            self.obj()
                .insert_action_group("artist-content-view", Some(&actions));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("album-clicked")
                        .param_types([Album::static_type()])
                        .build(),
                ]
            })
        }
    }

    impl WidgetImpl for ArtistContentView {}

    impl ArtistContentView {
        pub fn on_song_selection_changed(&self) {
            let sel_model = &self.song_sel_model;
            // TODO: self can be slow, might consider redesigning
            let n_sel = sel_model.selection().size();
            if n_sel == 0 || (n_sel as u32) == sel_model.model().unwrap().n_items() {
                self.selecting_all.replace(true);
                self.replace_queue_text.set_label("Play all");
                self.append_queue_text.set_label("Queue all");
            } else {
                // TODO: l10n
                self.selecting_all.replace(false);
                self.replace_queue_text
                    .set_label(format!("Play {n_sel}").as_str());
                self.append_queue_text
                    .set_label(format!("Queue {n_sel}").as_str());
            }
        }
    }
}

glib::wrapper! {
    pub struct ArtistContentView(ObjectSubclass<imp::ArtistContentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ArtistContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ArtistContentView {
    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().append_queue.set_sensitive(!queuing);
    }

    fn hide_bio(&self) {
        self.imp().infobox_spinner.set_visible(false);
        self.imp().bio_text.set_visible(false);
        self.imp().bio_text.set_label("");
        self.imp().bio_attrib.set_visible(false);
        self.imp().bio_attrib.set_label("");
        self.imp().bio_link.set_visible(false);
        self.imp().bio_link.set_uri("");
    }

    async fn update_meta(&self, overwrite: bool) {
        if let Some(artist) = self.imp().artist.borrow().as_ref() {
            let stack = self.imp().infobox_spinner.get();
            // If the current artist is the "untitled" one (i.e. for songs without an artist tag),
            // don't attempt to update metadata.
            if artist.get_name().is_empty() {
                stack.set_visible(false);
            } else {
                stack.set_visible(true);
                if stack.visible_child_name().is_none_or(|name| name != "spinner")  {
                    stack.set_visible_child_name("spinner");
                }
                let cache = self.imp().cache.get().unwrap().clone();
                let bio_text = self.imp().bio_text.get();
                let bio_link = self.imp().bio_link.get();
                let bio_attrib = self.imp().bio_attrib.get();
                let res = cache.get_artist_meta(artist.get_info(), true, overwrite).await;
                stack.set_visible_child_name("content");
                match res {
                    Ok(Some(meta)) => {
                        if let Some(bio) = meta.bio {
                            stack.set_visible(true);
                            bio_text.set_visible(true);
                            bio_text.set_label(&bio.content);
                            if let Some(url) = bio.url.as_ref() {
                                bio_link.set_visible(true);
                                bio_link.set_uri(url);
                            } else {
                                bio_link.set_visible(false);
                                bio_link.set_uri("");
                            }
                            bio_attrib.set_visible(true);
                            bio_attrib.set_label(&bio.attribution);
                            if stack.visible_child_name().unwrap() != "content" {
                                stack.set_visible_child_name("content");
                            }
                        } else {
                            self.hide_bio();
                        }
                    }
                    Ok(None) => {
                        self.hide_bio();
                    }
                    Err(e) => {
                        self.hide_bio();
                        dbg!(e);
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn setup_info_box(&self) {
        let cache = self.imp().cache.get().unwrap();
        cache.get_cache_state().connect_closure(
            "artist-avatar-set",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, name: String, hires: gdk::Texture, _: gdk::Texture| {
                    if this.imp().artist.borrow().as_ref().is_some_and(|a| a.get_name() == name) {
                        this.update_avatar(Some(&hires));
                    }
                }
            ),
        );
        cache.get_cache_state().connect_closure(
            "artist-avatar-cleared",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, tag: String| {
                    if this.imp().artist.borrow().as_ref().is_some_and(|a| a.get_name() == tag) {
                        this.update_avatar(None);
                    }
                }
            ),
        );
    }

    fn setup_song_subview(&self) {
        // Hook up buttons
        let replace_queue_btn = self.imp().replace_queue.get();
        replace_queue_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let Some(artist) = this.imp().artist.borrow().as_ref() {
                            this.set_is_queuing(true);
                            let library = this.imp().library.upgrade().unwrap();
                            if this.imp().selecting_all.get() {
                                if let Err(e) = library.queue_artist(artist, false, true, true).await {dbg!(e);}
                            } else {
                                let store = &this.imp().song_list;
                                // Get list of selected songs
                                let sel = &this.imp().song_sel_model.selection();
                                let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                                let (iter, first_idx) = gtk::BitsetIter::init_first(sel).unwrap();
                                songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                                iter.for_each(|idx| {
                                    songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                                });
                                if let Err(e) = library.queue_songs(&songs, true, true).await {dbg!(e);}
                            }
                            this.set_is_queuing(false);
                        }
                    }
                ));
            }
        ));
        let append_queue_btn = self.imp().append_queue.get();
        append_queue_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let Some(artist) = this.imp().artist.borrow().as_ref() {
                            this.set_is_queuing(true);
                            let library = this.imp().library.upgrade().unwrap();
                            if this.imp().selecting_all.get() {
                                library.queue_artist(artist, false, false, false).await;
                            } else {
                                let store = &this.imp().song_list;
                                // Get list of selected songs
                                let sel = &this.imp().song_sel_model.selection();
                                let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                                let (iter, first_idx) = gtk::BitsetIter::init_first(sel).unwrap();
                                songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                                iter.for_each(|idx| {
                                    songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                                });
                                library.queue_songs(&songs, false, false).await;
                            }
                        }
                        this.set_is_queuing(false);
                    }
                ));
            }
        ));

        // Set up factory
        let library = self.imp().library.upgrade().unwrap();
        let cache = self.imp().cache.get().unwrap();
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak]
            library,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache), None);
                item.property_expression("item")
                    .chain_property::<Song>("name")
                    .bind(&row, "name", gtk::Widget::NONE);

                row.set_first_attrib_icon_name(Some("library-music-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("album")
                    .bind(&row, "first-attrib-text", gtk::Widget::NONE);

                row.set_second_attrib_icon_name(Some("hourglass-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("duration")
                    .chain_closure::<String>(closure_local!(|_: Option<glib::Object>, dur: u64| {
                        format_secs_as_duration(dur as f64)
                    }))
                    .bind(&row, "second-attrib-text", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("quality-grade")
                    .bind(&row, "quality-grade", gtk::Widget::NONE);
                let end_widget = RowAddButtons::new(&library);
                row.set_end_widget(Some(&end_widget.into()));
                item.set_child(Some(&row));
            }
        ));
        factory.connect_bind(move |_, list_item| {
            // Get `Song` from `ListItem` (that is, the data side)
            let item: Song = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Song>()
                .expect("The item has to be a common::Song.");

            // Get `SongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be a `SongRow`.");

            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(Some(&item));
            child.on_bind(&item);
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `SongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be a `SongRow`.");
            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(None);
            child.on_unbind();
        });

        // Set the factory of the list view
        self.imp().song_subview.set_factory(Some(&factory));
    }

    fn setup_album_subview(&self) {
        let settings = settings_manager().child("ui");

        // Set up factory
        let cache = self.imp().cache.get().unwrap();
        let factory = SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                // TODO: refactor album cells to use expressions too
                let album_cell = AlbumCell::new(item, cache, None);
                item.set_child(Some(&album_cell));
            }
        ));
        factory.connect_bind(move |_, list_item| {
            let item: Album = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Album>()
                .expect("The item has to be a common::Album.");
            let child: AlbumCell = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumCell>()
                .expect("The child has to be an `AlbumCell`.");

            // Within this binding fn is where the cached artist avatar texture gets used.
            child.bind(&item);
        });

        factory.connect_unbind(move |_, list_item| {
            let child: AlbumCell = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumCell>()
                .expect("The child has to be an `AlbumCell`.");
            child.unbind();
        });

        // Set the factory of the grid view
        let grid_view = self.imp().album_subview.get();
        grid_view.set_factory(Some(&factory));
        settings
            .bind("max-columns", &grid_view, "max-columns")
            .build();
    }

    pub fn setup(&self, library: &Library, cache: Rc<Cache>) {
        self.imp()
            .cache
            .set(cache)
            .expect("Could not register artist content view with cache controller");
        self.imp().library.set(Some(library));

        self.setup_info_box();
        self.setup_song_subview();
        self.setup_album_subview();

        self.imp()
            .add_to_playlist
            .setup(library, &self.imp().song_sel_model);
    }

    /// Set a user-selected path as the new local avatar.
    pub fn set_avatar(&self, path: String) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let (Some(artist), Some(cache)) = (
                    this.imp().artist.borrow().as_ref(),
                    this.imp().cache.get(),
                ) {
                    cache.set_artist_avatar(artist.get_name().to_owned(), &path).await;
                }
            }
        ));
    }

    #[inline]
    fn update_avatar(&self, tex: Option<&gdk::Texture>) {
        // Set text in case there is no image
        self.imp().avatar.set_custom_image(tex);
    }

    async fn schedule_avatar(&self) {
        self.update_avatar(None);
        if let Some(info) = self.imp().artist.borrow().as_ref().map(|a| a.get_info()) {
            match self.imp().cache.get().unwrap().clone().get_artist_avatar(
                info, false, true  // Content page is the one to fetch external sources
            ).await {
                Ok(maybe_tex) => {
                    self.update_avatar(maybe_tex.as_ref());
                }
                Err(e) => {dbg!(e);}
            }
        }
    }

    pub fn bind(&self, artist: &Artist) {
        self.imp().on_song_selection_changed();
        let info = artist.get_info();
        self.imp().avatar.set_text(Some(&info.name));

        let name_label = self.imp().name.get();
        let mut bindings = self.imp().bindings.borrow_mut();

        let name_binding = artist
            .bind_property("name", &name_label, "label")
            .transform_to(|_, s: Option<&str>| {
                Some(if s.is_none_or(|s| s.is_empty()) {
                    (*EMPTY_ARTIST_STRING).to_value()
                } else {
                    s.to_value()
                })
            })
            .sync_create()
            .build();
        // Save binding
        bindings.push(name_binding);

        // Save reference to artist object
        self.imp().artist.borrow_mut().replace(artist.clone());

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            artist,
            async move {
                let album_spinner = this.imp().album_spinner.get();
                let library = this.imp().library.upgrade().unwrap();
                if album_spinner.visible_child_name().is_none_or(|name| name != "spinner") {
                    album_spinner.set_visible_child_name("spinner");
                }
                let album_list = this.imp().album_list.clone();
                album_list.remove_all();
                let song_spinner = this.imp().song_spinner.get();
                if song_spinner.visible_child_name().is_none_or(|name| name != "spinner") {
                    song_spinner.set_visible_child_name("spinner");
                }
                let song_list = this.imp().song_list.clone();
                song_list.remove_all();
                // Important, MPD-side content first
                library.get_artist_content(
                    &artist,
                    |album| {album_list.append(&album);},
                    |songs| {song_list.extend_from_slice(&songs);}
                ).await;
                album_spinner.set_visible_child_name("content");
                song_spinner.set_visible_child_name("content");
                this.imp()
                    .song_count
                    .set_label(&this.imp().song_list.n_items().to_string());

                // The extra fluff later
                this.schedule_avatar().await;
                this.update_meta(false).await;
            }
        ));
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.take().into_iter() {
            binding.unbind();
        }
        if let Some(id) = self.imp().avatar_signal_id.take() {
            if let Some(cache) = self.imp().cache.get() {
                cache.get_cache_state().disconnect(id);
            }
        }
        // Unset metadata widgets
        self.imp().avatar.set_text(None);
        self.clear_content();
        duplicate! {
            [stack; [infobox_spinner]; [song_spinner]; [album_spinner];]
            if self.imp().stack.visible_child_name().unwrap() != "spinner" {
                self.imp().stack.set_visible_child_name("spinner");
            }
        }
        self.imp().infobox_spinner.set_visible(true);
    }

    fn clear_content(&self) {
        self.imp().song_list.remove_all();
        self.imp().album_list.remove_all();
    }
}
