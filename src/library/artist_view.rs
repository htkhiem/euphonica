use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    glib,
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
};
use std::{cell::Cell, cmp::Ordering, rc::Rc};

use glib::clone;

use super::{ArtistCell, ArtistContentView, Library};
use crate::{
    cache::Cache,
    client::ClientState,
    common::Artist,
    utils::{g_cmp_str_options, g_search_substr, settings_manager},
};

mod imp {

    use std::{cell::OnceCell, sync::OnceLock};

    use glib::{subclass::Signal, Properties};

    use super::*;

    #[derive(Debug, CompositeTemplate, Properties)]
    #[properties(wrapper_type = super::ArtistView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-view.ui")]
    pub struct ArtistView {
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
        pub search_btn: TemplateChild<gtk::ToggleButton>,
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
        pub content_view: TemplateChild<ArtistContentView>,

        // Search & filter models
        pub search_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        #[property(get, set)]
        pub collapsed: Cell<bool>,

        pub library: OnceCell<Library>
    }

    impl Default for ArtistView {
        fn default() -> Self {
            Self {
                nav_view: TemplateChild::default(),
                show_sidebar: TemplateChild::default(),
                // Search & filter widgets
                sort_dir: TemplateChild::default(),
                sort_dir_btn: TemplateChild::default(),
                // sort_mode: TemplateChild::default(),
                search_btn: TemplateChild::default(),
                // search_mode: TemplateChild::default(),
                search_bar: TemplateChild::default(),
                search_entry: TemplateChild::default(),
                // Content
                grid_view: TemplateChild::default(),
                content_page: TemplateChild::default(),
                content_view: TemplateChild::default(),

                // Search & filter models
                search_filter: gtk::CustomFilter::default(),
                sorter: gtk::CustomSorter::default(),
                // Keep last length to optimise search
                // If search term is now longer, only further filter still-matching
                // items.
                // If search term is now shorter, only check non-matching items to see
                // if they now match.
                last_search_len: Cell::new(0),
                collapsed: Cell::new(false),

                library: OnceCell::new()
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArtistView {
        const NAME: &'static str = "EuphonicaArtistView";
        type Type = super::ArtistView;
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
    impl ObjectImpl for ArtistView {
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

    impl WidgetImpl for ArtistView {}
}

glib::wrapper! {
    pub struct ArtistView(ObjectSubclass<imp::ArtistView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ArtistView {
    fn default() -> Self {
        Self::new()
    }
}

impl ArtistView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();
        res
    }

    pub fn setup(&self, library: Library, client_state: ClientState, cache: Rc<Cache>) {
        self.imp().library.set(library.clone()).expect("Unable to register library with Artist View");
        self.setup_sort();
        self.setup_search();
        self.setup_gridview(client_state.clone(), cache.clone());

        let content_view = self.imp().content_view.get();
        content_view.setup(library, cache, client_state);
        self.imp().content_page.connect_hidden(move |_| {
            content_view.unbind();
        });
    }

    pub fn get_content_view(&self) -> ArtistContentView {
        self.imp().content_view.get()
    }

    fn setup_sort(&self) {
        // TODO: use albumsort & albumartistsort tags where available
        // Setup sort widget & actions
        let settings = settings_manager();
        let state = settings.child("state").child("artistview");
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

        self.imp().sorter.set_sort_func(clone!(
            #[strong]
            library_settings,
            #[strong]
            state,
            move |obj1, obj2| {
                let artist1 = obj1
                    .downcast_ref::<Artist>()
                    .expect("Sort obj has to be a common::Artist.");

                let artist2 = obj2
                    .downcast_ref::<Artist>()
                    .expect("Sort obj has to be a common::Artist.");

                // Should we sort ascending?
                let asc = state.enum_("sort-direction") > 0;
                // Should the sorting be case-sensitive, i.e. uppercase goes first?
                let case_sensitive = library_settings.boolean("sort-case-sensitive");
                // Should nulls be put first or last?
                let nulls_first = library_settings.boolean("sort-nulls-first");

                g_cmp_str_options(
                    Some(artist1.get_name()),
                    Some(artist2.get_name()),
                    nulls_first,
                    asc,
                    case_sensitive,
                )
            }
        ));

        // Update when changing sort settings
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
                let artist = obj
                    .downcast_ref::<Artist>()
                    .expect("Search obj has to be a common::Artist.");

                let search_term = this.imp().search_entry.text();
                if search_term.is_empty() {
                    return true;
                }

                // Should the searching be case-sensitive?
                let case_sensitive = library_settings.boolean("search-case-sensitive");
                // Vary behaviour depending on dropdown
                g_search_substr(Some(artist.get_name()), &search_term, case_sensitive)
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
    }

    pub fn on_artist_clicked(&self, artist: &Artist) {
        // - Upon receiving click signal, get the list item at the indicated activate index.
        // - Extract artist from that list item.
        // - Bind ArtistContentView to that album. This will cause the ArtistContentView to start listening
        //   to the cache & client (MpdWrapper) states for arrival of avatar, album arts, etc.
        // - Try to ensure existence of local metadata by queuing download if necessary. Since
        //   ArtistContentView is now listening to the relevant signals, it will immediately update itself
        //   in an asynchronous manner.
        // - Schedule client to fetch the following:
        //   - All songs of this artist (for the "all songs" tab),
        //   - All albums of this artist (for the discography tab),
        //   - Art of the above albums, and
        //   - Artist metadata: bio, avatar, etc.
        // - Now we can push the AlbumContentView. At this point, it must already have been bound to at
        //   least the album's basic information (title, artist, etc). If we're lucky, it might also have
        //   its song list and wiki initialised, but that's not mandatory.
        // NOTE: We do not ensure local album art again in the above steps, since we have already done so
        // once when adding this album to the ListStore for the GridView.
        //
        let content_view = self.imp().content_view.get();
        content_view.unbind();
        content_view.bind(artist.clone());
        self.imp().nav_view.push_by_tag("content");
        self.imp().library.get().unwrap().init_artist(artist);
    }

    fn setup_gridview(&self, client_state: ClientState, cache: Rc<Cache>) {
        let settings = settings_manager().child("ui");
        // Refresh upon reconnection.
        // User-initiated refreshes will also trigger a reconnection, which will
        // in turn trigger this.
        let artists = self.imp().library.get().unwrap().artists();
        
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
            Some(artists.clone()),
            Some(self.imp().search_filter.clone()),
        );
        search_model.set_incremental(true);
        let sort_model =
            gtk::SortListModel::new(Some(search_model), Some(self.imp().sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        let grid_view = self.imp().grid_view.get();
        grid_view.set_model(Some(&sel_model));
        settings
            .bind(
                "max-columns",
                &grid_view,
                "max-columns"
            )
            .build();

        // Set up factory
        let factory = SignalListItemFactory::new();

        // Create an empty `ArtistCell` during setup
        factory.connect_setup(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let artist_cell = ArtistCell::new(&item, cache);
                item.set_child(Some(&artist_cell));
            }
        ));

        // Artist name has already been taken care of by the above property expression.
        // Here we only need to start listening to the cache for artist images.
        factory.connect_bind(move |_, list_item| {
            // Get `Artist` from `ListItem` (that is, the data side)
            let item: Artist = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Artist>()
                .expect("The item has to be a common::Artist.");

            // Get `ArtistCell` from `ListItem` (the UI widget)
            let child: ArtistCell = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<ArtistCell>()
                .expect("The child has to be an `ArtistCell`.");

            // Within this binding fn is where the cached album art texture gets used.
            child.bind(&item);
        });

        // When cell goes out of sight, unbind from item to allow reuse with another.
        // Remember to also unset the thumbnail widget's texture to potentially free it from memory.
        factory.connect_unbind(move |_, list_item| {
            // Get `ArtistCell` from `ListItem` (the UI widget)
            let child: ArtistCell = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<ArtistCell>()
                .expect("The child has to be an `ArtistCell`.");
            // Un-listen to cache, so that we don't update album art for cells that are not in view
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
                let artist = model
                    .item(position)
                    .and_downcast::<Artist>()
                    .expect("The item has to be a `common::Artist`.");
                println!("Clicked on {:?}", &artist);
                this.on_artist_clicked(&artist);
            }
        ));
    }
}
