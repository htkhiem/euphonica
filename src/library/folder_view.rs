use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    gio,
    glib,
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
};
use std::{cell::Cell, cmp::Ordering, rc::Rc};

use glib::clone;

use super::{generic_row::GenericRow, Library};
use crate::{
    cache::Cache,
    common::{INode, INodeType},
    utils::{g_cmp_str_options, g_search_substr, settings_manager},
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
    use std::{cell::OnceCell, sync::OnceLock};

    use glib::{subclass::Signal, ParamSpec, ParamSpecBoolean};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Debug, CompositeTemplate)]
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
        pub sort_dir: TemplateChild<gtk::Image>,
        #[template_child]
        pub sort_dir_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub sort_mode: TemplateChild<gtk::DropDown>,
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
        pub search_filter: gtk::CustomFilter,
        pub sorter: gtk::CustomSorter,
        // Keep last length to optimise search
        // If search term is now longer, only further filter still-matching
        // items.
        // If search term is now shorter, only check non-matching items to see
        // if they now match.
        pub last_search_len: Cell<usize>,
        pub library: OnceCell<Library>,
        pub collapsed: Cell<bool>
    }

    impl Default for FolderView {
        fn default() -> Self {
            Self {
                show_sidebar: TemplateChild::default(),
                path_widget: TemplateChild::default(),
                back_btn: TemplateChild::default(),
                forward_btn: TemplateChild::default(),
                // Search & filter widgets
                sort_dir: TemplateChild::default(),
                sort_dir_btn: TemplateChild::default(),
                sort_mode: TemplateChild::default(),
                search_btn: TemplateChild::default(),
                search_bar: TemplateChild::default(),
                search_entry: TemplateChild::default(),
                // Content
                list_view: TemplateChild::default(),
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
                    if let Some(lib) = this.library.get() {
                        lib.folder_backward();
                    }
                }
            ));

            self.forward_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let Some(lib) = this.library.get() {
                        lib.folder_forward();
                    }
                }
            ));

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

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> =
                Lazy::new(|| vec![
                    ParamSpecBoolean::builder("collapsed").build()
                ]);
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
                _ => unimplemented!()
            }
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

    impl WidgetImpl for FolderView {}

    impl FolderView {
        pub fn update_nav_btn_sensitivity(&self) {
            let lib = self.library.get().unwrap();
            let curr_idx = lib.folder_curr_idx();
            let hist_len = lib.folder_history_len();
            println!("Curr idx: {}", curr_idx);
            println!("Hist len: {}", hist_len);
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
        @implements gio::ActionGroup, gio::ActionMap;
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

    fn library(&self) -> &Library {
        self.imp().library.get().unwrap()
    }

    pub fn setup(&self, library: Library, cache: Rc<Cache>) {
        self.imp()
            .library
            .set(library.clone())
            .expect("Cannot init FolderView with Library");
        self.setup_sort();
        self.setup_search();
        self.setup_listview(cache.clone(), library.clone());

        library
            .bind_property("folder-path", &self.imp().path_widget.get(), "label")
            .sync_create()
            .build();

        library.connect_notify_local(
            Some("folder-curr-idx"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    this.imp().update_nav_btn_sensitivity();
                }
            )
        );

        library.connect_notify_local(
            Some("folder-his-len"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _| {
                    this.imp().update_nav_btn_sensitivity();
                }
            )
        );
    }

    fn setup_sort(&self) {
        // Setup sort widget & actions
        let settings = settings_manager();
        let state = settings.child("state").child("folderview");
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
                    "filename" => Some(0.to_value()),
                    "last-modified" => Some(1.to_value()),
                    _ => unreachable!(),
                }
            })
            .set_mapping(|val, _| match val.get::<u32>().unwrap() {
                0 => Some("filename".to_variant()),
                1 => Some("last-modified".to_variant()),
                _ => unreachable!(),
            })
            .build();
        self.imp().sorter.set_sort_func(clone!(
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
                let inode = obj
                    .downcast_ref::<INode>()
                    .expect("Search obj has to be a common::INode.");

                let search_term = this.imp().search_entry.text();
                if search_term.is_empty() {
                    return true;
                }

                // Should the searching be case-sensitive?
                let case_sensitive = library_settings.boolean("search-case-sensitive");
                g_search_substr(Some(inode.get_uri()), &search_term, case_sensitive)
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
                self.library().navigate_to(name);
            }
        }
    }

    fn setup_listview(&self, cache: Rc<Cache>, library: Library) {
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
                let folder_row = GenericRow::new(library, &item);
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
                println!("Clicked on {:?}", &inode);
                this.on_inode_clicked(&inode);
            }
        ));
    }
}
