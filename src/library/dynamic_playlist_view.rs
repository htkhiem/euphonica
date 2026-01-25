use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
    glib::{self, closure_local},
};
use std::{
    cell::{Cell, OnceCell},
    cmp::Ordering,
    rc::Rc,
    sync::OnceLock,
};

use glib::Properties;
use glib::WeakRef;
use glib::clone;
use glib::subclass::Signal;

use super::{DynamicPlaylistContentView, Library};
use crate::{
    cache::{Cache, sqlite},
    client::{ClientState, ConnectionState, Result as ClientResult},
    common::{DynamicPlaylist, INode, ContentStack},
    library::{DynamicPlaylistEditorView, playlist_row::PlaylistRow},
    utils::{g_cmp_str_options, settings_manager},
    window::EuphonicaWindow,
};

// DynamicPlaylist view implementation
mod imp {
    use ashpd::desktop::file_chooser::SelectedFiles;
    use gio::{ActionEntry, SimpleActionGroup};

    use crate::utils;

    use super::*;

    #[derive(Debug, CompositeTemplate, Properties, Default)]
    #[properties(wrapper_type = super::DynamicPlaylistView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/dynamic-playlist-view.ui")]
    pub struct DynamicPlaylistView {
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

        #[template_child]
        pub create_btn: TemplateChild<adw::SplitButton>,

        // Content
        #[template_child]
        pub stack: TemplateChild<ContentStack>,
        #[template_child]
        pub list_view: TemplateChild<gtk::ListView>,
        #[template_child]
        pub content_page: TemplateChild<adw::NavigationPage>,
        #[template_child]
        pub content_view: TemplateChild<DynamicPlaylistContentView>,

        // Editor view is created on demand.

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
        pub window: WeakRef<EuphonicaWindow>,
        #[property(get, set)]
        pub collapsed: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DynamicPlaylistView {
        const NAME: &'static str = "EuphonicaDynamicPlaylistView";
        type Type = super::DynamicPlaylistView;
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
    impl ObjectImpl for DynamicPlaylistView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            println!("Disposing DP view");
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

            // Setup sorting
            let settings = settings_manager();
            let state = settings.child("state").child("dynplaylistview");
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

            let action_import_json = ActionEntry::builder("import-json")
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _, _| {
                        let (sender, receiver) = async_channel::unbounded();
                        utils::tokio_runtime().spawn(async move {
                            let maybe_files = SelectedFiles::open_file()
                                .title("Import Dynamic Playlist")
                                .modal(true)
                                .multiple(true)
                                .send()
                                .await
                                .expect("ashpd file open await failure")
                                .response();

                            match maybe_files {
                                Ok(files) => {
                                    let uris = files.uris();
                                    if !uris.is_empty() {
                                        let _ = sender.send_blocking(uris.to_owned());
                                    } else {
                                        let _ = sender.send_blocking(vec![]);
                                    }
                                }
                                Err(err) => {
                                    dbg!(err);
                                }
                            }
                        });
                        glib::spawn_future_local(clone!(
                            #[weak] this,
                            async move {
                                use futures::prelude::*;
                                let mut receiver = std::pin::pin!(receiver);
                                if let Some(paths) = receiver.next().await {
                                    'outer: for path in paths.iter() {
                                        let str_path = path.to_string();
                                        // Assume ashpd always return filesystem spec
                                        let filepath = urlencoding::decode(if str_path.starts_with("file://") {
                                            &str_path[7..]
                                        } else {
                                            &str_path
                                        }).expect("Path must be in UTF-8").into_owned();
                                        match utils::import_from_json::<DynamicPlaylist>(&filepath) {
                                            Ok(dp) => {
                                                // TODO: add a "keep both" option that renames the incoming
                                                // playlist in a way that skips all existing ones.
                                                let obj = this.obj();
                                                let res = match sqlite::exists_dynamic_playlist(&dp.name) {
                                                    Ok(exists) => {
                                                        if exists {
                                                            // TODO: translatable
                                                            let diag = adw::AlertDialog::builder()
                                                                .heading("Playlist Exists")
                                                                .body(format!("A dynamic playlist named \"{}\" already exists. Would you like to overwrite it?", &dp.name))
                                                                .build();
                                                            diag.add_response("abort", "_Abort");
                                                            diag.add_response("skip", "_Skip");
                                                            diag.add_response("overwrite", "_Overwrite");
                                                            diag.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
                                                            match diag.choose_future(obj.as_ref()).await.to_string().as_str() {
                                                                "overwrite" => {
                                                                    sqlite::insert_dynamic_playlist(&dp, Some(&dp.name))
                                                                }
                                                                "abort" => {
                                                                    break 'outer;
                                                                }
                                                                _ => Ok(())
                                                            }
                                                        } else {
                                                            sqlite::insert_dynamic_playlist(&dp, None)
                                                        }
                                                    },
                                                    Err(e) => Err(e)
                                                };
                                                match res {
                                                    Ok(_) => {},
                                                    Err(e) => {
                                                        dbg!(e);
                                                        if let Some(window) = this.window.upgrade() {
                                                            window.send_simple_toast(&format!("Couldn't import {}", &filepath), 5);
                                                        }
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                dbg!(e);
                                                if let Some(window) = this.window.upgrade() {
                                                    window.send_simple_toast(&format!("Couldn't import {}", &filepath), 5);
                                                }
                                            }
                                        }
                                    }

                                    if let Err(e) = this.obj().init_dyn_playlists(true).await {dbg!(e);}
                                }
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_sort_by, action_sort_direction, action_import_json]);
            self.obj().insert_action_group("dp-view", Some(&actions));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    impl WidgetImpl for DynamicPlaylistView {}
}

glib::wrapper! {
    pub struct DynamicPlaylistView(ObjectSubclass<imp::DynamicPlaylistView>)
        @extends gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gio::ActionGroup, gio::ActionMap;
}

impl Default for DynamicPlaylistView {
    fn default() -> Self {
        Self::new()
    }
}

impl DynamicPlaylistView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn pop(&self) {
        self.imp().nav_view.pop();
    }

    pub async fn delete(&self, name: &str) {
        if let Some(library) = self.imp().library.upgrade() {
            self.imp().nav_view.pop();
            self.imp().content_view.unbind();
            match sqlite::delete_dynamic_playlist(name) {
                Ok(_) => {
                    if let Err(e) = library.init_dyn_playlists(true).await {dbg!(e);}
                }
                Err(err) => {
                    dbg!(err);
                }
            };
        }
    }

    async fn init_dyn_playlists(&self, refresh: bool) -> ClientResult<()> {
        let stack = self.imp().stack.get();
        let library = self.imp().library.upgrade().unwrap();
        stack.show_spinner();
        let res = library.init_dyn_playlists(refresh).await;
        if library.dyn_playlists().n_items() > 0 {
            stack.show_content();
        } else {
            stack.show_placeholder();
        }
        res
    }

    pub fn setup(
        &self,
        library: &Library,
        cache: Rc<Cache>,
        client_state: &ClientState,
        window: &EuphonicaWindow,
    ) {
        let content_view = self.imp().content_view.get();
        content_view.setup(self, library, cache.clone(), window);
        self.imp().content_page.connect_hidden(move |_| {
            content_view.unbind();
        });
        self.imp().library.set(Some(library));
        self.imp()
            .cache
            .set(cache.clone())
            .expect("Cannot init DynamicPlaylistView with cache controller");
        self.imp().window.set(Some(window));
        self.setup_listview();

        client_state.connect_notify_local(
            Some("connection-state"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |state, _| {
                    if state.connection_state() == ConnectionState::Connected {
                        glib::spawn_future_local(clone!(#[weak] this, async move {
                            this.init_dyn_playlists(false).await;
                        }));
                    }
                }
            ),
        );

        self.imp().create_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            library,
            #[weak]
            cache,
            #[weak]
            window,
            move |_| {
                let editor = DynamicPlaylistEditorView::default();
                editor.setup(&library, cache, &window);
                editor.connect_closure(
                    "exit-clicked",
                    false,
                    closure_local!(
                        #[weak]
                        this,
                        #[weak]
                        library,
                        move |_: DynamicPlaylistEditorView, should_refresh: bool| {
                            glib::spawn_future_local(clone!(
                                #[weak] this, async move {
                                if should_refresh {
                                    this.init_dyn_playlists(true).await;
                                }
                                this.imp().nav_view.pop();
                            }));
                        }
                    ),
                );

                this.imp().nav_view.push(
                    &adw::NavigationPage::builder()
                        .tag("editor")
                        .title("New Dynamic Playlist")
                        .child(&editor)
                        .build(),
                );
            }
        ));
    }

    pub fn edit_playlist(&self, dp: DynamicPlaylist) {
        let editor = DynamicPlaylistEditorView::default();
        let library = self.imp().library.upgrade().unwrap();
        editor.setup(
            &library,
            self.imp().cache.get().unwrap().clone(),
            &self.imp().window.upgrade().unwrap(),
        );
        editor.init(dp);
        editor.connect_closure(
            "exit-clicked",
            false,
            closure_local!(
                #[weak(rename_to = this)] self, #[weak] library,
                move |editor: DynamicPlaylistEditorView, should_refresh: bool| {
                    glib::spawn_future_local(clone!(
                        #[weak] this, async move {
                            let content_view = this.imp().content_view.get();
                            content_view.unbind();
                            this.imp().nav_view.pop();
                            content_view.bind_by_name(editor.get_current_name()).await;
                            if should_refresh {
                                this.init_dyn_playlists(true).await;
                            }
                        }
                    ));
                }
            ),
        );

        self.imp().nav_view.push(
            &adw::NavigationPage::builder()
                .tag("editor")
                .title("Edit Dynamic Playlist")
                .child(&editor)
                .build(),
        );
    }

    pub fn on_playlist_clicked(&self, inode: &INode) {
        let uri = inode.get_uri().to_owned();
        glib::spawn_future_local(clone!(#[weak(rename_to = this)] self, async move {
            let content_view = this.imp().content_view.get();
            content_view.unbind();
            // Switch view first before binding again, as binding involves awaiting
            // rule resolution & we'd like to use the content view to show a loading
            // spinner while that happens.
            if this
                .imp()
                .nav_view
                .visible_page_tag()
                .is_none_or(|tag| tag.as_str() != "content")
            {
                this.imp().nav_view.push_by_tag("content");
            }
            content_view.bind_by_name(uri).await;
        }));
    }

    fn setup_listview(&self) {
        let library = self.imp().library.upgrade().unwrap();
        let cache = self.imp().cache.get().unwrap();
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
        let playlists = library.dyn_playlists();
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
                let folder_row = PlaylistRow::new(true, library, item, cache);
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
                .expect("The child has to be an `DynamicPlaylistRow`.");

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
