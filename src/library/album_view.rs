use adw::prelude::*;
use adw::subclass::prelude::*;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{Properties, clone, subclass::Signal, WeakRef};
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
    glib::{self},
};
use std::{cell::Cell, cell::OnceCell, cmp::Ordering, rc::Rc, sync::OnceLock};

use super::{AlbumCell, AlbumContentView, Library};
use crate::{
    cache::Cache,
    client::ClientState,
    common::{Album, Rating},
    utils::{LazyInit, g_cmp_options, g_cmp_str_options, g_search_substr, settings_manager},
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

        // Content
        #[template_child]
        pub grid_view: TemplateChild<gtk::GridView>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub content_view: TemplateChild<AlbumContentView>,

        // Search & filter models
        pub search_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: WeakRef<Library>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,
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
            // Note to self: to work with menus, an action's state must be boolean or string.
            let action_sort_by = ActionEntry::builder("sort-by")
                .parameter_type(Some(&String::static_variant_type()))
                .state(state.enum_("sort-by").to_string().into())
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    state,
                    move |_, action, param| {
                        let param = param
                            .expect("Could not get parameter.")
                            .get::<String>()
                            .expect("The value needs to be of type `String`.");
                        let idx = param.parse::<i32>().unwrap();

                        if state.set_enum("sort-by", idx).is_ok() {
                            this.sorter.changed(gtk::SorterChange::Different);
                            action.set_state(&param.to_variant());
                        }
                    }
                ))
                .build();

            let action_sort_direction = ActionEntry::builder("sort-direction")
                .parameter_type(Some(&String::static_variant_type()))
                .state(state.enum_("sort-direction").to_string().into())
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    state,
                    move |_, action, param| {
                        let param = param
                            .expect("Could not get parameter.")
                            .get::<String>()
                            .expect("The value needs to be of type `String`.");
                        let idx = param.parse::<i32>().unwrap();

                        if state.set_enum("sort-direction", idx).is_ok() {
                            this.sorter.changed(gtk::SorterChange::Inverted);
                            action.set_state(&param.to_variant());
                        }
                    }
                ))
                .build();

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
                                Some(album1.get_sortable_title()),
                                Some(album2.get_sortable_title()),
                                nulls_first,
                                asc,
                                case_sensitive,
                            )
                        }
                        4 => {
                            // AlbumArtist
                            g_cmp_str_options(
                                album1.get_sortable_artist_tag(),
                                album2.get_sortable_artist_tag(),
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
                        8 => {
                            // Rating
                            g_cmp_options(
                                album1.get_rating().as_ref(),
                                album2.get_rating().as_ref(),
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
                        this.sorter.changed(gtk::SorterChange::Different);
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
                        this.sorter.changed(gtk::SorterChange::Inverted);
                    }
                ),
            );

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

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_sort_by, action_sort_direction]);
            self.obj().insert_action_group("album-view", Some(&actions));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
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
        self.imp()
            .library
            .set(Some(&library));
        self.setup_gridview(cache.clone());

        let content_view = self.imp().content_view.get();
        content_view.setup(library, client_state, cache, window);
        self.imp().content_page.connect_hidden(move |_| {
            content_view.unbind();
        });
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
            .sync_create()
            .build();

        // Chain search & sort. Put sort after search to reduce number of sort items.
        let search_model = gtk::FilterListModel::new(
            Some(album_list.clone()),
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
        if let Some(library) = self.imp().library.upgrade() {
            // TODO: add spinner
            glib::spawn_future_local(async move {
                let _ = library.init_albums().await;
            });
        }
    }
}
