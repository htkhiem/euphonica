use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    gio,
    glib::{self},
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
    Ordering
};
use rustc_hash::FxHashMap;
use std::{cell::Cell, rc::Rc};

use glib::{clone, closure_local, Properties};

use super::{AlbumCell, ArtistCell, Library};
use crate::{
    cache::{sqlite, Cache}, client::ClientState, common::{Album, Artist}, player::Player, utils::{g_cmp_options, g_cmp_str_options, g_search_substr, settings_manager}, window::EuphonicaWindow
};

mod imp {
    use std::{cell::OnceCell, sync::OnceLock};

    use glib::subclass::Signal;

    use super::*;

    #[derive(Debug, CompositeTemplate, Properties, Default)]
    #[properties(wrapper_type = super::RecentView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/recent-view.ui")]
    pub struct RecentView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,

        // Albums row
        #[template_child]
        pub collapse_albums: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub album_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub album_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_row: TemplateChild<gtk::GridView>,

        // Artists row
        #[template_child]
        pub collapse_artists: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub artist_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub artist_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub artist_row: TemplateChild<gtk::GridView>,

        pub library: OnceCell<Library>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,

        // Search & filter models
        pub album_filter: gtk::CustomFilter,
        pub album_sorter: gtk::CustomSorter,

        pub artist_filter: gtk::CustomFilter,
        pub artist_sorter: gtk::CustomSorter,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecentView {
        const NAME: &'static str = "EuphonicaRecentView";
        type Type = super::RecentView;
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
    impl ObjectImpl for RecentView {
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

            self.collapse_albums
                .bind_property(
                    "active",
                    &self.album_revealer.get(),
                    "reveal-child"
                )
                .invert_boolean()
                .sync_create()
                .build();

            self.album_revealer
                .bind_property(
                    "child-revealed",
                    &self.collapse_albums.get(),
                    "icon-name"
                )
                .transform_to(|_, is_revealed| {
                    if is_revealed {
                        Some("up-symbolic".to_value())
                    } else {
                        Some("down-symbolic".to_value())
                    }
                })
                .sync_create()
                .build();

            self.collapse_artists
                .bind_property(
                    "active",
                    &self.artist_revealer.get(),
                    "reveal-child"
                )
                .invert_boolean()
                .sync_create()
                .build();

            self.artist_revealer
                .bind_property(
                    "child-revealed",
                    &self.collapse_artists.get(),
                    "icon-name"
                )
                .transform_to(|_, is_revealed| {
                    if is_revealed {
                        Some("up-symbolic".to_value())
                    } else {
                        Some("down-symbolic".to_value())
                    }
                })
                .sync_create()
                .build();
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

    impl WidgetImpl for RecentView {}
}

glib::wrapper! {
    pub struct RecentView(ObjectSubclass<imp::RecentView>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for RecentView {
    fn default() -> Self {
        Self::new()
    }
}

impl RecentView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn setup(
        &self,
        library: Library,
        player: Player,
        cache: Rc<Cache>,
        window: &EuphonicaWindow
    ) {
        self.imp()
            .library
            .set(library.clone())
            .expect("Cannot init RecentView with Library");

        self.on_history_changed();
        player.connect_closure(
            "history-changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: Player| {
                    this.on_history_changed();
                }
            )
        );

        self.setup_album_row(window, cache.clone());
        self.setup_artist_row(window, cache.clone());
    }

    pub fn on_history_changed(&self) {
        // Albums
        let recent_albums = sqlite::get_last_n_albums(10).expect("Sqlite DB error");
        println!("Recent albums: {:?}", &recent_albums);
        if recent_albums.len() > 0 {
            let mut albums_map: FxHashMap<String, usize> = FxHashMap::default();
            for tup in recent_albums.iter().enumerate() {
                albums_map.insert(tup.1.clone(), tup.0);
            }
            let albums_map_cloned = albums_map.clone();
            self.imp().album_filter.set_filter_func(move |obj| {
                let album = obj
                    .downcast_ref::<Album>()
                    .expect("Search obj has to be a common::Album.");

                albums_map_cloned.contains_key(album.get_title())
            });
            self.imp().album_filter.changed(gtk::FilterChange::Different);

            self.imp().album_sorter.set_sort_func(move |obj1: &glib::Object, obj2: &glib::Object| -> Ordering {
                let album1 = obj1
                    .downcast_ref::<Album>()
                    .expect("Sort obj has to be a common::Album.");

                let album2 = obj2
                    .downcast_ref::<Album>()
                    .expect("Sort obj has to be a common::Album.");

                if let (Some(order1), Some(order2)) = (albums_map.get(album1.get_title()), albums_map.get(album2.get_title())) {
                    Ordering::from(order1.cmp(order2))
                }
                 else {
                    Ordering::Equal
                }
            });
            self.imp().album_sorter.changed(gtk::SorterChange::Different);
        }
        else {
            // Don't show anything. This will also cause the stack to switch to the empty page.
            self.imp().album_filter.set_filter_func(|_| false);
            self.imp().album_filter.changed(gtk::FilterChange::Different);
        }

        // Artists
        let recent_artists = sqlite::get_last_n_artists(10).expect("Sqlite DB error");
        println!("Recent artists: {:?}", &recent_artists);
        if recent_artists.len() > 0 {
            let mut artists_map: FxHashMap<String, usize> = FxHashMap::default();
            for tup in recent_artists.iter().enumerate() {
                artists_map.insert(tup.1.clone(), tup.0);
            }
            let artists_map_cloned = artists_map.clone();
            self.imp().artist_filter.set_filter_func(move |obj| {
                let artist = obj
                    .downcast_ref::<Artist>()
                    .expect("Search obj has to be a common::Artist.");

                artists_map_cloned.contains_key(artist.get_name())
            });
            self.imp().artist_filter.changed(gtk::FilterChange::Different);

            self.imp().artist_sorter.set_sort_func(move |obj1: &glib::Object, obj2: &glib::Object| -> Ordering {
                let artist1 = obj1
                    .downcast_ref::<Artist>()
                    .expect("Sort obj has to be a common::Artist.");

                let artist2 = obj2
                    .downcast_ref::<Artist>()
                    .expect("Sort obj has to be a common::Artist.");

                if let (Some(order1), Some(order2)) = (artists_map.get(artist1.get_name()), artists_map.get(artist2.get_name())) {
                    Ordering::from(order1.cmp(order2))
                }
                 else {
                    Ordering::Equal
                }
            });
            self.imp().artist_sorter.changed(gtk::SorterChange::Different);
        }
        else {
            // Don't show anything. This will also cause the stack to switch to the empty page.
            self.imp().artist_filter.set_filter_func(|_| false);
            self.imp().artist_filter.changed(gtk::FilterChange::Different);
        }
    }

    fn setup_album_row(&self, window: &EuphonicaWindow, cache: Rc<Cache>) {
        let album_list = self.imp().library.get().unwrap().albums();

        // Chain search & sort. Put sort after search to reduce number of sort items.
        let search_model = gtk::FilterListModel::new(
            Some(album_list.clone()),
            Some(self.imp().album_filter.clone()),
        );
        search_model.set_incremental(true);
        // Switch to empty page in case nothing is returned
        search_model
            .bind_property(
                "n-items",
                &self.imp().album_stack.get(),
                "visible-child-name"
            )
            .transform_to(|_, val: u32| {
                if val > 0 {
                    Some("row".to_value())
                }
                else {
                    Some("empty".to_value())
                }
            })
            .sync_create()
            .build();

        let sort_model =
            gtk::SortListModel::new(Some(search_model), Some(self.imp().album_sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        self.imp().album_row.set_model(Some(&sel_model));

        // Set up factory
        let factory = SignalListItemFactory::new();
        let adj = self.imp().album_row.hadjustment().unwrap();

        // Create an empty `AlbumCell` during setup.
        // Reset scroll position to zero every time a new item is created such that
        // upon startup or insertion of a new just-listened album we'll be at the
        // start of the row.
        factory.connect_setup(clone!(
            #[weak]
            cache,
            #[weak]
            adj,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let album_cell = AlbumCell::new(&item, cache);
                item.set_child(Some(&album_cell));
                adj.set_value(0.0);
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
        self.imp().album_row.set_factory(Some(&factory));

        // Setup click action
        self.imp().album_row.connect_activate(clone!(
            #[weak]
            window,
            move |grid_view, position| {
                let model = grid_view.model().expect("The model has to exist.");
                let album = model
                    .item(position)
                    .and_downcast::<Album>()
                    .expect("The item has to be a `common::Album`.");
                window.goto_album(&album);
            }
        ));
    }


    fn setup_artist_row(&self, window: &EuphonicaWindow, cache: Rc<Cache>) {
        let artist_list = self.imp().library.get().unwrap().artists();

        // Chain search & sort. Put sort after search to reduce number of sort items.
        let search_model = gtk::FilterListModel::new(
            Some(artist_list.clone()),
            Some(self.imp().artist_filter.clone()),
        );
        search_model.set_incremental(true);
        // Switch to empty page in case nothing is returned
        search_model
            .bind_property(
                "n-items",
                &self.imp().artist_stack.get(),
                "visible-child-name"
            )
            .transform_to(|_, val: u32| {
                if val > 0 {
                    Some("row".to_value())
                }
                else {
                    Some("empty".to_value())
                }
            })
            .sync_create()
            .build();

        let sort_model =
            gtk::SortListModel::new(Some(search_model), Some(self.imp().artist_sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        self.imp().artist_row.set_model(Some(&sel_model));

        // Set up factory
        let factory = SignalListItemFactory::new();
        let adj = self.imp().artist_row.hadjustment().unwrap();

        // Create an empty `ArtistCell` during setup
        factory.connect_setup(clone!(
            #[weak]
            cache,
            #[weak]
            adj,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let artist_cell = ArtistCell::new(&item, cache);
                item.set_child(Some(&artist_cell));
                adj.set_value(0.0);
                adj.set_value(0.0);
                adj.set_value(0.0);
            }
        ));

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
            child.unbind();
        });

        // Set the factory of the list view
        self.imp().artist_row.set_factory(Some(&factory));

        // Setup click action
        self.imp().artist_row.connect_activate(clone!(
            #[weak]
            window,
            move |grid_view, position| {
                let model = grid_view.model().expect("The model has to exist.");
                let artist = model
                    .item(position)
                    .and_downcast::<Artist>()
                    .expect("The item has to be a `common::Artist`.");
                window.goto_artist(&artist);
            }
        ));
    }
}
