use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    gio,
    glib::{self, closure_local},
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
};
use std::{cell::Cell, cmp::Ordering, rc::Rc};

use glib::{clone, Properties};

use super::{AlbumCell, AlbumContentView, Library};
use crate::{
    cache::Cache,
    client::{ClientState, ConnectionState},
    common::Album,
    utils::{g_cmp_options, g_cmp_str_options, g_search_substr, settings_manager},
};

mod imp {
    use std::{cell::OnceCell, sync::OnceLock};

    use glib::subclass::Signal;

    use super::*;

    #[derive(Debug, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::AlbumView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-view.ui")]
    pub struct AlbumView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,

        // Search & filter widgets
        #[template_child]
        pub sort_dir: TemplateChild<gtk::Image>,
        #[template_child]
        pub sort_dir_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub sort_mode: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub search_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub search_mode: TemplateChild<gtk::DropDown>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,

        // Content
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub content_view: TemplateChild<AlbumContentView>,

        pub album_list: gio::ListStore,
        // Search & filter models
        pub search_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: OnceCell<Library>,

        #[property(get, set)]
        pub collapsed: Cell<bool>
    }

    impl Default for AlbumView {
        fn default() -> Self {
            Self {
                nav_view: TemplateChild::default(),
                show_sidebar: TemplateChild::default(),
                // Search & filter widgets
                sort_dir: TemplateChild::default(),
                sort_dir_btn: TemplateChild::default(),
                sort_mode: TemplateChild::default(),
                search_btn: TemplateChild::default(),
                search_mode: TemplateChild::default(),
                search_bar: TemplateChild::default(),
                search_entry: TemplateChild::default(),
                // Content
                grid_view: TemplateChild::default(),
                content_page: TemplateChild::default(),
                content_view: TemplateChild::default(),
                album_list: gio::ListStore::new::<Album>(),
                // Search & filter models
                search_filter: gtk::CustomFilter::default(),
                sorter: gtk::CustomSorter::default(),
                // Keep last length to optimise search
                // If search term is now longer, only further filter still-matching
                // items.
                // If search term is now shorter, only check non-matching items to see
                // if they now match.
                last_search_len: Cell::new(0),
                library: OnceCell::new(),

                collapsed: Cell::new(false)
            }
        }
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
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.obj()
                .bind_property(
                    "collapsed",
                    &self.show_sidebar.get(),
                    "visible"
                )
                .sync_create()
                .build();

            self.show_sidebar.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().emit_by_name::<()>("show-sidebar-clicked", &[]);
                }
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("show-sidebar-clicked").build(),
                ]
            })
        }
    }

    impl WidgetImpl for AlbumView {}
}

glib::wrapper! {
    pub struct AlbumView(ObjectSubclass<imp::AlbumView>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
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

    pub fn setup(&self, library: Library, cache: Rc<Cache>, client_state: ClientState) {
        self.setup_sort();
        self.setup_search();
        self.imp()
            .library
            .set(library.clone())
            .expect("Cannot init AlbumView with Library");
        self.setup_gridview(client_state.clone(), cache.clone());

        let content_view = self.imp().content_view.get();
        content_view.setup(library.clone(), client_state, cache);
        self.imp().content_page.connect_hidden(move |_| {
            content_view.unbind();
        });
    }

    fn setup_sort(&self) {
        // TODO: use albumsort & albumartistsort tags where available
        // Setup sort widget & actions
        let settings = settings_manager();
        let state = settings.child("state").child("albumview");
        let library_settings = settings.child("library");
        let sort_dir_btn = self.imp().sort_dir_btn.get();
        sort_dir_btn.connect_clicked(clone!(
            #[weak]
            state,
            move |_| {
                if state.string("sort-direction") == "asc" {
                    let _ = state.set_string("sort-direction", "desc");
                } else {
                    let _ = state.set_string("sort-direction", "asc");
                }
            }
        ));
        let sort_dir = self.imp().sort_dir.get();
        state
            .bind("sort-direction", &sort_dir, "icon-name")
            .get_only()
            .mapping(|dir, _| match dir.get::<String>().unwrap().as_ref() {
                "asc" => Some("view-sort-ascending-symbolic".to_value()),
                _ => Some("view-sort-descending-symbolic".to_value()),
            })
            .build();
        let sort_mode = self.imp().sort_mode.get();
        state
            .bind("sort-by", &sort_mode, "selected")
            .mapping(|val, _| {
                // TODO: i18n
                match val.get::<String>().unwrap().as_ref() {
                    "album-title" => Some(0.to_value()),
                    "album-artist" => Some(1.to_value()),
                    "release-date" => Some(2.to_value()),
                    _ => unreachable!(),
                }
            })
            .set_mapping(|val, _| match val.get::<u32>().unwrap() {
                0 => Some("album-title".to_variant()),
                1 => Some("album-artist".to_variant()),
                2 => Some("release-date".to_variant()),
                _ => unreachable!(),
            })
            .build();
        self.imp().sorter.set_sort_func(clone!(
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

                // Should we sort ascending?
                let asc = state.enum_("sort-direction") > 0;
                // Should the sorting be case-sensitive, i.e. uppercase goes first?
                let case_sensitive = library_settings.boolean("sort-case-sensitive");
                // Should nulls be put first or last?
                let nulls_first = library_settings.boolean("sort-nulls-first");

                // Vary behaviour depending on sort menu
                match state.enum_("sort-by") {
                    // Refer to the io.github.htkhiem.Euphonica.sortby enum the gschema
                    3 => {
                        // Album title
                        g_cmp_str_options(
                            Some(album1.get_title()),
                            Some(album2.get_title()),
                            nulls_first,
                            asc,
                            case_sensitive,
                        )
                    }
                    4 => {
                        // AlbumArtist
                        g_cmp_str_options(
                            album1.get_artist_str().as_deref(),
                            album2.get_artist_str().as_deref(),
                            nulls_first,
                            asc,
                            case_sensitive,
                        )
                    }
                    5 => {
                        // Release date
                        g_cmp_options(
                            album1.get_release_date().as_ref(),
                            album2.get_release_date().as_ref(),
                            nulls_first,
                            asc,
                        )
                    }
                    _ => unreachable!(),
                }
            }
        ));

        // Update when changing sort settings
        state.connect_changed(
            Some("sort-by"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    println!("Updating sort...");
                    this.imp().sorter.changed(gtk::SorterChange::Different);
                }
            ),
        );
        state.connect_changed(
            Some("sort-direction"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    println!("Flipping sort...");
                    // Don't actually sort, just flip the results :)
                    this.imp().sorter.changed(gtk::SorterChange::Inverted);
                }
            ),
        );
    }

    fn setup_search(&self) {
        let settings = settings_manager();
        let library_settings = settings.child("library");
        // Set up search filter
        self.imp().search_filter.set_filter_func(clone!(
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

                let search_term = this.imp().search_entry.text();
                if search_term.is_empty() {
                    return true;
                }

                // Should the searching be case-sensitive?
                let case_sensitive = library_settings.boolean("search-case-sensitive");
                // Vary behaviour depending on dropdown
                match this.imp().search_mode.selected() {
                    // Keep these indices in sync with the GtkStringList in the UI file
                    0 => {
                        // Match either album title or AlbumArtist (not artist tag)
                        g_search_substr(Some(album.get_title()), &search_term, case_sensitive)
                            || g_search_substr(
                                album.get_artist_str().as_deref(),
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
                        g_search_substr(
                            album.get_artist_str().as_deref(),
                            &search_term,
                            case_sensitive,
                        )
                    }
                    _ => true,
                }
            }
        ));

        // Connect search entry to filter. Filter will later be put in GtkSearchModel.
        // That GtkSearchModel will listen to the filter's changed signal.
        let search_entry = self.imp().search_entry.get();
        search_entry.connect_search_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |entry| {
                let text = entry.text();
                let new_len = text.len();
                let old_len = this.imp().last_search_len.replace(new_len);
                match new_len.cmp(&old_len) {
                    Ordering::Greater => {
                        this.imp()
                            .search_filter
                            .changed(gtk::FilterChange::MoreStrict);
                    }
                    Ordering::Less => {
                        this.imp()
                            .search_filter
                            .changed(gtk::FilterChange::LessStrict);
                    }
                    Ordering::Equal => {
                        this.imp()
                            .search_filter
                            .changed(gtk::FilterChange::Different);
                    }
                }
            }
        ));

        let search_mode = self.imp().search_mode.get();
        search_mode.connect_notify_local(
            Some("selected"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    println!("Changed search mode");
                    this.imp()
                        .search_filter
                        .changed(gtk::FilterChange::Different);
                }
            ),
        );
    }

    pub fn on_album_clicked(&self, album: &Album) {
        // - Upon receiving click signal, get the list item at the indicated activate index.
        // - Extract album from that list item.
        // - Bind AlbumContentView to that album. This will cause the AlbumContentView to start listening
        //   to the cache & client (MpdWrapper) states for arrival of album arts, contents & metadata.
        // - Try to ensure existence of local metadata by queuing download if necessary. Since
        //   AlbumContentView is now listening to the relevant signals, it will immediately update itself
        //   in an asynchronous manner.
        // - Schedule client to fetch all songs with this album tag in the same manner.
        // - Now we can push the AlbumContentView. At this point, it must already have been bound to at
        //   least the album's basic information (title, artist, etc). If we're lucky, it might also have
        //   its song list and wiki initialised, but that's not mandatory.
        // NOTE: We do not ensure local album art again in the above steps, since we have already done so
        // once when adding this album to the ListStore for the GridView.
        let content_view = self.imp().content_view.get();
        content_view.bind(album.clone());
        self.imp()
            .library
            .get()
            .expect("AlbumView is incorrectly set up (no Library reference)")
            .init_album(album);
        self.imp().nav_view.push_by_tag("content");
    }

    fn setup_gridview(&self, client_state: ClientState, cache: Rc<Cache>) {
        // Refresh upon reconnection.
        // User-initiated refreshes will also trigger a reconnection, which will
        // in turn trigger this.
        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |state, _| {
                    if state.get_connection_state() == ConnectionState::Connected {
                        this.clear();
                        this.imp().library.get().unwrap().init_albums();
                    }
                }
            ),
        );
        client_state.connect_closure(
            "album-basic-info-downloaded",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |_: ClientState, album: Album| {
                    this.add_album(album);
                }
            ),
        );
        // Setup search bar
        let search_bar = self.imp().search_bar.get();
        let search_entry = self.imp().search_entry.get();
        search_bar.connect_entry(&search_entry);

        let search_btn = self.imp().search_btn.get();
        search_btn
            .bind_property("active", &search_bar, "search-mode-enabled")
            .sync_create()
            .build();

        // Chain search & sort. Put sort after search to reduce number of sort items.
        let search_model = gtk::FilterListModel::new(
            Some(self.imp().album_list.clone()),
            Some(self.imp().search_filter.clone()),
        );
        search_model.set_incremental(true);
        let sort_model =
            gtk::SortListModel::new(Some(search_model), Some(self.imp().sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        self.imp().grid_view.set_model(Some(&sel_model));

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
                let album_cell = AlbumCell::new(&item, cache);
                item.set_child(Some(&album_cell));
            }
        ));

        factory.connect_teardown(move |_, list_item| {
            // Get `AlbumCell` from `ListItem` (the UI widget)
            let child: Option<AlbumCell> = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<AlbumCell>();
            if let Some(c) = child {
                c.teardown();
            }
        });

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

    fn add_album(&self, album: Album) {
        self.imp().album_list.append(&album);
        // self.imp().album_count.set_label(&self.imp().album_list.n_items().to_string());
    }

    pub fn clear(&self) {
        self.imp().album_list.remove_all();
    }
}
