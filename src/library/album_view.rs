use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection, gio,
    glib::{self},
    glib::{Properties, SignalHandlerId, WeakRef, clone, subclass::Signal},
};
use std::cell::{OnceCell, RefCell};
use std::{cell::Cell, cmp::Ordering, rc::Rc, sync::OnceLock};

use super::{
    AlbumCell, AlbumContentView, Library, TagsFilter,
    album_sort::{ALBUM_SORT_SETTINGS, AlbumSortCriterion, album_sort_tiers, compare_albums},
};
use crate::{
    cache::{Cache, sqlite},
    client::ClientState,
    common::{Album, ContentStack, Rating},
    utils::{LazyInit, SearchableView, g_search_substr, settings_manager},
    window::EuphonicaWindow,
};

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate, Properties, Default)]
    #[properties(wrapper_type = super::AlbumView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-view.ui")]
    pub struct AlbumView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
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
        pub reverse: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub reverse_2: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub reverse_3: TemplateChild<gtk::CheckButton>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_mode: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub rating: TemplateChild<Rating>,
        #[template_child]
        pub rating_mode: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub genres_filter_widget: TemplateChild<TagsFilter>,
        #[template_child]
        pub tags_filter_widget: TemplateChild<TagsFilter>,

        // Content
        #[template_child]
        pub stack: TemplateChild<ContentStack>,
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub content_view: TemplateChild<AlbumContentView>,

        // Search & filter models
        pub search_filter: gtk::CustomFilter,
        pub genres_filter: gtk::CustomFilter,
        pub tags_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: WeakRef<Library>,
        pub sort_handlers: RefCell<Option<(gio::Settings, Vec<SignalHandlerId>)>>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,

        pub initializing: Cell<bool>,
        pub window: OnceCell<WeakRef<EuphonicaWindow>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for AlbumView {
        const NAME: &'static str = "EuphonicaAlbumView";
        type Type = super::AlbumView;
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
    impl ObjectImpl for AlbumView {
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
            let state = settings.child("state").child("albumview");
            let library_settings = settings.child("library");

            // Setup sorting
            let view_options_btn = self.view_options_btn.get();
            state
                .bind("sort-direction", &view_options_btn, "icon-name")
                .get_only()
                .mapping(|dir, _| match dir.get::<String>().unwrap().as_ref() {
                    "asc" => Some("view-sort-ascending-symbolic".to_value()),
                    _ => Some("view-sort-descending-symbolic".to_value()),
                })
                .build();
            let tier_widgets = [
                (self.sort_by.get(), self.reverse.get()),
                (self.sort_by_2.get(), self.reverse_2.get()),
                (self.sort_by_3.get(), self.reverse_3.get()),
            ];
            for (setting_keys, (sort_by, reverse)) in
                ALBUM_SORT_SETTINGS.into_iter().zip(tier_widgets)
            {
                constructed_bind_sort_tier(&state, setting_keys, &sort_by, &reverse);
            }

            self.sorter.set_sort_func(clone!(
                #[strong]
                library_settings,
                #[strong]
                state,
                move |obj1, obj2| {
                    let album1 = obj1
                        .downcast_ref::<Album>()
                        .expect("Sort obj has to be a common::Album.");

                    let album2 = obj2
                        .downcast_ref::<Album>()
                        .expect("Sort obj has to be a common::Album.");

                    // Should the sorting be case-sensitive, i.e. uppercase goes first?
                    let case_sensitive = library_settings.boolean("sort-case-sensitive");
                    // Should nulls be put first or last?
                    let nulls_first = library_settings.boolean("sort-nulls-first");

                    let tiers = album_sort_tiers(&state);
                    compare_albums(album1, album2, &tiers, nulls_first, case_sensitive)
                }
            ));

            // Update when changing sort settings.
            // Not Inverted on a direction flip: the other tiers keep their own direction.
            let sort_handler_ids = ALBUM_SORT_SETTINGS
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
                    let album = obj
                        .downcast_ref::<Album>()
                        .expect("Search obj has to be a common::Album.");

                    // Filter by rating
                    let this_rating = album.get_rating();
                    let filter_rating = this.rating.value();
                    let matches_rating = match this.rating_mode.selected() {
                        0 => true,
                        1 => this_rating.is_some() && this_rating.unwrap() >= filter_rating,
                        2 => this_rating.is_some() && this_rating.unwrap() < filter_rating,
                        3 => this_rating.is_some() && this_rating.unwrap() == filter_rating,
                        _ => unimplemented!(),
                    };

                    if !matches_rating {
                        return false;
                    }

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
                            // Match either album title or AlbumArtist (not artist tag)
                            g_search_substr(Some(album.get_title()), &search_term, case_sensitive)
                                || g_search_substr(
                                    album.get_artist_tag(),
                                    &search_term,
                                    case_sensitive,
                                )
                        }
                        1 => {
                            // Match only album title
                            g_search_substr(Some(album.get_title()), &search_term, case_sensitive)
                        }
                        2 => {
                            // Match only AlbumArtist (albums without such tag will never match)
                            g_search_substr(album.get_artist_tag(), &search_term, case_sensitive)
                        }
                        _ => true,
                    }
                }
            ));

            // Tags and genres filter are set on changes only
            self.genres_filter.set_filter_func(|_| true);
            self.tags_filter.set_filter_func(|_| true);

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

            let rating = self.rating.get();
            let rating_mode = self.rating_mode.get();
            let search_mode = self.search_mode.get();
            for mode in [&rating_mode, &search_mode] {
                mode.connect_notify_local(
                    Some("selected"),
                    clone!(
                        #[weak(rename_to = this)]
                        self,
                        move |_, _| {
                            this.search_filter.changed(gtk::FilterChange::Different);
                        }
                    ),
                );
            }

            rating.connect_notify_local(
                Some("value"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _| {
                        if this.rating_mode.selected() > 0 {
                            this.search_filter.changed(gtk::FilterChange::Different);
                        }
                    }
                ),
            );

            rating_mode
                .bind_property("selected", &rating, "visible")
                .transform_to(|_, val: u32| Some(val > 0))
                .sync_create()
                .build();
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    fn constructed_bind_sort_tier(
        state: &gio::Settings,
        (sort_by_key, sort_direction_key): (&str, &str),
        sort_by: &gtk::DropDown,
        reverse: &gtk::CheckButton,
    ) {
        // A failing get mapping would make GSettings show the key's default instead,
        // so a value without a dropdown item (e.g. `filename`) clears the selection.
        state
            .bind(sort_by_key, sort_by, "selected")
            .mapping(|nick, _| {
                let nick = nick.get::<String>()?;
                let idx = AlbumSortCriterion::ALL
                    .iter()
                    .position(|criterion| criterion.nick() == nick)
                    .map_or(gtk::INVALID_LIST_POSITION, |idx| idx as u32);
                Some(idx.to_value())
            })
            .set_mapping(|idx, _| {
                let criterion = AlbumSortCriterion::ALL.get(idx.get::<u32>().ok()? as usize)?;
                Some(criterion.nick().to_variant())
            })
            .build();
        state
            .bind(sort_direction_key, reverse, "active")
            .mapping(|dir, _| Some((dir.get::<String>()? == "desc").to_value()))
            .set_mapping(|reverse, _| {
                let dir = if reverse.get::<bool>().ok()? {
                    "desc"
                } else {
                    "asc"
                };
                Some(dir.to_variant())
            })
            .build();
    }

    impl WidgetImpl for AlbumView {}
}

glib::wrapper! {
    pub struct AlbumView(ObjectSubclass<imp::AlbumView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for AlbumView {
    fn default() -> Self {
        Self::new()
    }
}

impl AlbumView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn setup(
        &self,
        library: &Library,
        cache: Rc<Cache>,
        client_state: &ClientState,
        window: &EuphonicaWindow,
    ) {
        self.imp().library.set(Some(library));
        let weak = WeakRef::new();
        weak.set(Some(window));

        self.imp()
            .window
            .set(weak)
            .expect("AlbumView window already set");
        self.setup_gridview(cache.clone());

        // Set up genres and tags filters
        self.imp().genres_filter_widget.setup(
            &library.genres(),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |genres| {
                    if !genres.is_empty() {
                        this.imp().genres_filter.set_filter_func(move |obj| {
                            let album = obj.downcast_ref::<Album>().unwrap();
                            !genres.is_disjoint(album.get_genres())
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

        self.imp().tags_filter_widget.setup(
            &library.album_tags(),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |tags| {
                    if !tags.is_empty() {
                        this.imp().tags_filter.set_filter_func(move |obj| {
                            let album = obj.downcast_ref::<Album>().unwrap();
                            sqlite::album_has_any_of_tags(album.get_folder_uri(), &tags)
                                .unwrap_or(true)
                        });
                        this.imp()
                            .tags_filter
                            .changed(gtk::FilterChange::MoreStrict);
                    } else {
                        this.imp().tags_filter.set_filter_func(|_| true);
                        this.imp()
                            .tags_filter
                            .changed(gtk::FilterChange::LessStrict);
                    }
                }
            ),
            window,
        );

        let content_view = self.imp().content_view.get();
        content_view.setup(library, client_state, cache, window);
        self.imp().content_page.connect_hidden(move |_| {
            content_view.unbind();
        });
    }

    pub fn search_bar(&self) -> gtk::SearchBar {
        self.imp().search_bar.get()
    }

    pub fn on_album_clicked(&self, album: &Album) {
        // - Upon receiving click signal, get the list item at the indicated activate index.
        // - Extract album from that list item.
        // - Bind AlbumContentView to that album. The content view should then populate itself.
        let content_view = self.imp().content_view.get();
        content_view.unbind();
        content_view.bind(album);
        if self
            .imp()
            .nav_view
            .visible_page_tag()
            .is_none_or(|tag| tag.as_str() != "content")
        {
            self.imp().nav_view.push_by_tag("content");
        }
    }

    fn setup_gridview(&self, cache: Rc<Cache>) {
        let settings = settings_manager().child("ui");
        // Setup search bar
        let album_list = self.imp().library.upgrade().unwrap().albums();
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
            Some(album_list.clone()),
            Some(self.imp().search_filter.clone()),
        );
        search_model.set_incremental(true);
        let genres_model =
            gtk::FilterListModel::new(Some(search_model), Some(self.imp().genres_filter.clone()));
        genres_model.set_incremental(true);
        let tags_model =
            gtk::FilterListModel::new(Some(genres_model), Some(self.imp().tags_filter.clone()));
        tags_model.set_incremental(true);

        let sort_model = gtk::SortListModel::new(Some(tags_model), Some(self.imp().sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        let grid_view = self.imp().grid_view.get();
        grid_view.set_model(Some(&sel_model));
        settings
            .bind("max-columns", &grid_view, "max-columns")
            .build();

        // Set up factory
        let factory = SignalListItemFactory::new();

        // Create an empty `AlbumCell` during setup
        factory.connect_setup(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let album_cell = AlbumCell::new(item, cache, None);
                item.set_child(Some(&album_cell));
            }
        ));

        // Tell factory how to bind `AlbumCell` to one of our Album GObjects.
        // If this cell is being bound to an album, that means it might be displayed.
        // As such, we'll also make it listen to the cache controller for any new
        // album art downloads. This ensures we will never have to iterate through
        // the entire grid to update album arts (only visible or nearly visible cells
        // will be updated, thus yielding a constant update cost).
        factory.connect_bind(move |_, list_item| {
            // Get `Album` from `ListItem` (that is, the data side)
            let item: Album = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Album>()
                .expect("The item has to be a common::Album.");

            // Get `AlbumCell` from `ListItem` (the UI widget)
            let child: AlbumCell = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumCell>()
                .expect("The child has to be an `AlbumCell`.");
            child.bind(&item);
        });

        // When cell goes out of sight, unbind from item to allow reuse with another.
        // Remember to also unset the thumbnail widget's texture to potentially free it from memory.
        factory.connect_unbind(move |_, list_item| {
            // Get `AlbumCell` from `ListItem` (the UI widget)
            let child: AlbumCell = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumCell>()
                .expect("The child has to be an `AlbumCell`.");
            child.unbind();
        });

        // Set the factory of the list view
        self.imp().grid_view.set_factory(Some(&factory));

        // Setup click action
        self.imp().grid_view.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |grid_view, position| {
                let model = grid_view.model().expect("The model has to exist.");
                let album = model
                    .item(position)
                    .and_downcast::<Album>()
                    .expect("The item has to be a `common::Album`.");
                this.on_album_clicked(&album);
            }
        ));
    }
}

impl LazyInit for AlbumView {
    fn populate(&self) {
        if let Some(library) = self.imp().library.upgrade()
            && !self.imp().initializing.get()
        {
            self.imp().initializing.set(true);
            let stack = self.imp().stack.get();
            let this = self.clone();
            stack.show_spinner();
            glib::spawn_future_local(async move {
                // Just get basic info first to reduce spinner time
                let _ = library.init_albums_and_albumartists().await;
                if library.albums().n_items() > 0 {
                    stack.show_content();
                } else {
                    stack.show_placeholder();
                }
                this.imp().initializing.set(false);
                // Now populate the stickers and genres
                let _ = futures::join!(library.init_genres(), library.refresh_album_tags());
            });
        }
    }
}

impl SearchableView for AlbumView {
    fn trigger_search(&self) {
        self.imp().search_bar.set_search_mode(true);
    }
}
