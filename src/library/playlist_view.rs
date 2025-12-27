use adw::prelude::*;
use adw::subclass::prelude::*;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{Properties, WeakRef, clone, subclass::Signal};
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
    glib::{self, closure_local},
};
use mpd::Subsystem;
use std::{cell::Cell, cmp::Ordering, ops::Deref, rc::Rc};
use std::{cell::OnceCell, sync::OnceLock};

use super::Library;
use crate::{
    cache::Cache,
    client::{ClientState, ConnectionState},
    common::INode,
    library::PlaylistContentView,
    library::playlist_row::PlaylistRow,
    utils::{g_cmp_str_options, settings_manager},
    window::EuphonicaWindow,
};

// Playlist view implementation
mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate, Properties, Default)]
    #[properties(wrapper_type = super::PlaylistView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/playlist-view.ui")]
    pub struct PlaylistView {
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
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,

        // Content
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub content_view: TemplateChild<PlaylistContentView>,

        // Search & filter models
        pub search_filter: gtk::StringFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: WeakRef<Library>,
        pub cache: OnceCell<Rc<Cache>>,
        #[property(get, set)]
        pub collapsed: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistView {
        const NAME: &'static str = "EuphonicaPlaylistView";
        type Type = super::PlaylistView;
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
    impl ObjectImpl for PlaylistView {
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

            // Setup sorting
            let settings = settings_manager();
            let state = settings.child("state").child("playlistview");
            let library_settings = settings.child("library");
            self.sorter.set_sort_func(clone!(
                #[strong]
                library_settings,
                #[strong]
                state,
                move |obj1, obj2| {
                    let inode1 = obj1
                        .downcast_ref::<INode>()
                        .expect("Sort obj has to be a common::INode.");

                    let inode2 = obj2
                        .downcast_ref::<INode>()
                        .expect("Sort obj has to be a common::INode.");

                    // Should we sort ascending?
                    let asc = state.enum_("sort-direction") > 0;
                    // Should the sorting be case-sensitive, i.e. uppercase goes first?
                    let case_sensitive = library_settings.boolean("sort-case-sensitive");
                    // Should nulls be put first or last?
                    let nulls_first = library_settings.boolean("sort-nulls-first");

                    // Vary behaviour depending on sort menu
                    match state.enum_("sort-by") {
                        // Refer to the io.github.htkhiem.Euphonica.sortby enum the gschema
                        6 => {
                            // Filename
                            g_cmp_str_options(
                                Some(inode1.get_uri()),
                                Some(inode2.get_uri()),
                                nulls_first,
                                asc,
                                case_sensitive,
                            )
                        }
                        7 => {
                            // Last modified
                            g_cmp_str_options(
                                inode1.get_last_modified(),
                                inode2.get_last_modified(),
                                nulls_first,
                                asc,
                                case_sensitive,
                            )
                        }
                        _ => unreachable!(),
                    }
                }
            ));

            // Setup searching
            library_settings
                .bind("search-case-sensitive", &self.search_filter, "ignore-case")
                .get_only()
                .invert_boolean()
                .build();
            self.search_filter
                .set_expression(Some(&gtk::PropertyExpression::new(
                    INode::static_type(),
                    Option::<gtk::PropertyExpression>::None,
                    "uri",
                )));
            let search_entry = self.search_entry.get();
            search_entry
                .bind_property("text", &self.search_filter, "search")
                .build();
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

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_sort_by, action_sort_direction]);
            self.obj()
                .insert_action_group("playlist-view", Some(&actions));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    impl WidgetImpl for PlaylistView {}
}

glib::wrapper! {
    pub struct PlaylistView(ObjectSubclass<imp::PlaylistView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PlaylistView {
    fn default() -> Self {
        Self::new()
    }
}

impl PlaylistView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn pop(&self) {
        self.imp().nav_view.pop();
    }

    pub fn setup(
        &self,
        library: &Library,
        cache: Rc<Cache>,
        client_state: &ClientState,
        window: &EuphonicaWindow,
    ) {
        let content_view = self.imp().content_view.get();
        content_view.setup(library, cache.clone(), window);
        self.imp().content_page.connect_hidden(move |_| {
            content_view.unbind(true);
        });
        self.imp().library.set(Some(library));
        self.imp()
            .cache
            .set(cache.clone())
            .expect("Cannot init PlaylistView with cache controller");
        self.setup_listview();

        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |state, _| {
                    if state.get_connection_state() == ConnectionState::Connected {
                        // Newly-connected? Get all playlists.
                        glib::spawn_future_local(clone!(#[weak] this, async move {
                            this.imp().library.upgrade().unwrap().init_playlists(false).await;
                        }));
                    }
                }
            ),
        );

        client_state.connect_closure(
            "idle",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, subsys: glib::BoxedAnyObject| {
                    if subsys.borrow::<Subsystem>().deref() == &Subsystem::Playlist {
                        glib::spawn_future_local(clone!(#[weak] this, async move {
                            let library = this.imp().library.upgrade().unwrap();
                            // Reload playlists
                            library.init_playlists(true).await;
                            // Also try to reload content view too, if it's still bound to one.
                            // If its currently-bound playlist has just been deleted, don't rebind it.
                            // Instead, force-switch the nav view to this page.
                            let content_view = this.imp().content_view.get();
                            if let Some(playlist) = content_view.current_playlist() {
                                // If this change involves renaming the current playlist, ensure
                                // we have updated the playlist object to the new name BEFORE sending
                                // the actual rename command to MPD, such this this will always occur
                                // with the current name being the NEW one.
                                // Else, we will lose track of the current playlist.
                                let curr_name = playlist.get_name();
                                // Temporarily unbind
                                content_view.unbind(true);
                                let playlists = library.playlists();
                                if let Some(idx) = playlists.find_with_equal_func(move |obj| {
                                    obj.downcast_ref::<INode>().unwrap().get_name() == curr_name
                                }) {
                                    this.on_playlist_clicked(
                                        playlists
                                            .item(idx)
                                            .unwrap()
                                            .downcast_ref::<INode>()
                                            .unwrap(),
                                    );
                                } else {
                                    this.pop();
                                }
                            }
                        }));
                    }
                }
            ),
        );
    }

    pub fn on_playlist_clicked(&self, inode: &INode) {
        let content_view = self.imp().content_view.get();
        content_view.unbind(true);
        content_view.bind(inode.clone());
        if self
            .imp()
            .nav_view
            .visible_page_tag()
            .is_none_or(|tag| tag.as_str() != "content")
        {
            self.imp().nav_view.push_by_tag("content");
        }
    }

    fn setup_listview(&self) {
        let library = self.imp().library.upgrade().unwrap();
        let cache = self.imp().cache.get().unwrap();
        // client_state.connect_closure(
        //     "inode-basic-info-downloaded",
        //     false,
        //     closure_local!(
        //         #[strong(rename_to = this)]
        //         self,
        //         move |_: ClientState, inode: INode| {
        //             this.add_inode(inode);
        //         }
        //     )
        // );
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
        let playlists = library.playlists();
        let search_model = gtk::FilterListModel::new(
            Some(playlists.clone()),
            Some(self.imp().search_filter.clone()),
        );
        search_model.set_incremental(true);
        let sort_model =
            gtk::SortListModel::new(Some(search_model), Some(self.imp().sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        self.imp().list_view.set_model(Some(&sel_model));

        // Set up factory
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak]
            library,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let folder_row = PlaylistRow::new(false, library, item, cache);
                item.set_child(Some(&folder_row));
            }
        ));

        factory.connect_bind(move |_, list_item| {
            let item = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem");

            let playlist = item
                .item()
                .and_downcast::<INode>()
                .expect("The item has to be a common::INode.");

            let child: PlaylistRow = item
                .child()
                .and_downcast::<PlaylistRow>()
                .expect("The child has to be an `PlaylistRow`.");

            child.bind(&playlist);
        });

        // Set the factory of the list view
        self.imp().list_view.set_factory(Some(&factory));

        // Setup click action
        self.imp().list_view.connect_activate(clone!(
            #[weak(rename_to = this)]
            self,
            move |grid_view, position| {
                let model = grid_view.model().expect("The model has to exist.");
                let inode = model
                    .item(position)
                    .and_downcast::<INode>()
                    .expect("The item has to be a `common::INode`.");
                println!("Clicked on {:?}", &inode);
                this.on_playlist_clicked(&inode);
            }
        ));
    }
}
