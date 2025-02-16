use adw::subclass::prelude::*;
use glib::{clone, closure_local, signal::SignalHandlerId, Binding};
use gtk::{gdk, gio, glib, prelude::*, CompositeTemplate, ListItem, SignalListItemFactory};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use super::{AlbumCell, ArtistSongRow, Library};
use crate::{
    cache::{Cache, CacheState},
    client::ClientState,
    common::{Album, AlbumInfo, Artist, ArtistInfo, Song},
};

mod imp {
    use std::{cell::Cell, sync::OnceLock};

    use glib::subclass::Signal;

    use crate::library::add_to_playlist::AddToPlaylistButton;

    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-content-view.ui")]
    pub struct ArtistContentView {
        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub song_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_count: TemplateChild<gtk::Label>,

        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub collapse_infobox: TemplateChild<gtk::ToggleButton>,

        #[template_child]
        pub bio_box: TemplateChild<gtk::ScrolledWindow>,
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
        pub song_subview: TemplateChild<gtk::ListView>,
        pub song_list: gio::ListStore,
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
        pub album_subview: TemplateChild<gtk::GridView>,
        pub album_list: gio::ListStore,

        pub artist: RefCell<Option<Artist>>,
        pub bindings: RefCell<Vec<Binding>>,
        pub avatar_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub selecting_all: Cell<bool>, // Enables queuing all songs from this artist efficiently
    }

    impl Default for ArtistContentView {
        fn default() -> Self {
            Self {
                avatar: TemplateChild::default(),
                name: TemplateChild::default(),
                song_count: TemplateChild::default(),
                album_count: TemplateChild::default(),
                infobox_revealer: TemplateChild::default(),
                collapse_infobox: TemplateChild::default(),
                bio_box: TemplateChild::default(),
                bio_text: TemplateChild::default(),
                bio_link: TemplateChild::default(),
                bio_attrib: TemplateChild::default(),
                // runtime: TemplateChild::default(),
                all_songs_btn: TemplateChild::default(),
                subview_stack: TemplateChild::default(),
                // All songs sub-view
                song_subview: TemplateChild::default(),
                song_list: gio::ListStore::new::<Song>(),
                song_sel_model: gtk::MultiSelection::new(Option::<gio::ListStore>::None),
                replace_queue: TemplateChild::default(),
                append_queue: TemplateChild::default(),
                replace_queue_text: TemplateChild::default(),
                append_queue_text: TemplateChild::default(),
                add_to_playlist: TemplateChild::default(),
                sel_all: TemplateChild::default(),
                sel_none: TemplateChild::default(),
                // Discography sub-view
                album_subview: TemplateChild::default(),
                album_list: gio::ListStore::new::<Album>(),
                artist: RefCell::new(None),
                bindings: RefCell::new(Vec::new()),
                avatar_signal_id: RefCell::new(None),
                cache: OnceCell::new(),
                selecting_all: Cell::new(true), // When nothing is selected, default to select-all
            }
        }
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
                .bind_property(
                    "active",
                    &self.subview_stack.get(),
                    "visible-child-name"
                )
                .transform_to(
                    |_, active| {
                        if active {
                            Some("songs")
                        }
                        else {
                            Some("albums")
                        }
                    }
                )
                .sync_create()
                .build();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![Signal::builder("album-clicked")
                    .param_types([Album::static_type()])
                    .build()]
            })
        }
    }

    impl WidgetImpl for ArtistContentView {}
}

glib::wrapper! {
    pub struct ArtistContentView(ObjectSubclass<imp::ArtistContentView>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for ArtistContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ArtistContentView {
    fn update_meta(&self, artist: &Artist) {
        let cache = self.imp().cache.get().unwrap().clone();
        let bio_box = self.imp().bio_box.get();
        let bio_text = self.imp().bio_text.get();
        let bio_link = self.imp().bio_link.get();
        let bio_attrib = self.imp().bio_attrib.get();
        if let Some(meta) = cache.load_cached_artist_meta(artist.get_info()) {
            if let Some(bio) = meta.bio {
                bio_box.set_visible(true);
                bio_text.set_label(&bio.content);
                if let Some(url) = bio.url.as_ref() {
                    bio_link.set_visible(true);
                    bio_link.set_uri(url);
                } else {
                    bio_link.set_visible(false);
                }
                bio_attrib.set_label(&bio.attribution);
            } else {
                bio_box.set_visible(false);
            }
        } else {
            bio_box.set_visible(false);
        }
    }

    #[inline(always)]
    fn setup_info_box(&self, cache: Rc<Cache>) {
        cache.get_cache_state().connect_closure(
            "artist-avatar-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, name: String| {
                    if let Some(artist) = this.imp().artist.borrow().as_ref() {
                        if name == artist.get_name() {
                            this.update_avatar(artist.get_info());
                        }
                    }
                }
            ),
        );
        cache.get_cache_state().connect_closure(
            "artist-meta-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, name: String| {
                    if let Some(artist) = this.imp().artist.borrow().as_ref() {
                        if name == artist.get_name() {
                            this.update_meta(artist);
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
    }

    fn setup_song_subview(&self, library: Library, cache: Rc<Cache>, client_state: ClientState) {
        // Hook up buttons
        let replace_queue_btn = self.imp().replace_queue.get();
        replace_queue_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            #[weak]
            library,
            move |_| {
                if let Some(artist) = this.imp().artist.borrow().as_ref() {
                    if this.imp().selecting_all.get() {
                        library.queue_artist(artist.clone(), false, true, true);
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
                        library.queue_songs(&songs, true, true);
                    }
                }
            }
        ));
        let append_queue_btn = self.imp().append_queue.get();
        append_queue_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            #[weak]
            library,
            move |_| {
                if let Some(artist) = this.imp().artist.borrow().as_ref() {
                    if this.imp().selecting_all.get() {
                        library.queue_artist(artist.clone(), false, false, false);
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
                        library.queue_songs(&songs, false, false);
                    }
                }
            }
        ));

        client_state.connect_closure(
            "artist-songs-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                cache,
                move |_: ClientState, name: String, songs: glib::BoxedAnyObject| {
                    if let Some(artist) = this.imp().artist.borrow().as_ref() {
                        if name == artist.get_name() {
                            this.add_songs(songs.borrow::<Vec<Song>>().as_ref(), cache);
                        }
                    }
                }
            ),
        );

        // Set up factory
        let factory = SignalListItemFactory::new();

        // Create an empty `ArtistSongRow` during setup
        factory.connect_setup(clone!(
            #[weak]
            library,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let song_row = ArtistSongRow::new(library, &item);
                item.set_child(Some(&song_row));
            }
        ));
        // Tell factory how to bind `ArtistSongRow` to one of our Artist GObjects
        factory.connect_bind(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                // Get `Song` from `ListItem` (that is, the data side)
                let item: Song = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .item()
                    .and_downcast::<Song>()
                    .expect("The item has to be a common::Song.");

                // Get `ArtistSongRow` from `ListItem` (the UI widget)
                let child: ArtistSongRow = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .child()
                    .and_downcast::<ArtistSongRow>()
                    .expect("The child has to be an `ArtistSongRow`.");

                // Within this binding fn is where the cached artist avatar texture gets used.
                child.bind(&item, cache);
            }
        ));

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `ArtistSongRow` from `ListItem` (the UI widget)
            let child: ArtistSongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<ArtistSongRow>()
                .expect("The child has to be an `ArtistSongRow`.");
            child.unbind();
        });

        // Set the factory of the list view
        self.imp().song_subview.set_factory(Some(&factory));
    }

    fn setup_album_subview(&self, cache: Rc<Cache>, client_state: ClientState) {
        // TODO: handle click (switch to album tab & push album content page)
        // Unlike songs, we receive albums one by one.
        client_state.connect_closure(
            "artist-album-basic-info-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                cache,
                move |_: ClientState, name: String, album: Album| {
                    if let Some(artist) = this.imp().artist.borrow().as_ref() {
                        if name == artist.get_name() {
                            this.add_album(album, cache);
                        }
                    }
                }
            ),
        );

        // Set up factory
        let factory = SignalListItemFactory::new();
        factory.connect_setup(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                // TODO: refactor album cells to use expressions too
                let album_cell = AlbumCell::new(&item, cache);
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

        // Set the factory of the list view
        self.imp().album_subview.set_factory(Some(&factory));
    }

    pub fn setup(&self, library: Library, cache: Rc<Cache>, client_state: ClientState) {
        let _ = self.imp().cache.set(cache.clone());
        self.setup_info_box(cache.clone());
        self.setup_song_subview(library.clone(), cache.clone(), client_state.clone());
        self.setup_album_subview(cache, client_state);

        self.imp()
            .add_to_playlist
            .setup(library.clone(), self.imp().song_sel_model.clone());
    }

    /// Returns true if an avatar was successfully retrieved.
    /// On false, we will want to call cache.ensure_cached_album_art()
    fn update_avatar(&self, info: &ArtistInfo) -> bool {
        // Set text in case there is no image
        self.imp().avatar.set_text(Some(&info.name));
        if let Some(cache) = self.imp().cache.get() {
            if let Some(tex) = cache.load_cached_artist_avatar(info, false) {
                self.imp().avatar.set_custom_image(Some(&tex));
                return true;
            } else {
                self.imp()
                    .avatar
                    .set_custom_image(Option::<&gdk::Texture>::None);
                return false;
            }
        }
        false
    }

    pub fn bind(&self, artist: Artist) {
        println!("Binding to artist: {:?}", &artist);
        self.update_meta(&artist);
        let info = artist.get_info();
        self.update_avatar(info);

        let name_label = self.imp().name.get();
        let mut bindings = self.imp().bindings.borrow_mut();

        let name_binding = artist
            .bind_property("name", &name_label, "label")
            .sync_create()
            .build();
        // Save binding
        bindings.push(name_binding);

        // Save reference to artist object
        self.imp().artist.borrow_mut().replace(artist);
    }

    pub fn unbind(&self) {
        println!("Artist content page hidden. Unbinding...");
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
        if let Some(id) = self.imp().avatar_signal_id.take() {
            if let Some(cache) = self.imp().cache.get() {
                cache.get_cache_state().disconnect(id);
            }
        }
        // Unset metadata widgets
        self.imp().bio_box.set_visible(false);
        self.imp().avatar.set_text(None);
        self.clear_content();
    }

    fn add_album(&self, album: Album, cache: Rc<Cache>) {
        self.imp().album_list.append(&album);
        cache.ensure_cached_album_art(album.get_info(), false);
        self.imp()
            .album_count
            .set_label(&self.imp().album_list.n_items().to_string());
    }

    pub fn add_songs(&self, songs: &[Song], cache: Rc<Cache>) {
        self.imp().song_list.extend_from_slice(songs);
        let infos: Vec<&AlbumInfo> = songs
            .into_iter()
            .map(|song| song.get_album())
            .filter(|ao| ao.is_some())
            .map(|info| info.unwrap())
            .collect();
        // Might queue downloads, depending on user settings, but will not
        // actually load anything into memory just yet.
        cache.ensure_cached_album_arts(&infos);
        self.imp()
            .song_count
            .set_label(&self.imp().song_list.n_items().to_string());
    }

    fn clear_content(&self) {
        self.imp().song_list.remove_all();
        self.imp().album_list.remove_all();
    }
}
