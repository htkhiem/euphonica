use super::{Library, TagsFilter};
use crate::{
    cache::Cache,
    common::{ContentStack, RowAddButtons, Song, SongRow},
    utils::{LazyInit, SearchableView, format_secs_as_duration, g_search_substr, settings_manager},
    window::EuphonicaWindow,
};
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, gio,
    glib::{self, Properties, SignalHandlerId, WeakRef, clone, closure_local, subclass::Signal},
};
use std::cell::RefCell;
use std::{cell::Cell, cmp::Ordering, rc::Rc, sync::OnceLock};
use strum::{EnumMessage, VariantArray};

mod imp {
    use derivative::Derivative;

    use crate::{
        common::Song,
        library::{
            add_to_playlist::AddToPlaylistButton,
            song_sort::{SONG_SORT_SETTINGS, SongSortCriterion, compare_songs, song_sort_tiers},
        },
    };

    use super::*;

    #[derive(Debug, CompositeTemplate, Properties, Derivative)]
    #[derivative(Default)]
    #[properties(wrapper_type = super::SongView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/song-view.ui")]
    pub struct SongView {
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,

        // Search & filter widgets
        #[template_child]
        pub view_options_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub sort_by: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub sort_by_2: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub sort_by_3: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub descending: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub descending_2: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub descending_3: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_mode: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub genres_filter_widget: TemplateChild<TagsFilter>,
        // TODO: maybe implement song tags & tag filtering??
        // Currently tags are associated with albums and artists only.
        // To search in both might be too slow.

        // Action bar
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

        // Content
        #[template_child]
        pub stack: TemplateChild<ContentStack>,
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,

        // Search & filter models
        pub search_filter: gtk::CustomFilter,
        pub genres_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: WeakRef<Library>,
        #[derivative(Default(value = "gtk::MultiSelection::new(Option::<gio::ListStore>::None)"))]
        pub sel_model: gtk::MultiSelection,
        pub sort_handlers: RefCell<Option<(gio::Settings, Vec<SignalHandlerId>)>>,
        pub is_queuing: Cell<bool>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,

        pub initializing: Cell<bool>,
        pub window: WeakRef<EuphonicaWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for SongView {
        const NAME: &'static str = "EuphonicaSongView";
        type Type = super::SongView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for SongView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            if let Some((state, ids)) = self.sort_handlers.take() {
                for id in ids {
                    state.disconnect(id);
                }
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.stack.show_placeholder();

            self.obj()
                .bind_property("collapsed", &self.show_sidebar.get(), "visible")
                .sync_create()
                .build();

            self.show_sidebar.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().emit_by_name::<()>("show-sidebar-clicked", &[]);
                }
            ));

            let settings = settings_manager();
            let state = settings.child("state").child("songview");
            let library_settings = settings.child("library");

            // Setup sorting
            bind_sort_direction_icon(&state, "sort-direction", &self.view_options_btn.get());
            let sort_by_model: gtk::StringList = SongSortCriterion::VARIANTS
                .iter()
                .map(ToString::to_string)
                .collect();
            let tier_widgets = [
                (self.sort_by.get(), self.descending.get()),
                (self.sort_by_2.get(), self.descending_2.get()),
                (self.sort_by_3.get(), self.descending_3.get()),
            ];
            for (setting_keys, (sort_by, descending)) in
                SONG_SORT_SETTINGS.into_iter().zip(tier_widgets)
            {
                sort_by.set_model(Some(&sort_by_model));
                constructed_bind_sort_tier(&state, setting_keys, &sort_by, &descending);
            }

            self.sorter.set_sort_func(clone!(
                #[strong]
                library_settings,
                #[strong]
                state,
                move |obj1, obj2| {
                    let song1 = obj1
                        .downcast_ref::<Song>()
                        .expect("Sort obj has to be a common::Song.");

                    let song2 = obj2
                        .downcast_ref::<Song>()
                        .expect("Sort obj has to be a common::Song.");

                    // Should the sorting be case-sensitive, i.e. uppercase goes first?
                    let case_sensitive = library_settings.boolean("sort-case-sensitive");
                    // Should nulls be put first or last?
                    let nulls_first = library_settings.boolean("sort-nulls-first");

                    let tiers = song_sort_tiers(&state);
                    compare_songs(song1, song2, &tiers, nulls_first, case_sensitive)
                }
            ));

            // Update when changing sort settings.
            // Not Inverted on a direction flip: the other tiers keep their own direction.
            let sort_handler_ids = SONG_SORT_SETTINGS
                .iter()
                .flat_map(|(sort_by, sort_direction)| [sort_by, sort_direction])
                .map(|name| {
                    state.connect_changed(
                        Some(name),
                        clone!(
                            #[weak(rename_to = this)]
                            self,
                            move |_, _| {
                                this.sorter.changed(gtk::SorterChange::Different);
                            }
                        ),
                    )
                })
                .collect();
            self.sort_handlers
                .replace(Some((state.clone(), sort_handler_ids)));

            // Set up search
            self.search_filter.set_filter_func(clone!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                library_settings,
                #[upgrade_or]
                true,
                move |obj| {
                    let song = obj
                        .downcast_ref::<Song>()
                        .expect("Search obj has to be a common::Song.");

                    let search_term = this.search_entry.text();
                    if search_term.is_empty() {
                        return true;
                    }

                    // Should the searching be case-sensitive?
                    let case_sensitive = library_settings.boolean("search-case-sensitive");
                    // Vary behaviour depending on dropdown
                    match this.search_mode.selected() {
                        // Keep these indices in sync with the GtkStringList in the UI file
                        0 => {
                            // Match either song title or artist tag (not albumartist tag)
                            g_search_substr(Some(song.get_name()), &search_term, case_sensitive)
                                || g_search_substr(
                                    Some(song.get_artist_str()),
                                    &search_term,
                                    case_sensitive,
                                )
                        }
                        1 => {
                            // Song title only
                            g_search_substr(Some(song.get_name()), &search_term, case_sensitive)
                        }
                        2 => {
                            // Artist tag only (songs without such tag will never match)
                            g_search_substr(
                                Some(song.get_artist_str()),
                                &search_term,
                                case_sensitive,
                            )
                        }
                        3 => {
                            // Match either album title or AlbumArtist (not artist tag)
                            g_search_substr(song.get_album_title(), &search_term, case_sensitive)
                                || g_search_substr(
                                    Some(song.get_albumartist_str()),
                                    &search_term,
                                    case_sensitive,
                                )
                        }
                        4 => {
                            // Match only album title
                            g_search_substr(song.get_album_title(), &search_term, case_sensitive)
                        }
                        5 => {
                            // Match only AlbumArtist (songs without such tag will never match)
                            g_search_substr(
                                Some(song.get_albumartist_str()),
                                &search_term,
                                case_sensitive,
                            )
                        }
                        _ => true,
                    }
                }
            ));

            self.search_mode.connect_notify_local(
                Some("selected"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| {
                        this.search_filter.changed(gtk::FilterChange::Different);
                    }
                ),
            );

            // Tags and genres filter are set on changes only
            self.genres_filter.set_filter_func(|_| true);

            // Connect search entry to filter. Filter will later be put in GtkSearchModel.
            // That GtkSearchModel will listen to the filter's changed signal.
            let search_entry = self.search_entry.get();
            search_entry.connect_search_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |entry| {
                    let text = entry.text();
                    let new_len = text.len();
                    let old_len = this.last_search_len.replace(new_len);
                    match new_len.cmp(&old_len) {
                        Ordering::Greater => {
                            this.search_filter.changed(gtk::FilterChange::MoreStrict);
                        }
                        Ordering::Less => {
                            this.search_filter.changed(gtk::FilterChange::LessStrict);
                        }
                        Ordering::Equal => {
                            this.search_filter.changed(gtk::FilterChange::Different);
                        }
                    }
                }
            ));

            // Change button labels depending on selection state
            self.sel_model.connect_selection_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, _| this.on_song_selection_changed()
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    fn bind_sort_direction_icon(
        state: &gio::Settings,
        sort_direction_key: &str,
        widget: &impl IsA<glib::Object>,
    ) {
        state
            .bind(sort_direction_key, widget, "icon-name")
            .get_only()
            .mapping(|dir, _| match dir.get::<String>().unwrap().as_ref() {
                "asc" => Some("view-sort-ascending-symbolic".to_value()),
                _ => Some("view-sort-descending-symbolic".to_value()),
            })
            .build();
    }

    fn constructed_bind_sort_tier(
        state: &gio::Settings,
        (sort_by_key, sort_direction_key): (&str, &str),
        sort_by: &gtk::DropDown,
        descending: &gtk::ToggleButton,
    ) {
        // Failure to get mapping would make GSettings show the key's default instead,
        // so a value without a dropdown item (e.g. `filename`) clears the selection.
        state
            .bind(sort_by_key, sort_by, "selected")
            .mapping(|nick, _| {
                let idx = nick
                    .get::<String>()?
                    .parse::<SongSortCriterion>()
                    .map_or(gtk::INVALID_LIST_POSITION, |criterion| criterion as u32);
                Some(idx.to_value())
            })
            .set_mapping(|idx, _| {
                let criterion = SongSortCriterion::VARIANTS.get(idx.get::<u32>().ok()? as usize)?;
                Some(criterion.get_serializations()[0].to_variant())
            })
            .build();
        bind_sort_direction_icon(state, sort_direction_key, descending);
        state
            .bind(sort_direction_key, descending, "active")
            .mapping(|dir, _| Some((dir.get::<String>()? == "desc").to_value()))
            .set_mapping(|descending, _| {
                let dir = if descending.get::<bool>().ok()? {
                    "desc"
                } else {
                    "asc"
                };
                Some(dir.to_variant())
            })
            .build();
    }

    impl WidgetImpl for SongView {}

    impl SongView {
        pub fn on_song_selection_changed(&self) {
            let sel_model = &self.sel_model;
            // TODO: self can be slow, might consider redesigning
            let n_sel = sel_model.selection().size();
            if n_sel == 0 || (n_sel as u32) == sel_model.n_items() {
                self.replace_queue_text.set_label("Play all");
                self.append_queue_text.set_label("Queue all");
            } else {
                // TODO: l10n
                self.replace_queue_text
                    .set_label(format!("Play {n_sel}").as_str());
                self.append_queue_text
                    .set_label(format!("Queue {n_sel}").as_str());
            }
        }
    }
}

glib::wrapper! {
    pub struct SongView(ObjectSubclass<imp::SongView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for SongView {
    fn default() -> Self {
        Self::new()
    }
}

impl SongView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn setup(&self, library: &Library, cache: Rc<Cache>, window: &EuphonicaWindow) {
        self.imp().library.set(Some(library));
        self.imp().window.set(Some(window));
        self.setup_listview(library, cache.clone(), window);

        // Set up genres filter
        self.imp().genres_filter_widget.setup(
            &library.genres(),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |genres| {
                    if !genres.is_empty() {
                        this.imp().genres_filter.set_filter_func(move |obj| {
                            let song = obj.downcast_ref::<Song>().unwrap();
                            song.get_genres().any(|genre| genres.contains(genre))
                        });
                        this.imp()
                            .genres_filter
                            .changed(gtk::FilterChange::MoreStrict);
                    } else {
                        this.imp().genres_filter.set_filter_func(|_| true);
                        this.imp()
                            .genres_filter
                            .changed(gtk::FilterChange::LessStrict);
                    }
                }
            ),
            window,
        );
    }

    pub fn search_bar(&self) -> gtk::SearchBar {
        self.imp().search_bar.get()
    }

    #[inline]
    fn set_is_queuing(&self, queuing: bool) {
        self.imp().is_queuing.set(queuing);
    }

    async fn queue_songs(&self, replace: bool, play_from: Option<u32>) {
        if let Some(library) = self.imp().library.upgrade() {
            let sel_model = &self.imp().sel_model;
            let sel = sel_model.selection();
            let n_selected = sel.size();
            if n_selected == 0 || n_selected == sel_model.n_items() as u64 {
                // Fast path for select-all (cowabunga it is)
                // NOTE: SELECT FROM THE SEL MODEL INSTEAD OF THE STORE SINCE
                // THE STORE HAS EVERYTHING, NOT JUST WHAT PASSED THE CURRENT FILTER.
                if let Err(e) = library
                    .queue_songs(
                        &sel_model
                            .iter::<glib::Object>()
                            .filter(|r| r.is_ok())
                            .map(|r| r.unwrap().downcast::<Song>().unwrap())
                            .collect::<Vec<Song>>(),
                        replace,
                        play_from,
                    )
                    .await
                {
                    dbg!(e);
                }
            } else {
                let mut songs = Vec::with_capacity(sel.size() as usize);
                // Need to go thru bitset iter. Guaranteed to have at least one song selected
                // since the zero-selected case goes into the above already.
                let (iter, first_idx) = gtk::BitsetIter::init_first(&sel).unwrap();
                songs.push(sel_model.item(first_idx).and_downcast::<Song>().unwrap());
                iter.for_each(|idx| {
                    songs.push(sel_model.item(idx).and_downcast::<Song>().unwrap())
                });
                if let Err(e) = library.queue_songs(&songs, replace, play_from).await {
                    dbg!(e);
                }
            }
        }
    }

    fn setup_listview(&self, library: &Library, cache: Rc<Cache>, window: &EuphonicaWindow) {
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
                        this.set_is_queuing(true);
                        this.queue_songs(true, Some(0)).await;
                        this.set_is_queuing(false);
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
                        this.set_is_queuing(true);
                        this.queue_songs(false, None).await;
                        this.set_is_queuing(false);
                    }
                ));
            }
        ));

        // Setup double-click action
        self.imp().list_view.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, pos| {
                // Song View might display hundreds of thousands of songs.
                // Should anyone want to "queue everything" there's the action bar at the top.
                // Double-click action should then only do something non-destructive and lightweight.
                // For now that's fixed to Append And Play.
                let song = this
                    .imp()
                    .sel_model
                    .item(pos)
                    .and_downcast::<Song>()
                    .expect("The item has to be a `common::Song`.");
                let play_from = Some(this.imp().library.upgrade().unwrap().queue_len());
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        this.set_is_queuing(true);
                        let library = this.imp().library.upgrade().unwrap();
                        if let Err(e) = library.queue_songs(&[song], false, play_from).await {
                            dbg!(e);
                        }
                        this.set_is_queuing(false);
                    }
                ));
            }
        ));

        // Setup search bar
        let song_list = self.imp().library.upgrade().unwrap().songs();
        let search_bar = self.imp().search_bar.get();
        let search_entry = self.imp().search_entry.get();
        search_bar.connect_entry(&search_entry);

        let search_btn = self.imp().search_btn.get();
        search_btn
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        // Chain order: substring search -> genres -> tags
        let search_model = gtk::FilterListModel::new(
            Some(song_list.clone()),
            Some(self.imp().search_filter.clone()),
        );
        search_model.set_incremental(true);
        let genres_model =
            gtk::FilterListModel::new(Some(search_model), Some(self.imp().genres_filter.clone()));
        genres_model.set_incremental(true);

        let sort_model =
            gtk::SortListModel::new(Some(genres_model), Some(self.imp().sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = &self.imp().sel_model;
        sel_model.set_model(Some(&sort_model));

        let list_view = self.imp().list_view.get();
        list_view.set_model(Some(sel_model));
        self.imp().add_to_playlist.bind_model(library, sel_model);

        self.imp().sel_all.connect_clicked(clone!(
            #[weak]
            sel_model,
            move |_| {
                sel_model.select_all();
            }
        ));
        self.imp().sel_none.connect_clicked(clone!(
            #[weak]
            sel_model,
            move |_| {
                sel_model.unselect_all();
            }
        ));

        // Set up factory
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak]
            library,
            #[weak]
            cache,
            #[weak]
            window,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache), Some(&window), Some(&library));
                item.property_expression("item")
                    .chain_property::<Song>("name")
                    .bind(&row, "name", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("album")
                    .bind(&row, "first-attrib-text", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("artist")
                    .bind(&row, "second-attrib-text", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("duration")
                    .chain_closure::<String>(closure_local!(|_: Option<glib::Object>, dur: u64| {
                        format_secs_as_duration(dur as f64)
                    }))
                    .bind(&row, "duration", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("quality-grade")
                    .bind(&row, "quality-grade", gtk::Widget::NONE);
                let end_widget = RowAddButtons::new(&library);
                row.set_end_widget(&end_widget.into());
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
            child.bind(&item);
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
            child.unbind();
        });

        // Set the factory of the list view
        self.imp().list_view.set_factory(Some(&factory));

        // No click action. SongView actions are via the top action bar.
    }
}

impl LazyInit for SongView {
    fn populate(&self) {
        if let Some(library) = self.imp().library.upgrade()
            && !self.imp().initializing.get()
        {
            self.imp().initializing.set(true);
            let stack = self.imp().stack.get();
            let this = self.clone();
            // Don't show spinner here: just let the song list appear gradually
            stack.show_content();
            // In return, disable queue buttons while we're loading songs.
            self.set_is_queuing(true);
            glib::spawn_future_local(async move {
                // Just get basic info first to reduce spinner time
                let _ = library.init_songs(false).await;
                if library.songs().n_items() == 0 {
                    stack.show_placeholder();
                }
                this.imp().initializing.set(false);
                this.set_is_queuing(false);
                // Now populate genres
                let _ = library.init_genres().await;
            });
        }
    }
}

impl SearchableView for SongView {
    fn trigger_search(&self) {
        self.imp().search_bar.set_search_mode(true);
    }
}
