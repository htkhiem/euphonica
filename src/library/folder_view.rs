use super::{Library, generic_row::GenericRow};
use crate::{
    cache::Cache,
    common::{INode, INodeType},
    utils::{LazyInit, g_cmp_str_options, settings_manager},
};
use adw::prelude::*;
use adw::subclass::prelude::*;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{ParamSpec, ParamSpecBoolean, WeakRef, clone, subclass::Signal};
use gtk::{CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection, glib};
use once_cell::sync::Lazy;
use std::{cell::Cell, cmp::Ordering, rc::Rc, sync::OnceLock};

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
    use super::*;

    #[derive(Debug, CompositeTemplate, Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/folder-view.ui")]
    pub struct FolderView {
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,
        #[template_child]
        pub path_widget: TemplateChild<gtk::Label>,
        #[template_child]
        pub back_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub forward_btn: TemplateChild<gtk::Button>,

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
        pub collapsed: Cell<bool>,
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

            self.back_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    glib::spawn_future_local(clone!(
                        #[weak] this, async move {
                            if let Some(lib) = this.library.upgrade() {
                                lib.folder_backward().await;
                            }
                        }
                    ));
                }
            ));

            self.forward_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    glib::spawn_future_local(clone!(
                        #[weak] this, async move {
                            if let Some(lib) = this.library.upgrade() {
                                lib.folder_forward().await;
                            }
                        }
                    ));
                }
            ));

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
            let state = settings.child("state").child("folderview");
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
                        idx => {
                            dbg!(idx);
                            unreachable!()
                        }
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

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_sort_by, action_sort_direction]);
            self.obj()
                .insert_action_group("folder-view", Some(&actions));
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> =
                Lazy::new(|| vec![ParamSpecBoolean::builder("collapsed").build()]);
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "collapsed" => self.collapsed.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "collapsed" => {
                    if let Ok(new) = value.get::<bool>() {
                        let old = self.collapsed.replace(new);
                        if old != new {
                            self.obj().notify("collapsed");
                        }
                    }
                }
                _ => unimplemented!(),
            }
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    impl WidgetImpl for FolderView {}

    impl FolderView {
        pub fn update_nav_btn_sensitivity(&self) {
            let lib = self.library.upgrade().unwrap();
            let curr_idx = lib.folder_curr_idx();
            let hist_len = lib.folder_history_len();
            // println!("Curr idx: {}", curr_idx);
            // println!("Hist len: {}", hist_len);
            if curr_idx == 0 {
                self.back_btn.set_sensitive(false);
            } else {
                self.back_btn.set_sensitive(true);
            }
            if curr_idx == hist_len {
                self.forward_btn.set_sensitive(false);
            } else {
                self.forward_btn.set_sensitive(true);
            }
        }
    }
}

glib::wrapper! {
    pub struct FolderView(ObjectSubclass<imp::FolderView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FolderView {
    fn default() -> Self {
        Self::new()
    }
}

impl FolderView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    fn library(&self) -> Library {
        self.imp().library.upgrade().unwrap()
    }

    pub fn setup(&self, library: &Library, cache: Rc<Cache>) {
        self.imp().library.set(Some(library));
        self.setup_listview(cache.clone(), library.clone());

        library
            .bind_property("folder-path", &self.imp().path_widget.get(), "label")
            .sync_create()
            .build();

        self.imp().update_nav_btn_sensitivity();
        library.connect_notify_local(
            Some("folder-curr-idx"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    this.imp().update_nav_btn_sensitivity();
                }
            ),
        );
        library.connect_notify_local(
            Some("folder-his-len"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    this.imp().update_nav_btn_sensitivity();
                }
            ),
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
        glib::spawn_future_local(clone!(#[weak(rename_to = this)] self, #[weak] inode, async move {
            if let Some(name) = inode.get_name() {
                if inode.get_info().inode_type == INodeType::Folder {
                    this.library().navigate_to(&name).await;
                }
            }
        }));
    }

    fn setup_listview(&self, _cache: Rc<Cache>, library: Library) {
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
            Some(self.library().folder_inodes()),
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

        // Create an empty `INodeCell` during setup
        factory.connect_setup(clone!(
            #[weak]
            library,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let folder_row = GenericRow::new(library, item);
                item.set_child(Some(&folder_row));
            }
        ));

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
                this.on_inode_clicked(&inode);
            }
        ));
    }
}

impl LazyInit for FolderView {
    fn populate(&self) {
        if let Some(library) = self.imp().library.upgrade() {
            glib::spawn_future_local(clone!(#[weak] library, async move {
                library.get_folder_contents();
            }));
        }
    }
}
