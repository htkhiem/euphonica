use super::{Library, artist_tag::ArtistTag};
use crate::{
    cache::{
        Cache, CacheState,
        placeholders::{ALBUMART_PLACEHOLDER, EMPTY_ALBUM_STRING},
    },
    client::{ClientState, state::StickersSupportLevel},
    common::{
        Album, AlbumInfo, Artist, ContentView, CoverSource, Rating, RowAddButtons, Song, SongRow,
    },
    library::add_to_playlist::AddToPlaylistButton,
    utils::{format_secs_as_duration, tokio_runtime},
    window::EuphonicaWindow,
};
use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use derivative::Derivative;
use gio::{ActionEntry, Menu, SimpleActionGroup};
use glib::{Binding, WeakRef, clone, closure_local, signal::SignalHandlerId};
use gtk::{
    BitsetIter, CompositeTemplate, ListItem, SignalListItemFactory, gdk, gio, glib, prelude::*,
};
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};
use time::{Date, format_description};

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-content-view.ui")]
    pub struct AlbumContentView {
        #[template_child]
        pub inner: TemplateChild<ContentView>,
        #[template_child]
        pub cover: TemplateChild<gtk::Image>,

        #[template_child]
        pub infobox_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub artists_box: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub rating: TemplateChild<Rating>,
        #[template_child]
        pub rating_readout: TemplateChild<gtk::Label>,

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
        pub content_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListView>,

        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        #[derivative(Default(value = "gtk::MultiSelection::new(Option::<gio::ListStore>::None)"))]
        pub sel_model: gtk::MultiSelection,
        #[derivative(Default(value = "gio::ListStore::new::<ArtistTag>()"))]
        pub artist_tags: gio::ListStore,
        pub library: WeakRef<Library>,
        pub album: RefCell<Option<Album>>,
        pub window: WeakRef<EuphonicaWindow>,
        pub bindings: RefCell<Vec<Binding>>,
        pub cover_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        #[derivative(Default(value = "Cell::new(true)"))]
        pub selecting_all: Cell<bool>, // Enables queuing the entire album efficiently
        pub cover_source: Cell<CoverSource>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumContentView {
        const NAME: &'static str = "EuphonicaAlbumContentView";
        type Type = super::AlbumContentView;
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
                move |_, _, _| this.on_selection_changed()
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
                .bind_property("value", &self.rating_readout.get(), "label")
                .transform_to(|_, r: i8| {
                    // TODO: l10n
                    if r < 0 {
                        Some("Unrated".to_value())
                    } else {
                        Some(format!("{:.1}", r as f32 / 2.0).to_value())
                    }
                })
                .sync_create()
                .build();

            // Edit actions
            let obj = self.obj();
            let action_clear_rating = ActionEntry::builder("clear-rating")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        if let (Some(album), Some(library)) = (
                            obj.imp().album.borrow().as_ref(),
                            obj.imp().library.upgrade(),
                        ) {
                            library.rate_album(album, None);
                            obj.imp().rating.set_value(-1);
                        }
                    }
                ))
                .build();
            let action_set_album_art = ActionEntry::builder("set-album-art")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        let (sender, receiver) = async_channel::unbounded();
                        tokio_runtime().spawn(async move {
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
                                if !uris.is_empty() {
                                    let _ = sender.send_blocking(uris[0].to_string());
                                }
                            } else {
                                println!("{maybe_files:?}");
                            }
                        });
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            #[upgrade_or]
                            (),
                            async move {
                                use futures::prelude::*;
                                // Allow receiver to be mutated, but keep it at the same memory address.
                                // See Receiver::next doc for why this is needed.
                                let mut receiver = std::pin::pin!(receiver);

                                if let Some(path) = receiver.next().await {
                                    obj.set_cover(&path);
                                }
                            }
                        ));
                    }
                ))
                .build();
            let action_clear_album_art = ActionEntry::builder("clear-album-art")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        if let (Some(album), Some(library)) = (
                            obj.imp().album.borrow().as_ref(),
                            obj.imp().library.upgrade(),
                        ) {
                            library.clear_cover(album.get_folder_uri());
                        }
                    }
                ))
                .build();

            let action_refetch_metadata = ActionEntry::builder("refetch-metadata")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        if let (Some(album), Some(library)) = (
                            obj.imp().album.borrow().as_ref(),
                            obj.imp().library.upgrade(),
                        ) {
                            let spinner = obj.imp().infobox_spinner.get();
                            if spinner.visible_child_name().unwrap() != "spinner" {
                                spinner.set_visible_child_name("spinner");
                            }
                            spinner.set_visible(true);
                            library.clear_cover(album.get_folder_uri());
                            library.refetch_album_metadata(album);
                        }
                    }
                ))
                .build();

            let action_insert_queue = ActionEntry::builder("insert-queue")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        if let (_, Some(library)) =
                            (obj.imp().album.borrow().as_ref(), obj.get_library())
                        {
                            let store = &obj.imp().song_list;
                            if obj.imp().selecting_all.get() {
                                let mut songs: Vec<Song> =
                                    Vec::with_capacity(store.n_items() as usize);
                                for i in 0..store.n_items() {
                                    songs.push(store.item(i).and_downcast::<Song>().unwrap());
                                }
                                library.insert_songs_next(&songs);
                            } else {
                                // Get list of selected songs
                                let sel = &obj.imp().sel_model.selection();
                                let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                                let (iter, first_idx) = BitsetIter::init_first(sel).unwrap();
                                songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                                iter.for_each(|idx| {
                                    songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                                });
                                library.insert_songs_next(&songs);
                            }
                        }
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([
                action_clear_rating,
                action_set_album_art,
                action_refetch_metadata,
                action_clear_album_art,
                action_insert_queue,
            ]);
            self.obj()
                .insert_action_group("album-content-view", Some(&actions));
        }
    }

    impl WidgetImpl for AlbumContentView {}

    impl AlbumContentView {
        pub fn on_selection_changed(&self) {
            let sel_model = &self.sel_model;
            // TODO: this can be slow, might consider redesigning
            let n_sel = sel_model.selection().size();
            if n_sel == 0 || (n_sel as u32) == sel_model.model().unwrap().n_items() {
                self.selecting_all.replace(true);
                self.replace_queue_text.set_label("Play all");
                self.queue_split_button_content.set_label("Queue all");
                let queue_split_menu = Menu::new();
                queue_split_menu.append(
                    Some("Queue all next"),
                    Some("album-content-view.insert-queue"),
                );
                self.queue_split_button
                    .set_menu_model(Some(&queue_split_menu));
            } else {
                // TODO: l10n
                self.selecting_all.replace(false);
                self.replace_queue_text
                    .set_label(format!("Play {n_sel}").as_str());
                self.queue_split_button_content
                    .set_label(format!("Queue {n_sel}").as_str());
                let queue_split_menu = Menu::new();
                queue_split_menu.append(
                    Some(format!("Queue {n_sel} next").as_str()),
                    Some("album-content-view.insert-queue"),
                );
                self.queue_split_button
                    .set_menu_model(Some(&queue_split_menu));
            }
        }
    }
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
    fn get_library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn update_meta(&self, album: &Album) {
        let stack = self.imp().infobox_spinner.get();
        // If the current album is the "untitled" one (i.e. for songs without an album tag),
        // don't attempt to update metadata.
        if album.get_title().is_empty() {
            stack.set_visible(false);
        } else {
            let cache = self.imp().cache.get().unwrap().clone();
            let wiki_text = self.imp().wiki_text.get();
            let wiki_link = self.imp().wiki_link.get();
            let wiki_attrib = self.imp().wiki_attrib.get();
            if let Some(meta) = cache.load_cached_album_meta(album.get_info()) {
                if let Some(wiki) = meta.wiki {
                    stack.set_visible(true);
                    wiki_text.set_visible(true);
                    wiki_text.set_label(&wiki.content);
                    if let Some(url) = wiki.url.as_ref() {
                        wiki_link.set_visible(true);
                        wiki_link.set_uri(url);
                    } else {
                        wiki_link.set_visible(false);
                        wiki_link.set_uri("");
                    }
                    wiki_attrib.set_visible(true);
                    wiki_attrib.set_label(&wiki.attribution);
                    if stack.visible_child_name().unwrap() != "content" {
                        stack.set_visible_child_name("content");
                    }
                } else {
                    stack.set_visible(false);
                    wiki_text.set_visible(false);
                    wiki_text.set_label("");
                    wiki_attrib.set_visible(false);
                    wiki_attrib.set_label("");
                    wiki_link.set_visible(false);
                    wiki_link.set_uri("");
                }
            }
        }
    }

    /// Set a user-selected path as the new local cover.
    pub fn set_cover(&self, path: &str) {
        if let (Some(album), Some(library)) = (
            self.imp().album.borrow().as_ref(),
            self.imp().library.upgrade(),
        ) {
            library.set_cover(album.get_folder_uri(), path);
        }
    }

    pub fn setup(
        &self,
        library: &Library,
        client_state: &ClientState,
        cache: Rc<Cache>,
        window: &EuphonicaWindow,
    ) {
        let cache_state = cache.get_cache_state();
        self.imp()
            .cache
            .set(cache)
            .expect("AlbumContentView cannot bind to cache");
        self.imp().window.set(Some(window));
        self.imp()
            .add_to_playlist
            .setup(library, &self.imp().sel_model);
        self.imp().library.set(Some(library));
        cache_state.connect_closure(
            "album-art-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, uri: String, thumb: bool, tex: gdk::Texture| {
                    if thumb {
                        return;
                    }
                    if let Some(album) = this.imp().album.borrow().as_ref() {
                        if album.get_folder_uri() == uri {
                            // Force update since we might have been using an embedded cover
                            // temporarily
                            this.update_cover(tex, CoverSource::Folder);
                        } else if this.imp().cover_source.get() != CoverSource::Folder
                            && album.get_example_uri() == uri
                        {
                            this.update_cover(tex, CoverSource::Embedded);
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
                                if album.get_folder_uri() == uri {
                                    this.clear_cover();
                                }
                            }
                            CoverSource::Embedded => {
                                if album.get_example_uri() == uri {
                                    this.clear_cover();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            ),
        );

        let rating = self.imp().rating.get();
        client_state
            .bind_property("stickers-support-level", &rating, "visible")
            .transform_to(|_, lvl: StickersSupportLevel| {
                Some((lvl == StickersSupportLevel::All).to_value())
            })
            .sync_create()
            .build();

        rating.connect_closure(
            "changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                #[upgrade_or]
                (),
                move |rating: Rating| {
                    if let (Some(album), Some(library)) =
                        (this.imp().album.borrow().as_ref(), this.get_library())
                    {
                        let rating_val = rating.value();
                        let rating_opt = if rating_val > 0 {
                            Some(rating_val)
                        } else {
                            None
                        };
                        album.set_rating(rating_opt);
                        library.rate_album(album, rating_opt);
                    }
                }
            ),
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

        let replace_queue_btn = self.imp().replace_queue.get();
        client_state
            .bind_property("is-queuing", &replace_queue_btn, "sensitive")
            .invert_boolean()
            .sync_create()
            .build();
        replace_queue_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            (),
            move |_| {
                if let (Some(album), Some(library)) =
                    (this.imp().album.borrow().as_ref(), this.get_library())
                {
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
        let append_queue_btn = self.imp().queue_split_button.get();
        client_state
            .bind_property("is-queuing", &append_queue_btn, "sensitive")
            .invert_boolean()
            .sync_create()
            .build();
        append_queue_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            (),
            move |_| {
                if let (Some(album), Some(library)) =
                    (this.imp().album.borrow().as_ref(), this.get_library())
                {
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

        // Set up factory
        let factory = SignalListItemFactory::new();

        // For now don't show album arts as most of the time songs in the same
        // album will have the same embedded art anyway.
        factory.connect_setup(clone!(
            #[weak]
            library,
            #[upgrade_or]
            (),
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(None, None);
                row.set_index_visible(true);
                row.set_thumbnail_visible(false);
                item.property_expression("item")
                    .chain_property::<Song>("track")
                    .bind(&row, "index", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("name")
                    .bind(&row, "name", gtk::Widget::NONE);

                row.set_first_attrib_icon_name(Some("music-artist-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("artist")
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
        // Tell factory how to bind `AlbumSongRow` to one of our Album GObjects
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
                .expect("The child has to be an `SongRow`.");

            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(Some(&item));
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `AlbumSongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be an `SongRow`.");
            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(None);
        });

        // Set the factory of the list view
        self.imp().content.set_factory(Some(&factory));

        // Setup click action
        self.imp().content.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            (),
            move |_, position| {
                if let (Some(album), Some(library)) =
                    (this.imp().album.borrow().as_ref(), this.get_library())
                {
                    library.queue_album(album.clone(), true, true, Some(position));
                }
            }
        ));
    }

    fn clear_cover(&self) {
        self.imp().cover_source.set(CoverSource::None);
        self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
    }

    fn schedule_cover(&self, info: &AlbumInfo) {
        self.imp().cover_source.set(CoverSource::Unknown);
        self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
        if let Some((tex, is_embedded)) = self
            .imp()
            .cache
            .get()
            .unwrap()
            .clone()
            .load_cached_folder_cover(info, false, true)
        {
            self.imp().cover.set_paintable(Some(&tex));
            self.imp().cover_source.set(if is_embedded {
                CoverSource::Embedded
            } else {
                CoverSource::Folder
            });
        }
    }

    fn update_cover(&self, tex: gdk::Texture, src: CoverSource) {
        self.imp().cover.set_paintable(Some(&tex));
        self.imp().cover_source.set(src);
    }

    pub fn bind(&self, album: Album) {
        self.imp().on_selection_changed();
        let title_label = self.imp().title.get();
        let artists_box = self.imp().artists_box.get();
        let rating = self.imp().rating.get();
        let release_date_label = self.imp().release_date.get();
        let mut bindings = self.imp().bindings.borrow_mut();

        let title_binding = album
            .bind_property("title", &title_label, "label")
            .transform_to(|_, s: Option<&str>| {
                Some(if s.is_none_or(|s| s.is_empty()) {
                    (*EMPTY_ALBUM_STRING).to_value()
                } else {
                    s.to_value()
                })
            })
            .sync_create()
            .build();
        // Save binding
        bindings.push(title_binding);

        // Populate artist tags
        let artist_tags = album
            .get_artists()
            .iter()
            .map(|info| {
                ArtistTag::new(
                    Artist::from(info.clone()),
                    self.imp().cache.get().unwrap().clone(),
                    &self.imp().window.upgrade().unwrap(),
                )
            })
            .collect::<Vec<ArtistTag>>();
        self.imp().artist_tags.extend_from_slice(&artist_tags);
        for tag in artist_tags {
            artists_box.append(&tag);
        }

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
        self.schedule_cover(info);
        self.imp().album.borrow_mut().replace(album);
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }

        // Clear artists wrapbox. TODO: when adw 1.8 drops as stable please use remove_all() instead.
        for tag in self.imp().artist_tags.iter::<gtk::Widget>() {
            self.imp().artists_box.remove(&tag.unwrap());
        }
        self.imp().artist_tags.remove_all();

        if let Some(id) = self.imp().cover_signal_id.take() {
            if let Some(cache) = self.imp().cache.get() {
                cache.get_cache_state().disconnect(id);
            }
        }
        if let Some(_) = self.imp().album.take() {
            self.clear_cover();
        }

        // Unset metadata widgets
        self.imp().song_list.remove_all();
        let content_spinner = self.imp().content_spinner.get();
        if content_spinner.visible_child_name().unwrap() != "spinner" {
            content_spinner.set_visible_child_name("spinner");
        }
        let infobox_spinner = self.imp().infobox_spinner.get();
        if infobox_spinner.visible_child_name().unwrap() != "spinner" {
            infobox_spinner.set_visible_child_name("spinner");
        }
        infobox_spinner.set_visible(true);
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
