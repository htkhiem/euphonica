use std::rc::Rc;
use std::{cell::Cell, sync::OnceLock};

use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
    glib::{self},
};

use glib::{Properties, WeakRef, clone, closure_local, subclass::Signal};

use super::{AlbumCell, ArtistCell, Library};
use crate::{
    client::{Result as ClientResult},
    cache::Cache,
    common::{Album, Artist, RowAddButtons, Song, SongRow, marquee::MarqueeWrapMode, ContentStack},
    player::Player,
    utils::LazyInit,
    window::EuphonicaWindow,
};

mod imp {
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
        pub stack: TemplateChild<ContentStack>,

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

        pub library: WeakRef<Library>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,
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

            self.clear.connect_clicked(clone!(
                #[weak(rename_to = this)] self,
                move |_| {
                    glib::spawn_future_local(clone!(#[weak] this, async move {
                        if let Err(e) = this.library.upgrade().unwrap().clear_recent_songs().await {
                            dbg!(e);
                        } else {
                            this.obj().on_history_changed();
                        }
                    }));
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
        library: &Library,
        player: &Player,
        cache: Rc<Cache>,
        window: &EuphonicaWindow,
    ) {
        self.imp().library.set(Some(library));

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
        glib::spawn_future_local(clone!(#[weak(rename_to = this)] self, async move {
            if let Err(e) = this.init_recent(true).await {
                dbg!(e);
            }
        }));
    }

    fn setup_album_row(&self, window: &EuphonicaWindow, cache: Rc<Cache>) {
        let album_list = self.imp().library.upgrade().unwrap().recent_albums();

        album_list
            .bind_property(
                "n-items",
                &self.imp().album_row_stack.get(),
                "visible-child-name",
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
                let album_cell = AlbumCell::new(item, cache, Some(MarqueeWrapMode::Scroll));
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
        let artist_list = self.imp().library.upgrade().unwrap().recent_artists();

        artist_list
            .bind_property(
                "n-items",
                &self.imp().artist_row_stack.get(),
                "visible-child-name",
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
                let artist_cell = ArtistCell::new(
                    item, cache,
                    // Only displaying at most 30 artists, so we can be the one to fetch externally
                    true
                );
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
        let library = self.imp().library.upgrade().unwrap().clone();
        let song_list = library.recent_songs();

        self.imp()
            .song_list
            .bind_model(Some(&song_list), move |obj| {
                let song = obj.downcast_ref::<Song>().unwrap();
                let row = SongRow::new(Some(cache.clone()), None);
                // Manually bind attributes here
                row.set_name(song.get_name());
                row.set_quality_grade(song.get_quality_grade());
                row.set_first_attrib_icon_name(Some("library-music-symbolic"));
                row.set_first_attrib_text(song.get_album_title());
                row.set_second_attrib_icon_name(Some("music-artist-symbolic"));
                row.set_second_attrib_text(song.get_artist_tag());
                row.set_third_attrib_icon_name(Some("recent-symbolic"));
                row.set_third_attrib_text(song.get_last_played_desc().as_deref());

                let end_widget = RowAddButtons::new(&library);
                end_widget.set_song(Some(song));
                row.set_end_widget(Some(&end_widget.into()));

                row.on_bind(song);
                row.into()
            });
    }

    async fn init_recent(&self, refresh: bool) -> ClientResult<()> {
        let stack = self.imp().stack.get();
        stack.show_spinner();
        let library = self.imp().library.upgrade().unwrap();
        let res = library.init_recent(refresh).await;
        if library.recent_songs().n_items() > 0 {
            stack.show_content();
        } else {
            stack.show_placeholder();
        }
        res
    }
}

impl LazyInit for RecentView {
    fn populate(&self) {
        glib::spawn_future_local(clone!(#[weak(rename_to = this)] self, async move {
            if let Err(e) = this.init_recent(false).await {
                dbg!(e);
            }
        }));
    }
}
