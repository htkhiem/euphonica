use adw::subclass::prelude::*;
use glib::{Properties, clone};
use gtk::{CompositeTemplate, glib, prelude::*};
use std::cell::Cell;

use crate::{
    application::EuphonicaApplication,
    cache::Cache,
    client::state::StickersSupportLevel,
    common::{INode, ImageStack, View},
    utils,
    window::EuphonicaWindow,
};
use std::rc::Rc;

use super::SidebarButton;

/// Build the 16px rounded playlist cover ImageStack used as the prefix of
/// recent playlist buttons in the sidebar
fn playlist_cover_prefix() -> (ImageStack, gtk::Box) {
    let cover = ImageStack::new();
    cover.set_size(16);
    cover.set_is_thumbnail(true);
    let rounded_box = gtk::Box::builder()
        .halign(gtk::Align::Center)
        .valign(gtk::Align::Center)
        .overflow(gtk::Overflow::Hidden)
        .css_classes(["border-radius-6"])
        .build();
    rounded_box.append(&cover);
    (cover, rounded_box)
}

fn fetch_playlist_cover(cache: &Rc<Cache>, cover: ImageStack, name: &str, is_dynamic: bool) {
    let name = name.to_string();
    cover.show_spinner();
    glib::spawn_future_local(clone!(
        #[strong]
        cache,
        #[strong]
        cover,
        async move {
            match cache.get_playlist_cover(name, is_dynamic, true).await {
                Ok(Some(tex)) => cover.show(&tex),
                Ok(None) => cover.clear(),
                Err(e) => {
                    dbg!(e);
                    cover.clear();
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
        pub recent_btn: TemplateChild<SidebarButton>,
        #[template_child]
        pub albums_btn: TemplateChild<SidebarButton>,
        #[template_child]
        pub artists_btn: TemplateChild<SidebarButton>,
        #[template_child]
        pub folders_btn: TemplateChild<SidebarButton>,
        #[template_child]
        pub playlists_section: TemplateChild<gtk::Box>,
        #[template_child]
        pub playlists_btn: TemplateChild<SidebarButton>,
        #[template_child]
        pub recent_playlists: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub dyn_playlists_section: TemplateChild<gtk::Box>,
        #[template_child]
        pub dyn_playlists_btn: TemplateChild<SidebarButton>,
        #[template_child]
        pub recent_dyn_playlists: TemplateChild<gtk::ListBox>,
        #[template_child]
        pub queue_btn: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub queue_len: TemplateChild<gtk::Label>,
        #[property(get, set)]
        pub showing_queue_view: Cell<bool>,
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
    impl ObjectImpl for Sidebar {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }
    }

    impl WidgetImpl for Sidebar {}

    // Trait shared by all boxes
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

    // Dirty hack to remove the highlight effect on hover
    // (as the items themselves are toggle buttons already, there is no need
    // for the ListBoxRows to do this)
    pub fn hide_highlights(&self) {
        let settings = utils::settings_manager().child("ui");
        let recent_playlists_widget = self.imp().recent_playlists.get();
        let recent_dyn_playlists_widget = self.imp().recent_dyn_playlists.get();
        for idx in 0..settings.uint("recent-playlists-count") {
            if let Some(row) = recent_playlists_widget.row_at_index(idx as i32) {
                row.set_activatable(false);
            }
            if let Some(row) = recent_dyn_playlists_widget.row_at_index(idx as i32) {
                row.set_activatable(false);
            }
        }
    }

    pub fn setup(&self, win: &EuphonicaWindow, app: &EuphonicaApplication) {
        let settings = utils::settings_manager().child("ui");
        
        let stack = win.get_stack();
        let split_view = win.get_split_view();
        let player = app.get_player();
        let library = app.get_library();
        let client_state = app.get_client().get_client_state();
        stack
            .bind_property("visible-child-name", self, "showing-queue-view")
            .transform_to(|_, name: String| Some(name == "queue"))
            .sync_create()
            .build();

        let recent_btn = self.imp().recent_btn.get();
        recent_btn.set_active(true);

        // Prefix icons for the static sidebar buttons
        recent_btn.set_prefix(&gtk::Image::builder().icon_name("recent-symbolic").build());
        self.imp().albums_btn.set_prefix(
            &gtk::Image::builder()
                .icon_name("library-music-symbolic")
                .build(),
        );
        self.imp().artists_btn.set_prefix(
            &gtk::Image::builder()
                .icon_name("music-artist-symbolic")
                .build(),
        );
        self.imp().folders_btn.set_prefix(
            &gtk::Image::builder()
                .icon_name("folder-symbolic")
                .build(),
        );
        self.imp().playlists_btn.set_prefix(
            &gtk::Image::builder()
                .icon_name("playlist-symbolic")
                .build(),
        );
        self.imp().dyn_playlists_btn.set_prefix(
            &gtk::Image::builder()
                .icon_name("playlist-symbolic")
                .build(),
        );
        // Hook each button to their respective views
        recent_btn.connect_toggled(clone!(
            #[weak]
            stack,
            move |btn| {
                if btn.is_active() {
                    stack.set_visible_child_name("recent");
                }
            }
        ));

        self.imp().albums_btn.connect_toggled(clone!(
            #[weak]
            stack,
            move |btn| {
                if btn.is_active() {
                    stack.set_visible_child_name("albums");
                }
            }
        ));

        self.imp().artists_btn.connect_toggled(clone!(
            #[weak]
            stack,
            move |btn| {
                if btn.is_active() {
                    stack.set_visible_child_name("artists");
                }
            }
        ));

        self.imp().folders_btn.connect_toggled(clone!(
            #[weak]
            stack,
            move |btn| {
                if btn.is_active() {
                    stack.set_visible_child_name("folders");
                }
            }
        ));

        let playlist_view = win.get_playlist_view();
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

        self.imp().playlists_btn.connect_toggled(clone!(
            #[weak]
            stack,
            #[weak]
            playlist_view,
            move |btn| {
                if btn.is_active() {
                    playlist_view.pop();
                    if stack
                        .visible_child_name()
                        .is_none_or(|name| name.as_str() != "playlists")
                    {
                        stack.set_visible_child_name("playlists");
                    }
                }
            }
        ));

        let recent_playlists_widget = self.imp().recent_playlists.get();
        recent_playlists_widget.bind_model(
            Some(&recent_playlists_model),
            clone!(
                #[strong]
                cache,
                #[weak]
                stack,
                #[weak]
                playlist_view,
                #[weak]
                split_view,
                #[weak]
                recent_btn,
                #[upgrade_or]
                SidebarButton::new("ERROR").upcast::<gtk::Widget>(),
                move |obj| {
                    let playlist = obj.downcast_ref::<INode>().unwrap();
                    let btn = SidebarButton::new(playlist.get_uri());
                    let (cover, cover_box) = playlist_cover_prefix();
                    fetch_playlist_cover(&cache, cover, playlist.get_uri(), false);
                    btn.set_prefix(&cover_box);
                    btn.set_group(Some(&recent_btn));
                    btn.connect_toggled(clone!(
                        #[weak]
                        stack,
                        #[weak]
                        playlist_view,
                        #[weak]
                        split_view,
                        #[weak]
                        playlist,
                        move |btn| {
                            if btn.is_active() {
                                playlist_view.on_playlist_clicked(&playlist);
                                if stack
                                    .visible_child_name()
                                    .is_none_or(|name| name.as_str() != "playlists")
                                {
                                    stack.set_visible_child_name("playlists");
                                }
                                split_view.set_show_sidebar(!split_view.is_collapsed());
                            }
                        }
                    ));
                    btn.into()
                }
            ),
        );

        let dyn_playlist_view = win.get_dyn_playlist_view();
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

        self.imp().dyn_playlists_btn.connect_toggled(clone!(
            #[weak]
            stack,
            #[weak]
            dyn_playlist_view,
            move |btn| {
                if btn.is_active() {
                    dyn_playlist_view.pop();
                    stack.set_visible_child_name("dyn-playlists");
                }
            }
        ));

        let recent_dyn_playlists_widget = self.imp().recent_dyn_playlists.get();
        recent_dyn_playlists_widget.bind_model(
            Some(&recent_dyn_playlists_model),
            clone!(
                #[strong]
                cache,
                #[weak]
                stack,
                #[weak]
                dyn_playlist_view,
                #[weak]
                split_view,
                #[weak]
                recent_btn,
                #[upgrade_or]
                SidebarButton::new("ERROR").upcast::<gtk::Widget>(),
                move |obj| {
                    let playlist = obj.downcast_ref::<INode>().unwrap();
                    let btn = SidebarButton::new(playlist.get_uri());
                    let (cover, cover_box) = playlist_cover_prefix();
                    fetch_playlist_cover(&cache, cover, playlist.get_uri(), true);
                    btn.set_prefix(&cover_box);
                    btn.set_group(Some(&recent_btn));
                    btn.connect_toggled(clone!(
                        #[weak]
                        stack,
                        #[weak]
                        dyn_playlist_view,
                        #[weak]
                        split_view,
                        #[weak]
                        playlist,
                        move |btn| {
                            if btn.is_active() {
                                dyn_playlist_view.on_playlist_clicked(&playlist);
                                if stack
                                    .visible_child_name()
                                    .is_none_or(|name| name.as_str() != "dyn-playlists")
                                {
                                    stack.set_visible_child_name("dyn-playlists");
                                }
                                split_view.set_show_sidebar(!split_view.is_collapsed());
                            }
                        }
                    ));
                    btn.into()
                }
            ),
        );

        self.hide_highlights();
        playlists.connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _, _| {
                this.hide_highlights();
            }
        ));
        dyn_playlists.connect_items_changed(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _, _| {
                this.hide_highlights();
            }
        ));

        // Hide the list widget when there is no playlist at all to avoid
        // an unnecessary ~6px space after the Saved Playlists button
        recent_playlists_model
            .bind_property("n-items", &recent_playlists_widget, "visible")
            .transform_to(|_, len: u32| Some(len > 0))
            .sync_create()
            .build();
        recent_dyn_playlists_model
            .bind_property("n-items", &recent_dyn_playlists_widget, "visible")
            .transform_to(|_, len: u32| Some(len > 0))
            .sync_create()
            .build();

        client_state
            .bind_property(
                "supports-playlists",
                &self.imp().playlists_section.get(),
                "visible",
            )
            .sync_create()
            .build();

        // Dynamic playlists may rely on stickers.
        client_state
            .bind_property(
                "stickers-support-level",
                &self.imp().dyn_playlists_section.get(),
                "visible",
            )
            .transform_to(|_, lvl: StickersSupportLevel| Some(lvl == StickersSupportLevel::All))
            .sync_create()
            .build();

        self.imp().queue_btn.connect_toggled(clone!(
            #[weak]
            stack,
            move |btn| {
                if btn.is_active() {
                    stack.set_visible_child_name("queue");
                }
            }
        ));

        // Connect the raw "clicked" signals to show-content
        self.imp()
            .queue_btn
            .upcast_ref::<gtk::Button>()
            .connect_clicked(clone!(
                #[weak]
                split_view,
                move |_| split_view.set_show_sidebar(!split_view.is_collapsed())
            ));
        for btn in [
            &self.imp().recent_btn.get(),
            &self.imp().albums_btn.get(),
            &self.imp().artists_btn.get(),
            &self.imp().folders_btn.get(),
            &self.imp().playlists_btn.get(),
            &self.imp().dyn_playlists_btn.get(),
        ] {
            btn.upcast_ref::<gtk::ToggleButton>()
                .upcast_ref::<gtk::Button>()
                .connect_clicked(clone!(
                    #[weak]
                    split_view,
                    move |_| split_view.set_show_sidebar(!split_view.is_collapsed())
                ));
        }

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
        let mut view_to_show = View::try_from(state.enum_("startup-view") as u32).expect("Invalid startup-view setting value");
        if matches!(view_to_show, View::Last) {
            view_to_show = View::try_from(state.enum_("last-view") as u32).expect("Invalid last-view setting value");
        }
        self.set_view(view_to_show.as_str());
    }

    pub fn set_view(&self, view_name: &str) {
        match view_name {
            "albums" => self.imp().albums_btn.set_active(true),
            "artists" => self.imp().artists_btn.set_active(true),
            "folders" => self.imp().folders_btn.set_active(true),
            "playlists" => self.imp().playlists_btn.set_active(true),
            "dyn-playlists" => self.imp().dyn_playlists_btn.set_active(true),
            "recent" => self.imp().recent_btn.set_active(true),
            "queue" => self.imp().queue_btn.set_active(true),
            _ => {
                eprintln!("Unknown view: {}", view_name);
            }
        }
    }
}
