use adw::{prelude::*, subclass::prelude::*};
use glib::{Properties, clone};
use gtk::{CompositeTemplate, glib};
use std::cell::Cell;

use crate::{
    application::EuphonicaApplication,
    cache::{Cache, placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER},
    client::state::StickersSupportLevel,
    common::{INode, View},
    utils,
    window::EuphonicaWindow,
};
use gtk::glib::WeakRef;
use std::cell::OnceCell;
use std::rc::Rc;

/// Fetch a playlist's cover texture and set it as the icon paintable of the
/// given sidebar item. While loading (or if the playlist has no cover), the
/// item keeps the albumart placeholder it was built with.
fn fetch_playlist_cover(cache: &Rc<Cache>, item: adw::SidebarItem, name: &str, is_dynamic: bool) {
    let name = name.to_string();
    glib::spawn_future_local(clone!(
        #[strong]
        cache,
        #[strong]
        item,
        async move {
            match cache.get_playlist_cover(name, is_dynamic, true).await {
                Ok(Some(tex)) => item.set_icon_paintable(Some(&tex)),
                Ok(None) => {}
                Err(e) => {
                    dbg!(e);
                }
            }
        }
    ));
}

mod imp {
    use super::*;

    #[derive(Debug, Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::Sidebar)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/sidebar.ui")]
    pub struct Sidebar {
        #[template_child]
        pub inner: TemplateChild<adw::Sidebar>,
        #[template_child]
        pub recent_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub albums_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub artists_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub folders_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub playlists_section: TemplateChild<adw::SidebarSection>,
        #[template_child]
        pub playlists_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub dyn_playlists_section: TemplateChild<adw::SidebarSection>,
        #[template_child]
        pub add_dyn_playlist_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub dyn_playlists_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub queue_btn: TemplateChild<adw::SidebarItem>,
        #[template_child]
        pub queue_len: TemplateChild<gtk::Label>,
        #[property(get, set)]
        pub showing_queue_view: Cell<bool>,
        pub win: WeakRef<EuphonicaWindow>,
        pub recent_dyn_playlists: OnceCell<gtk::SliceListModel>,
        pub recent_playlists: OnceCell<gtk::SliceListModel>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Sidebar {
        const NAME: &'static str = "EuphonicaSidebar";
        type Type = super::Sidebar;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for Sidebar {}

    impl WidgetImpl for Sidebar {}

    impl BoxImpl for Sidebar {}
}

glib::wrapper! {
    pub struct Sidebar(ObjectSubclass<imp::Sidebar>)
        @extends gtk::Box, gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl Default for Sidebar {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl Sidebar {
    pub fn new() -> Self {
        Self::default()
    }

    fn update_recent_playlists(
        &self,
        model: &gtk::SliceListModel,
        cache: &Rc<Cache>,
        dynamic: bool,
        raw_pos: u32,
        removed: u32,
        added: u32,
    ) {
        let section = if dynamic {
            self.imp().dyn_playlists_section.get()
        } else {
            self.imp().playlists_section.get()
        };
        // Add one as the first element in the section is the "All" button
        let pos = raw_pos + 1;
        // Remove first
        for _ in 0..removed {
            if let Some(item) = section.item(pos) {
                section.remove(&item);
            }
        }
        // Then add, moving the cursor along the way
        for cur in pos..pos + added {
            if let Some(playlist) = model.item(cur - 1).and_downcast::<INode>() {
                let item = adw::SidebarItem::builder()
                    .title(playlist.get_uri())
                    // Start with the placeholder texture; the fetched cover
                    // replaces it once the future resolves.
                    .icon_paintable(&*ALBUMART_THUMBNAIL_PLACEHOLDER)
                    .build();
                fetch_playlist_cover(cache, item.clone(), playlist.get_uri(), dynamic);
                section.insert(item, cur as i32);
            }
        }
    }

    fn on_selection_changed(&self) {
        let imp = self.imp();
        let win = imp.win.upgrade().unwrap();
        let sidebar = imp.inner.get();
        let idx = sidebar.selected();
        let split_view = win.get_split_view();
        let playlist_view = win.get_playlist_view();
        let dyn_playlist_view = win.get_dyn_playlist_view();
        let stack = win.get_stack();

        if let Some(item) = sidebar.item(idx) {
            split_view.set_show_sidebar(!split_view.is_collapsed());
            let section = item.section();
            // TODO: something cleaner than a long chain of if/else
            if item == imp.recent_btn.get() {
                stack.set_visible_child_name("recent");
            } else if item == imp.albums_btn.get() {
                stack.set_visible_child_name("albums");
            } else if item == imp.artists_btn.get() {
                stack.set_visible_child_name("artists");
            } else if item == imp.folders_btn.get() {
                stack.set_visible_child_name("folders");
            } else if item == imp.dyn_playlists_btn.get() {
                stack.set_visible_child_name("dyn-playlists");
                dyn_playlist_view.pop();
            } else if section
                .as_ref()
                .is_some_and(|s| s == &imp.dyn_playlists_section.get())
            {
                // Recent dyn playlists
                let model_idx = item.section_index() - 1; // offset by one since first element is always the "All" option
                if let Some(playlist) = imp
                    .recent_dyn_playlists
                    .get()
                    .unwrap()
                    .item(model_idx)
                    .and_downcast::<INode>()
                {
                    dyn_playlist_view.on_playlist_clicked(&playlist);
                    if stack
                        .visible_child_name()
                        .is_none_or(|name| name.as_str() != "dyn-playlists")
                    {
                        stack.set_visible_child_name("dyn-playlists");
                    }
                }
            } else if item == imp.playlists_btn.get() {
                stack.set_visible_child_name("playlists");
                playlist_view.pop();
            } else if section
                .as_ref()
                .is_some_and(|s| s == &imp.playlists_section.get())
            {
                // Recent playlists
                let model_idx = item.section_index() - 1; // offset by one since first element is always the "All" option
                if let Some(playlist) = imp
                    .recent_playlists
                    .get()
                    .unwrap()
                    .item(model_idx)
                    .and_downcast::<INode>()
                {
                    playlist_view.on_playlist_clicked(&playlist);
                    if stack
                        .visible_child_name()
                        .is_none_or(|name| name.as_str() != "playlists")
                    {
                        stack.set_visible_child_name("playlists");
                    }
                }
            } else if item == imp.queue_btn.get() {
                stack.set_visible_child_name("queue");
            }
        }
    }

    pub fn setup(&self, win: &EuphonicaWindow, app: &EuphonicaApplication) {
        let settings = utils::settings_manager().child("ui");
        let _ = self.imp().win.set(Some(win));

        let stack = win.get_stack();
        let player = app.get_player();
        let library = app.get_library();
        let client_state = app.get_client().get_client_state();
        stack
            .bind_property("visible-child-name", self, "showing-queue-view")
            .transform_to(|_, name: String| Some(name == "queue"))
            .sync_create()
            .build();

        let dyn_playlist_view = win.get_dyn_playlist_view();
        self.imp().add_dyn_playlist_btn.connect_clicked(clone!(
            #[weak]
            stack,
            #[weak]
            dyn_playlist_view,
            move |_| {
                if stack
                    .visible_child_name()
                    .is_none_or(|name| name.as_str() != "dyn-playlists")
                {
                    stack.set_visible_child_name("dyn-playlists");
                    dyn_playlist_view.pop();
                    dyn_playlist_view.new_playlist();
                }
            }
        ));

        // Hook each button to their respective views
        let inner = self.imp().inner.get();
        inner.connect_activated(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                this.on_selection_changed();
            }
        ));

        let playlists = library.playlists();
        let cache = app.get_cache();
        let recent_playlists_model = gtk::SliceListModel::new(
            Some(gtk::SortListModel::new(
                Some(playlists.clone()),
                Some(
                    gtk::StringSorter::builder()
                        .expression(gtk::PropertyExpression::new(
                            INode::static_type(),
                            Option::<gtk::PropertyExpression>::None,
                            "last-modified",
                        ))
                        .build(),
                ),
            )),
            0,
            5, // placeholder, will be bound to a GSettings key later
        );
        settings
            .bind("recent-playlists-count", &recent_playlists_model, "size")
            .build();
        recent_playlists_model.connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cache,
            move |model, pos, removed, added| {
                this.update_recent_playlists(model, &cache, false, pos, removed, added);
            }
        ));
        let _ = self.imp().recent_playlists.set(recent_playlists_model);

        // Hide entire playlist section when client doesn't have a playlist DB configured
        client_state
            .bind_property(
                "supports-playlists",
                &self.imp().playlists_btn.get(),
                "visible",
            )
            .sync_create()
            .build();

        let dyn_playlists = library.dyn_playlists();
        let recent_dyn_playlists_model = gtk::SliceListModel::new(
            Some(gtk::SortListModel::new(
                Some(dyn_playlists.clone()),
                Some(
                    gtk::StringSorter::builder()
                        .expression(gtk::PropertyExpression::new(
                            INode::static_type(),
                            Option::<gtk::PropertyExpression>::None,
                            "last-modified",
                        ))
                        .build(),
                ),
            )),
            0,
            5, // placeholder, will be bound to a GSettings key later
        );
        settings
            .bind(
                "recent-playlists-count",
                &recent_dyn_playlists_model,
                "size",
            )
            .build();
        recent_dyn_playlists_model.connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cache,
            move |model, pos, removed, added| {
                this.update_recent_playlists(model, &cache, true, pos, removed, added);
            }
        ));
        let _ = self
            .imp()
            .recent_dyn_playlists
            .set(recent_dyn_playlists_model);

        // Dynamic playlists may rely on stickers.
        client_state
            .bind_property(
                "stickers-support-level",
                &self.imp().dyn_playlists_btn.get(),
                "visible",
            )
            .transform_to(|_, lvl: StickersSupportLevel| Some(lvl == StickersSupportLevel::All))
            .sync_create()
            .build();

        client_state
            .bind_property(
                "stickers-support-level",
                &self.imp().add_dyn_playlist_btn.get(),
                "sensitive",
            )
            .transform_to(|_, lvl: StickersSupportLevel| Some(lvl == StickersSupportLevel::All))
            .sync_create()
            .build();

        player
            .bind_property("queue-len", &self.imp().queue_len.get(), "label")
            .transform_to(|_, size: u32| Some(size.to_string()))
            .sync_create()
            .build();
        // Set startup view.
        // If playlists or dynamic playlists were selected as startup view or was the last
        // view but are now not available, that view will still be displayed at first but will
        // be empty & can't be navigated back to once moved away.
        let state = utils::settings_manager().child("state");
        let mut view_to_show = View::try_from(state.enum_("startup-view") as u32)
            .expect("Invalid startup-view setting value");
        if matches!(view_to_show, View::Last) {
            view_to_show = View::try_from(state.enum_("last-view") as u32)
                .expect("Invalid last-view setting value");
        }
        self.set_view(view_to_show.as_str());
    }

    pub fn set_view(&self, view_name: &str) {
        let inner = self.imp().inner.get();
        if let Some(idx) = match view_name {
            "albums" => Some(self.imp().albums_btn.index()),
            "artists" => Some(self.imp().artists_btn.index()),
            "folders" => Some(self.imp().folders_btn.index()),
            "playlists" => Some(self.imp().playlists_btn.index()),
            "dyn-playlists" => Some(self.imp().dyn_playlists_btn.index()),
            "recent" => Some(self.imp().recent_btn.index()),
            "queue" => Some(self.imp().queue_btn.index()),
            _ => {
                eprintln!("Unknown view: {}", view_name);
                None
            }
        } {
            inner.set_selected(idx);
            self.on_selection_changed();
        }
    }
}
