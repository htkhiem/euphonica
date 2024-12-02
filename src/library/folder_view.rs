use std::{
    rc::Rc,
    cell::Cell,
    cmp::Ordering
};
use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    gio,
    glib::{self, closure_local},
    CompositeTemplate,
    ListItem,
    SignalListItemFactory,
    SingleSelection
};

use glib::clone;

use super::{folder_row::FolderRow, Library};
use crate::{
    cache::Cache,
    client::{
        ClientState,
        ConnectionState,
        MpdWrapper
    }, common::{Album, INode, INodeType},
    utils::{g_cmp_str_options, g_search_substr, settings_manager}
};

// Folder view implementation
// Unlike other views, here we use a single ListView and update its contents as we browse.
// The back and forward buttons are disasbled/enabled as follows:
// - If history is empty: both are disabled.
// - If history is not empty:
//   - If curr_idx > 0: enable back button, then
//   - If curr_idx < history.len(): enable forward button too.
//
// Upon clicking on a folder:
// 1. Append name to history. If curr_idx is not equal to history.len() then we have to
// first truncate history be curr_idx long before appending, as with common forward button
// behaviour.
// 2. Increment curr_idx by one.
// 3. Send a request for folder contents with uri = current history up to curr_idx - 1
// joined using forward slashes (Linux-only).
// 4. Switch stack to showing loading animation.
// 5. Upon receiving a `folder-contents-downloaded` signal with matching URI, populate
// the inode list with its contents and switch the stack back to showing the ListView.
//
// Upon backing out:
// 1. Decrement curr_idx by one. Do not modify history.
// 2. Repeat steps 3-5 as above. If curr_idx is 0 after decrementing, simply use ""
// as path.
mod imp {
    use std::cell::{OnceCell, RefCell};

    use glib::{ParamSpec, ParamSpecString};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/org/euphonica/Euphonica/gtk/library/folder-view.ui")]
    pub struct FolderView {
        #[template_child]
        pub nav_view: TemplateChild<adw::NavigationView>,
        #[template_child]
        pub path_widget: TemplateChild<gtk::Label>,
        #[template_child]
        pub loading_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub back_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub forward_btn: TemplateChild<gtk::Button>,

        // Search & filter widgets
        #[template_child]
        pub sort_dir: TemplateChild<gtk::Image>,
        #[template_child]
        pub sort_mode: TemplateChild<gtk::Label>,
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
        pub list_view: TemplateChild<gtk::ListView>,

        // Files and folders
        pub history: RefCell<Vec<String>>,
        pub curr_idx: Cell<usize>,  // 0 means at root.
        pub inodes: gio::ListStore,
        // Search & filter models
        pub search_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: OnceCell<Library>
    }

    impl Default for FolderView {
        fn default() -> Self {
            Self {
                nav_view: TemplateChild::default(),
                path_widget: TemplateChild::default(),
                loading_stack: TemplateChild::default(),
                back_btn: TemplateChild::default(),
                forward_btn: TemplateChild::default(),
                // Search & filter widgets
                sort_dir: TemplateChild::default(),
                sort_mode: TemplateChild::default(),
                search_btn: TemplateChild::default(),
                search_mode: TemplateChild::default(),
                search_bar: TemplateChild::default(),
                search_entry: TemplateChild::default(),
                // Content
                history: RefCell::new(Vec::new()),
                curr_idx: Cell::new(0),
                list_view: TemplateChild::default(),
                inodes: gio::ListStore::new::<Album>(),
                // Search & filter models
                search_filter: gtk::CustomFilter::default(),
                sorter: gtk::CustomSorter::default(),
                // Keep last length to optimise search
                // If search term is now longer, only further filter still-matching
                // items.
                // If search term is now shorter, only check non-matching items to see
                // if they now match.
                last_search_len: Cell::new(0),
                library: OnceCell::new()
           }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for FolderView {
        const NAME: &'static str = "EuphonicaFolderView";
        type Type = super::FolderView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for FolderView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let _ = self.obj().bind_property("path", &self.path_widget.get(), "label");

            self.back_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.move_back()
            ));

            self.forward_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| this.move_forward()
            ));
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("path").read_only().build()
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "path" => obj.get_path().to_value(),
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for FolderView {}

    impl FolderView {
        fn update_nav_btn_sensitivity(&self) {
            let curr_idx = self.curr_idx.get();
            let hist_len = self.history.borrow().len();
            if curr_idx == 0 {
                self.back_btn.set_sensitive(false);
            }
            if curr_idx == hist_len {
                self.forward_btn.set_sensitive(false);
            }
        }
        pub fn move_back(&self) {
            let curr_idx = self.curr_idx.get();
            if curr_idx > 0 {
                self.curr_idx.replace(curr_idx - 1);
                self.obj().navigate();
                self.update_nav_btn_sensitivity();
            }
        }

        pub fn move_forward(&self) {
            let curr_idx = self.curr_idx.get();
            let hist_len = self.history.borrow().len();
            // Reminder: this index is 1-based because 0 means root (before first
            // element in history).
            if curr_idx < hist_len {
                self.curr_idx.replace(curr_idx + 1);
                self.obj().navigate();
                self.update_nav_btn_sensitivity();
            }
        }
    }
}

glib::wrapper! {
    pub struct FolderView(ObjectSubclass<imp::FolderView>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for FolderView {
    fn default() -> Self {
        Self::new()
    }
}

impl FolderView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn get_path(&self) -> String {
        let imp = self.imp();
        let history = imp.history.borrow();
        let curr_idx = imp.curr_idx.get();
        if history.len() > 0 && curr_idx > 0 {
            history[..(curr_idx - 1)].join("/")
        }
        else {
            // TODO: l10n
            "Library Root".to_string()
        }
    }

    pub fn navigate(&self) {
        self.imp().library.get().unwrap().get_folder_contents(&self.get_path());
        self.imp().loading_stack.set_visible_child_name("loading");
    }

    pub fn setup(&self, library: Library, cache: Rc<Cache>, client: Rc<MpdWrapper>) {
        let client_state = client.get_client_state();
        self.imp().library.set(library.clone()).expect("Cannot init FolderView with Library");
        client_state.connect_notify_local(Some("connection-state"), clone!(
            #[weak(rename_to = this)]
            self,
            move |state, _| {
                if state.get_connection_state() == ConnectionState::Connected {
                    // Newly-connected? Reset path to ""
                    let _ = this.imp().history.replace(Vec::new());
                    let _ = this.imp().curr_idx.replace(0);
                    this.imp().library.get().unwrap().get_folder_contents("");
                }
            }
        ));
        self.setup_sort();
        self.setup_search();
        self.setup_listview(client_state.clone(), cache.clone(), library.clone());
    }

    fn setup_sort(&self) {
        // TODO: use albumsort & albumartistsort tags where available
        // Setup sort widget & actions
        let settings = settings_manager();
        let state = settings.child("state").child("folderview");
        let library_settings = settings.child("library");
        let actions = gio::SimpleActionGroup::new();
        actions.add_action(
            &state.create_action("sort-by")
        );
        actions.add_action(
            &state.create_action("sort-direction")
        );
        self.insert_action_group("folderview", Some(&actions));
        let sort_dir = self.imp().sort_dir.get();
        state
            .bind(
                "sort-direction",
                &sort_dir,
                "icon-name"
            )
            .get_only()
            .mapping(|dir, _| {
                match dir.get::<String>().unwrap().as_ref() {
                    "asc" => Some("view-sort-ascending-symbolic".to_value()),
                    _ => Some("view-sort-descending-symbolic".to_value())
                }
            })
            .build();
        let sort_mode = self.imp().sort_mode.get();
        state
            .bind(
                "sort-by",
                &sort_mode,
                "label",
            )
            .get_only()
            .mapping(|val, _| {
                // TODO: i18n
                match val.get::<String>().unwrap().as_ref() {
                    "filename" => Some("Name".to_value()),
                    "last-modified" => Some("Last modified".to_value()),
                    _ => unreachable!()
                }
            })
            .build();
        self.imp().sorter.set_sort_func(
            clone!(
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
                        // Refer to the org.euphonica.Euphonica.sortby enum the gschema
                        6 => {
                            // Filename
                            g_cmp_str_options(
                                Some(inode1.get_uri()),
                                Some(inode2.get_uri()),
                                nulls_first,
                                asc,
                                case_sensitive
                            )
                        }
                        7 => {
                            // Last modified
                            g_cmp_str_options(
                                inode1.get_last_modified(),
                                inode2.get_last_modified(),
                                nulls_first,
                                asc,
                                case_sensitive
                            )
                        }
                        _ => unreachable!()
                    }
                }
            )
        );

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
            )
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
            )
        );
    }

    fn setup_search(&self) {
        let settings = settings_manager();
        let library_settings = settings.child("library");
        // Set up search filter
        self.imp().search_filter.set_filter_func(
            clone!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                library_settings,
                #[upgrade_or]
                true,
                move |obj| {
                    let inode = obj
                        .downcast_ref::<INode>()
                        .expect("Search obj has to be a common::INode.");

                    let search_term = this.imp().search_entry.text();
                    if search_term.is_empty() {
                        return true;
                    }

                    // Should the searching be case-sensitive?
                    let case_sensitive = library_settings.boolean("search-case-sensitive");
                    g_search_substr(
                        Some(inode.get_uri()),
                        &search_term,
                        case_sensitive
                    )
                }
            )
        );

        // Connect search entry to filter. Filter will later be put in GtkSearchModel.
        // That GtkSearchModel will listen to the filter's changed signal.
        let search_entry = self.imp().search_entry.get();
        search_entry.connect_search_changed(
            clone!(
                #[weak(rename_to = this)]
                self,
                move |entry| {
                    let text = entry.text();
                    let new_len = text.len();
                    let old_len = this.imp().last_search_len.replace(new_len);
                    match new_len.cmp(&old_len) {
                        Ordering::Greater => {
                            this.imp().search_filter.changed(gtk::FilterChange::MoreStrict);
                        }
                        Ordering::Less => {
                            this.imp().search_filter.changed(gtk::FilterChange::LessStrict);
                        }
                        Ordering::Equal => {
                            this.imp().search_filter.changed(gtk::FilterChange::Different);
                        }
                    }
                }
            )
        );

        let search_mode = self.imp().search_mode.get();
        search_mode.connect_notify_local(
            Some("selected"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    println!("Changed search mode");
                    this.imp().search_filter.changed(gtk::FilterChange::Different);
                }
            )
        );
    }

    pub fn on_inode_clicked(&self, inode: &INode) {
        // - Upon receiving click signal, get the list item at the indicated activate index.
        // - Extract inode from that list item.
        // - If this inode is a folder:
        //   - If we're not currently at the head of the history, truncate history to be curr_idx long.
        //   - Append inode name to CWD
        //   - Increment curr_idx
        //   - Send an lsinfo query with the newly-updated URI.
        //   - Switch to loading page.
        // - Else: do nothing (adding songs and playlists are done with buttons to the right of each row).
        if let Some(name) = inode.get_name() {
            if inode.get_info().inode_type == INodeType::Folder {
                {
                    let curr_idx = self.imp().curr_idx.get();
                    let mut history = self.imp().history.borrow_mut();
                    let hist_len = history.len();
                    if curr_idx < hist_len {
                        history.truncate(curr_idx);
                    }
                    history.push(name.to_owned());
                }
                self.imp().move_forward();
            }
        }
    }

    fn setup_listview(&self, client_state: ClientState, cache: Rc<Cache>, library: Library) {
        client_state.connect_closure(
            "inode-basic-info-downloaded",
            false,
            closure_local!(
                #[strong(rename_to = this)]
                self,
                move |_: ClientState, inode: INode| {
                    this.add_inode(inode);
                }
            )
        );
        // Setup search bar
        let search_bar = self.imp().search_bar.get();
        let search_entry = self.imp().search_entry.get();
        search_bar.connect_entry(&search_entry);

        let search_btn = self.imp().search_btn.get();
        search_btn
            .bind_property(
                "active",
                &search_bar,
                "search-mode-enabled"
            )
            .sync_create()
            .build();

        // Chain search & sort. Put sort after search to reduce number of sort items.
        let search_model = gtk::FilterListModel::new(Some(self.imp().inodes.clone()), Some(self.imp().search_filter.clone()));
        search_model.set_incremental(true);
        let sort_model = gtk::SortListModel::new(Some(search_model), Some(self.imp().sorter.clone()));
        sort_model.set_incremental(true);
        let sel_model = SingleSelection::new(Some(sort_model));

        self.imp().list_view.set_model(Some(&sel_model));

        // Set up factory
        let factory = SignalListItemFactory::new();

        // Create an empty `INodeCell` during setup
        factory.connect_setup(
            clone!(
                #[weak]
                library,
                move |_, list_item| {
                    let item = list_item
                        .downcast_ref::<ListItem>()
                        .expect("Needs to be ListItem");
                    let folder_row = FolderRow::new(library, &item);
                    item.set_child(Some(&folder_row));
                }
            )
        );

        // factory.connect_teardown(
        //     move |_, list_item| {
        //         // Get `INodeCell` from `ListItem` (the UI widget)
        //         let child: Option<FolderRow> = list_item
        //             .downcast_ref::<ListItem>()
        //             .expect("Needs to be ListItem")
        //             .child()
        //             .and_downcast::<FolderRow>();
        //         if let Some(c) = child {
        //             c.teardown();
        //         }
        //     }
        // );

        // Tell factory how to bind `INodeCell` to one of our INode GObjects.
        // If this cell is being bound to an inode, that means it might be displayed.
        // As such, we'll also make it listen to the cache controller for any new
        // inode art downloads. This ensures we will never have to iterate through
        // the entire grid to update inode arts (only visible or nearly visible cells
        // will be updated, thus yielding a constant update cost).
        factory.connect_bind(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                // Get `INode` from `ListItem` (that is, the data side)
                let item: INode = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .item()
                    .and_downcast::<INode>()
                    .expect("The item has to be a common::INode.");

                // Get `FolderRow` from `ListItem` (the UI widget)
                let child: FolderRow = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .child()
                    .and_downcast::<FolderRow>()
                    .expect("The child has to be an `FolderRow`.");
                child.bind(&item, cache);
            }
        ));


        // When cell goes out of sight, unbind from item to allow reuse with another.
        // Remember to also unset the thumbnail widget's texture to potentially free it from memory.
        factory.connect_unbind(
            move |_, list_item| {
                // Get `FolderRow` from `ListItem` (the UI widget)
                let child: FolderRow = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .child()
                    .and_downcast::<FolderRow>()
                    .expect("The child has to be an `FolderRow`.");
                child.unbind();
            }
        );

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
                this.on_inode_clicked(&inode);
            })
        );
    }

    fn add_inode(&self, inode: INode) {
        self.imp().inodes.append(&inode);
        // self.imp().inode_count.set_label(&self.imp().inode_list.n_items().to_string());
    }

    pub fn clear(&self) {
        self.imp().inodes.remove_all();
    }
}