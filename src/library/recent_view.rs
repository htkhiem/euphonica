use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    glib::{self},
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
};

use glib::{clone, closure_local, Properties};

use super::{AlbumCell, ArtistCell, Library};
use crate::{
    utils::LazyInit,
    cache::Cache,
    common::{marquee::MarqueeWrapMode, Album, Artist, Song},
    library::recent_song_row::RecentSongRow,
    player::Player,
    window::EuphonicaWindow
};

mod imp {
    use std::{cell::{Cell, OnceCell}, sync::OnceLock};

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
        #[template_child]
        pub clear: TemplateChild<gtk::Button>,
        #[template_child]
        pub stack: TemplateChild<gtk::Stack>,

        // Albums row
        #[template_child]
        pub collapse_albums: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub album_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub album_row_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub album_row: TemplateChild<gtk::GridView>,

        // Artists row
        #[template_child]
        pub collapse_artists: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub artist_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub artist_row_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub artist_row: TemplateChild<gtk::GridView>,

        // Songs list
        #[template_child]
        pub collapse_songs: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub song_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub song_list: TemplateChild<gtk::ListBox>,

        pub library: OnceCell<Library>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,

        pub initialized: Cell<bool>  // Only start fetching content when navigated to for the first time
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

            self.clear.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.library.get().unwrap().clear_recent_songs();
                    this.obj().on_history_changed();
                }
            ));

            self.collapse_albums
                .bind_property("active", &self.album_revealer.get(), "reveal-child")
                .invert_boolean()
                .sync_create()
                .build();

            self.album_revealer
                .bind_property("child-revealed", &self.collapse_albums.get(), "icon-name")
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
                .bind_property("active", &self.artist_revealer.get(), "reveal-child")
                .invert_boolean()
                .sync_create()
                .build();

            self.artist_revealer
                .bind_property("child-revealed", &self.collapse_artists.get(), "icon-name")
                .transform_to(|_, is_revealed| {
                    if is_revealed {
                        Some("up-symbolic".to_value())
                    } else {
                        Some("down-symbolic".to_value())
                    }
                })
                .sync_create()
                .build();

            self.collapse_songs
                .bind_property("active", &self.song_revealer.get(), "reveal-child")
                .invert_boolean()
                .sync_create()
                .build();

            self.song_revealer
                .bind_property("child-revealed", &self.collapse_songs.get(), "icon-name")
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
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    impl WidgetImpl for RecentView {}
}

glib::wrapper! {
    pub struct RecentView(ObjectSubclass<imp::RecentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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
        window: &EuphonicaWindow,
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
            ),
        );

        self.setup_album_row(window, cache.clone());
        self.setup_artist_row(window, cache.clone());
        self.setup_song_list(cache);
    }

    pub fn on_history_changed(&self) {
        if self.imp().initialized.get() {
            let library = self.imp().library.get().unwrap();
            library.fetch_recent_albums();
            library.fetch_recent_artists();
            library.fetch_recent_songs();
        }
    }

    fn setup_album_row(&self, window: &EuphonicaWindow, cache: Rc<Cache>) {
        let album_list = self.imp().library.get().unwrap().recent_albums();

        album_list
            .bind_property(
                "n-items",
                &self.imp().album_row_stack.get(),
                "visible-child-name"
            )
            .transform_to(|_, n: u32| {
                if n > 0 {
                    Some("content".to_value())
                } else {
                    Some("loading".to_value())
                }
            })
            .sync_create()
            .build();

        let sel_model = SingleSelection::new(Some(album_list));
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
                let album_cell = AlbumCell::new(&item, cache, Some(MarqueeWrapMode::Scroll));
                // propagating the tallest cell's height to the revealer if said row wasn't
                // the first initialised.
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
        let artist_list = self.imp().library.get().unwrap().recent_artists();

        artist_list
            .bind_property(
                "n-items",
                &self.imp().artist_row_stack.get(),
                "visible-child-name"
            )
            .transform_to(|_, n: u32| {
                if n > 0 {
                    Some("content".to_value())
                } else {
                    Some("loading".to_value())
                }
            })
            .sync_create()
            .build();

        let sel_model = SingleSelection::new(Some(artist_list));
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

    fn setup_song_list(&self, cache: Rc<Cache>) {
        let library = self.imp().library.get().unwrap().clone();
        let song_list = library.recent_songs();

        song_list
            .bind_property(
                "n-items",
                &self.imp().stack.get(),
                "visible-child-name",
            )
            .transform_to(|_, val: u32| {
                if val > 0 {
                    Some("content".to_value())
                } else {
                    Some("empty".to_value())
                }
            }
        )
         .sync_create()
         .build();

        self.imp().song_list.bind_model(
            Some(&song_list),
            move |obj| {
                let row = RecentSongRow::new(library.clone(), obj.downcast_ref::<Song>().unwrap(), cache.clone());
                row.into()
            }
        );
    }
}

impl LazyInit for RecentView {
    fn clear(&self) {
        self.imp().initialized.set(false);
    }

    fn populate(&self) {
        let was_populated = self.imp().initialized.replace(true);
        if !was_populated {
            println!("Initialising recents");
            self.on_history_changed();
        }
    }
}
