use super::{TagsSection, Library, artist_tag_button::ArtistTagButton, Tag};
use crate::common::FadingScrolledWindow;
use crate::meta_providers::models::Wiki;
use crate::meta_providers::models::AlbumMeta;
use crate::{
    cache::{Cache, CacheState, Error as CacheError, placeholders::EMPTY_ALBUM_STRING},
    client::{ClientState, state::StickersSupportLevel},
    common::{Album, Artist, ContentStack, ImageStack, Rating, RowAddButtons, Song, SongRow},
    library::{add_to_playlist::AddToPlaylistButton, tag_button::TagButton},
    utils::{format_secs_as_duration, tokio_runtime},
    window::EuphonicaWindow,
};
use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use derivative::Derivative;
use gio::{ActionEntry, Menu, SimpleActionGroup};
use glib::{Binding, WeakRef, clone, closure_local, signal::SignalHandlerId};
use gtk::{
    BitsetIter, CompositeTemplate, gdk, gio, glib, prelude::*,
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
        pub cover: TemplateChild<ImageStack>,

        #[template_child]
        pub wiki_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub artists_box: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub genres_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub genres_box: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub rating: TemplateChild<Rating>,
        #[template_child]
        pub rating_readout: TemplateChild<gtk::Label>,

        #[template_child]
        pub add_wiki_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub wiki_fader: TemplateChild<FadingScrolledWindow>,
        #[template_child]
        pub wiki_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub wiki_link: TemplateChild<gtk::LinkButton>,
        #[template_child]
        pub wiki_attrib: TemplateChild<gtk::Label>,
        #[template_child]
        pub wiki_desc_field: TemplateChild<gtk::TextView>,
        #[template_child]
        pub wiki_link_field: TemplateChild<gtk::Entry>,
        #[template_child]
        pub wiki_attribution_field: TemplateChild<gtk::Entry>,
        #[template_child]
        pub wiki_save: TemplateChild<gtk::Button>,
        #[template_child]
        pub wiki_cancel: TemplateChild<gtk::Button>,

        #[template_child]
        pub release_date: TemplateChild<gtk::Label>,
        #[template_child]
        pub track_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub runtime: TemplateChild<gtk::Label>,

        #[template_child]
        pub tags_widget: TemplateChild<TagsSection>,

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
        #[derivative(Default(value = "gtk::MultiSelection::new(Option::<gio::ListStore>::None)"))]
        pub sel_model: gtk::MultiSelection,
        pub library: WeakRef<Library>,
        pub album: RefCell<Option<Album>>,
        pub window: WeakRef<EuphonicaWindow>,
        pub bindings: RefCell<Vec<Binding>>,
        pub cover_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cover_set_id: RefCell<Option<SignalHandlerId>>,
        pub cover_cleared_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        #[derivative(Default(value = "Cell::new(true)"))]
        pub selecting_all: Cell<bool>, // Enables queuing the entire album efficiently
        pub meta: RefCell<Option<AlbumMeta>>,
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
            if let Some(cache) = self.cache.get() {
                let state = cache.get_cache_state();
                if let Some(id) = self.cover_set_id.take() {
                    state.disconnect(id);
                }
                if let Some(id) = self.cover_cleared_id.take() {
                    state.disconnect(id);
                }
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.sel_model.set_model(Some(&self.song_list.clone()));
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

            self.song_list
                .bind_property("n-items", &self.track_count.get(), "label")
                .sync_create()
                .build();

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

            self.wiki_save.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let (Some(cache), Some(album)) =
                        (this.cache.get(), this.album.borrow().as_ref())
                    {
                        let mut new_meta = this.meta.take().unwrap_or_default();
                        let buf = this.wiki_desc_field.buffer();
                        let mut wiki = new_meta.wiki.clone().unwrap_or_default();
                        wiki.content = buf
                            .text(&buf.start_iter(), &buf.end_iter(), false)
                            .as_str()
                            .to_owned();
                        let maybe_link = this.wiki_link_field.text();
                        if !maybe_link.is_empty() {
                            wiki.url = Some(maybe_link.as_str().to_owned());
                        } else {
                            wiki.url = None;
                        }
                        wiki.attribution = this.wiki_attribution_field.text().as_str().to_owned();
                        new_meta.wiki = Some(wiki);
                        // Might want to make this async?
                        if let Err(e) = cache.set_album_meta(album.get_info(), &new_meta) {
                            dbg!(e);
                        }
                        this.obj().update_wiki(new_meta.wiki.as_ref());
                    }
                }
            ));

            self.wiki_cancel.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.wiki_stack.show_content();
                }
            ));

            self.tags_widget.set_on_tag_added(clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.obj().write_tags();
                }
            ));
            self.tags_widget.set_on_tag_removed(clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.obj().write_tags();
                }
            ));
            self.tags_widget.set_on_add_btn_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.obj().write_tags();
                }
            ));

            // Edit actions
            let obj = self.obj();
            let wiki_stack = self.wiki_stack.get();
            let action_edit_wiki = ActionEntry::builder("edit-wiki")
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _, _| {
                        // Initialise with current values
                        if let Some(wiki) = this
                            .meta
                            .borrow()
                            .as_ref()
                            .and_then(|meta| meta.wiki.as_ref())
                        {
                            this.wiki_desc_field.buffer().set_text(&wiki.content);
                            this.wiki_link_field
                                .set_text(wiki.url.as_deref().unwrap_or(""));
                            this.wiki_attribution_field.set_text(&wiki.attribution);
                        }
                        wiki_stack.show_edit();
                    }
                ))
                .build();
            let action_clear_rating = ActionEntry::builder("clear-rating")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let (Some(album), Some(library)) = (
                                    obj.imp().album.borrow().as_ref(),
                                    obj.imp().library.upgrade(),
                                ) {
                                    if let Err(e) = library.rate_album(album, None).await {
                                        dbg!(e);
                                    } else {
                                        obj.imp().rating.set_value(-1);
                                    }
                                }
                            }
                        ));
                    }
                ))
                .build();
            let action_set_album_art = ActionEntry::builder("set-album-art")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        let (sender, receiver) = oneshot::channel();
                        tokio_runtime().spawn(async move {
                            let maybe_files = SelectedFiles::open_file()
                                .title("Select a new album art")
                                .modal(true)
                                .multiple(false)
                                .send()
                                .await
                                .expect("ashpd file open await failure")
                                .response();

                            sender
                                .send(if let Ok(files) = maybe_files {
                                    let uris = files.uris();
                                    if !uris.is_empty() {
                                        Some(uris[0].to_string())
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                })
                                .expect("Broken oneshot sender");
                        });
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let Some(path) = receiver.await.expect("Broken oneshot receiver")
                                {
                                    obj.set_cover(&path).await;
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
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let (Some(album), Some(cache)) =
                                    (obj.imp().album.borrow().as_ref(), obj.imp().cache.get())
                                    && let Err(e) = cache
                                        .clear_cover(album.get_folder_uri().to_owned(), true)
                                        .await
                                {
                                    obj.show_cache_error("Couldn't clear cover", e);
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
                                obj.update_meta(true).await;
                                obj.schedule_cover(true).await;
                            }
                        ));
                    }
                ))
                .build();

            let action_insert_queue = ActionEntry::builder("insert-queue")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let (_, Some(library)) =
                                    (obj.imp().album.borrow().as_ref(), obj.get_library())
                                {
                                    obj.set_is_queuing(true);
                                    let store = &obj.imp().song_list;
                                    if obj.imp().selecting_all.get() {
                                        let mut songs: Vec<Song> =
                                            Vec::with_capacity(store.n_items() as usize);
                                        for i in 0..store.n_items() {
                                            songs.push(
                                                store.item(i).and_downcast::<Song>().unwrap(),
                                            );
                                        }
                                        if let Err(e) = library.insert_songs_next(&songs).await {
                                            dbg!(e);
                                        }
                                    } else {
                                        // Get list of selected songs
                                        let sel = &obj.imp().sel_model.selection();
                                        let mut songs: Vec<Song> =
                                            Vec::with_capacity(sel.size() as usize);
                                        let (iter, first_idx) =
                                            BitsetIter::init_first(sel).unwrap();
                                        songs.push(
                                            store.item(first_idx).and_downcast::<Song>().unwrap(),
                                        );
                                        iter.for_each(|idx| {
                                            songs.push(
                                                store.item(idx).and_downcast::<Song>().unwrap(),
                                            )
                                        });
                                        if let Err(e) = library.insert_songs_next(&songs).await {
                                            dbg!(e);
                                        }
                                    }
                                    obj.set_is_queuing(false);
                                }
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([
                action_edit_wiki,
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
    fn show_cache_error(&self, prefix: &str, err: CacheError) {
        if let Some(win) = self.imp().window.upgrade() {
            win.send_simple_toast(&format!("{}: {}", prefix, dbg!(err).message()), 3);
        }
    }

    fn get_library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn album(&self) -> Option<Album> {
        self.imp().album.borrow().as_ref().cloned()
    }

    /// Write the current tag list to the database.
    pub fn write_tags(&self) {
        if let (Some(cache), Some(album)) =
            (self.imp().cache.get(), self.imp().album.borrow().as_ref())
        {
            let tags = self.imp().tags_widget.get_tags();
            let folder_uri = album.get_info().folder_uri.clone();
            if let Err(e) = cache.set_album_tags(&folder_uri, &tags) {
                dbg!(e);
            }
        }
    }

    pub fn update_wiki(&self, wiki: Option<&Wiki>) {
        if let Some(wiki) = wiki {
            let wiki_text = self.imp().wiki_text.get();
            let wiki_link = self.imp().wiki_link.get();
            let wiki_attrib = self.imp().wiki_attrib.get();
            self.imp().wiki_stack.show_content();
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
            self.imp().wiki_stack.show_content();
        } else {
            self.imp().wiki_stack.show_placeholder();
        }
    }

    async fn update_meta(&self, overwrite: bool) {
        if let Some(album) = self.album() {
            // If the current album is the "untitled" one (i.e. for songs without an album tag),
            // don't attempt to update metadata.
            if album.get_title().is_empty() {
                self.imp().wiki_stack.show_placeholder();
                self.imp().tags_widget.remove_all(false);
            } else {
                self.imp().wiki_stack.show_spinner();
                self.imp().tags_widget.remove_all(true);
                let cache = self.imp().cache.get().unwrap().clone();
                let folder_uri = album.get_info().folder_uri.clone();

                let res = cache
                    .get_album_meta(
                        album.get_info(),
                        true,
                        overwrite,
                        self.imp().window.upgrade().as_ref(),
                    )
                    .await;
                match res {
                    Ok(Some(meta)) => {
                        let _ = self.imp().meta.replace(Some(meta.clone()));
                        // Handle wiki
                        self.update_wiki(meta.wiki.as_ref());

                        // Load tags from DB
                        let tags = cache.get_album_tags(&folder_uri);
                        if let Ok(tags) = tags {
                            if tags.is_empty() {
                                self.imp().tags_widget.show_placeholder();
                            } else {
                                for tag in tags {
                                    self.imp().tags_widget.add_tag(
                                        &Tag::new(
                                            tag.name,
                                            tag.url,
                                            tag.count,
                                            true,
                                            tag.set_by_user
                                        )
                                    );
                                }
                            }
                        } else {
                            self.imp().tags_widget.show_placeholder();
                        }
                    }
                    Ok(None) => {
                        self.imp().wiki_stack.show_placeholder();
                        self.imp().tags_widget.show_placeholder();
                    }
                    Err(e) => {
                        self.imp().wiki_stack.show_placeholder();
                        self.imp().tags_widget.show_placeholder();
                        dbg!(e);
                    }
                }
            }
        }
    }

    /// Set a user-selected path as the new local cover.
    pub async fn set_cover(&self, path: &str) {
        if let (Some(album), Some(cache)) = (self.album(), self.imp().cache.get())
            && let Err(e) = cache
                .set_cover(album.get_folder_uri().to_owned(), path, true)
                .await
        {
            self.show_cache_error("Couldn't set cover", e);
        }
    }

    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().queue_split_button.set_sensitive(!queuing);
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
        self.imp().tags_widget.set_window(window);
        self.imp().window.set(Some(window));
        self.imp()
            .add_to_playlist
            .setup(library, &self.imp().sel_model);
        self.imp().library.set(Some(library));
        self.imp()
            .cover_set_id
            .replace(Some(cache_state.connect_closure(
                "folder-cover-set",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String, hires: gdk::Texture, _: gdk::Texture| {
                        if this.album().is_some_and(|a| a.get_folder_uri() == uri) {
                            this.update_cover(hires);
                        }
                    }
                ),
            )));
        self.imp()
            .cover_cleared_id
            .replace(Some(cache_state.connect_closure(
                "folder-cover-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String| {
                        if this.album().is_some_and(|a| a.get_folder_uri() == uri) {
                            this.clear_cover();
                        }
                    }
                ),
            )));

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
                move |rating: Rating| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        #[weak]
                        rating,
                        async move {
                            if let (Some(album), Some(library)) = (this.album(), this.get_library())
                            {
                                let rating_val = rating.value();
                                let rating_opt = if rating_val > 0 {
                                    Some(rating_val)
                                } else {
                                    None
                                };
                                album.set_rating(rating_opt);
                                if let Err(e) = library.rate_album(&album, rating_opt).await {
                                    dbg!(e);
                                }
                            }
                        }
                    ));
                }
            ),
        );

        self.imp().replace_queue.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let (Some(album), Some(library)) = (this.album(), this.get_library()) {
                            this.set_is_queuing(true);
                            if this.imp().selecting_all.get() {
                                if let Err(e) =
                                    library.queue_album(album.clone(), true, true, None).await
                                {
                                    dbg!(e);
                                }
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
                                if let Err(e) = library.queue_songs(&songs, true, true).await {
                                    dbg!(e);
                                }
                            }
                            this.set_is_queuing(false);
                        }
                    }
                ));
            }
        ));

        self.imp().queue_split_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[upgrade_or]
            (),
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let (Some(album), Some(library)) = (this.album(), this.get_library()) {
                            this.set_is_queuing(true);
                            if this.imp().selecting_all.get() {
                                if let Err(e) =
                                    library.queue_album(album.clone(), false, false, None).await
                                {
                                    dbg!(e);
                                }
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
                                if let Err(e) = library.queue_songs(&songs, false, false).await {
                                    dbg!(e);
                                }
                            }
                            this.set_is_queuing(false);
                        }
                    }
                ));
            }
        ));

        // Set up ListBox
        self.imp().content.bind_model(
            Some(&self.imp().sel_model),
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

                    row.set_name(&song.get_name());
                    row.set_first_attrib_icon_name(Some("music-artist-symbolic"));
                    row.set_first_attrib_text(song.get_artist_tag().as_deref());

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

        // Setup click action
        self.imp().content.connect_row_activated(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, row| {
                let idx = row.index() as u32;
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let (Some(album), Some(library)) = (this.album(), this.get_library())
                            && let Err(e) = library
                                .queue_album(album.clone(), true, true, Some(idx))
                                .await
                        {
                            dbg!(e);
                        }
                    }
                ));
            }
        ));
    }

    #[inline]
    fn clear_cover(&self) {
        self.imp().cover.clear();
    }

    #[inline]
    fn update_cover(&self, tex: gdk::Texture) {
        self.imp().cover.show(&tex);
    }

    async fn schedule_cover(&self, overwrite: bool) {
        self.imp().cover.show_spinner();
        if let Some(info) = self.album().as_ref().map(|a| a.get_info()) {
            let cache = self.imp().cache.get().unwrap().clone();
            // Remove existing entry in SQLite, which might be an empty "do not retry" placeholder.
            if overwrite {
                // Don't notify, else we'd interrupt the spinner
                if let Err(e) = cache.clear_cover(info.folder_uri.to_owned(), false).await {
                    self.show_cache_error("Couldn't clear cover", e);
                }
            }
            match cache.get_album_cover(info, false).await {
                Ok(Some(tex)) => {
                    self.update_cover(tex);
                }
                Ok(None) => {
                    self.clear_cover();
                }
                Err(e) => {
                    self.show_cache_error("Couldn't fetch cover", e);
                    self.clear_cover();
                }
            }
        }
    }

    pub fn bind(&self, album: &Album) {
        self.imp().on_selection_changed();
        let title_label = self.imp().title.get();
        let artists_box = self.imp().artists_box.get();
        let genres_box = self.imp().genres_box.get();
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

        let genres = album.get_genres();
        if !genres.is_empty() {
            let genres_stack = self.imp().genres_stack.get();
            let window = self.imp().window.upgrade().unwrap();
            album
                .get_genres()
                .iter()
                .map(|genre| TagButton::new(
                    &Tag::new(genre.clone(), None, None, false, false),
                    &genres_box, &window, |_| {}))
                .for_each(|tag| genres_box.append(&tag));

            if genres_stack
                .visible_child_name()
                .is_some_and(|name| name == "empty")
            {
                genres_stack.set_visible_child_name("content");
            }
        }

        let rating_binding = album
            .bind_property("rating", &rating, "value")
            .sync_create()
            .build();
        // Save binding
        bindings.push(rating_binding);

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

        self.imp().album.borrow_mut().replace(album.clone());
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            album,
            async move {
                let library = this.imp().library.upgrade().unwrap();
                // Important, MPD-side content first
                let stack = this.imp().content_stack.get();
                stack.show_spinner();
                let song_list = this.imp().song_list.clone();
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
                            // Only now can we populate the artist tags, as the initial albuminfo
                            // is stripped of artist MBID for album grid performance
                            song_list
                                .item(0)
                                .unwrap()
                                .downcast_ref::<Song>()
                                .unwrap()
                                .get_artists()
                                .iter()
                                .map(|info| {
                                    ArtistTagButton::new(
                                        &Artist::from(info.clone()),
                                        this.imp().cache.get().unwrap().clone(),
                                        &this.imp().window.upgrade().unwrap(),
                                    )
                                })
                                .for_each(|tag| artists_box.append(&tag));
                        } else {
                            stack.show_placeholder();
                        }
                    }
                    Err(e) => {
                        dbg!(e);
                    }
                };
                this.imp().runtime.set_label(&format_secs_as_duration(
                    song_list
                        .iter()
                        .map(|item: Result<Song, _>| {
                            if let Ok(song) = item {
                                return song.get_duration();
                            }
                            0
                        })
                        .sum::<u64>() as f64,
                ));
                // The extra fluff later
                this.schedule_cover(false).await;
                this.update_meta(false).await;
            }
        ));
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.take().into_iter() {
            binding.unbind();
        }

        // We're now on libadwaita 1.8 so we can use this
        self.imp().artists_box.remove_all();
        self.imp().genres_box.remove_all();
        self.imp().tags_widget.remove_all(true);
        let genres_stack = self.imp().genres_stack.get();
        if genres_stack
            .visible_child_name()
            .is_some_and(|name| name == "content")
        {
            genres_stack.set_visible_child_name("empty");
        }

        if let Some(id) = self.imp().cover_signal_id.take()
            && let Some(cache) = self.imp().cache.get()
        {
            cache.get_cache_state().disconnect(id);
        }
        if let Some(_) = self.imp().album.take() {
            self.clear_cover();
        }

        // Unset metadata widgets
        self.imp().song_list.remove_all();
        self.imp().content_stack.show_placeholder();
        self.imp().wiki_stack.show_spinner();
    }
}
