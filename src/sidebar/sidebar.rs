use adw::subclass::prelude::*;
use glib::{clone, Properties};
use gtk::{glib, prelude::*, CompositeTemplate};

use crate::{application::EuphonicaApplication, common::INode, utils, window::EuphonicaWindow};

use super::SidebarButton;

mod imp {
    use std::cell::Cell;

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
        for idx in 0..settings.uint("recent-playlists-count") {
            if let Some(row) = recent_playlists_widget.row_at_index(idx as i32) {
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
        // Set default view. TODO: remember last view
        stack.set_visible_child_name("recent");
        stack
            .bind_property("visible-child-name", self, "showing-queue-view")
            .transform_to(|_, name: String| Some(name == "queue"))
            .sync_create()
            .build();

        let recent_btn = self.imp().recent_btn.get();
        recent_btn.set_active(true);
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

        let recent_playlists_widget = self.imp().recent_playlists.get();
        recent_playlists_widget.bind_model(
            Some(&recent_playlists_model),
            clone!(
                #[weak]
                stack,
                #[weak]
                playlist_view,
                #[weak]
                split_view,
                #[weak]
                recent_btn,
                #[upgrade_or]
                SidebarButton::new("ERROR", "dot-symbolic").upcast::<gtk::Widget>(),
                move |obj| {
                    let playlist = obj.downcast_ref::<INode>().unwrap();
                    let btn = SidebarButton::new(playlist.get_uri(), "dot-symbolic");
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
                                stack.set_visible_child_name("playlists");
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
            move |_, _, _, _| {this.hide_highlights();}
        ));

        // Hide the list widget when there is no playlist at all to avoid
        // an unnecessary ~6px space after the Saved Playlists button
        recent_playlists_model
            .bind_property("n-items", &recent_playlists_widget, "visible")
            .transform_to(|_, len: u32| Some(len > 0))
            .sync_create()
            .build();

        self.imp().playlists_btn.connect_toggled(clone!(
            #[weak]
            stack,
            #[weak]
            playlist_view,
            move |btn| {
                if btn.is_active() {
                    playlist_view.pop();
                    stack.set_visible_child_name("playlists");
                }
            }
        ));

        client_state
            .bind_property(
                "supports-playlists",
                &self.imp().playlists_section.get(),
                "visible",
            )
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
            &self.imp().albums_btn.get(),
            &self.imp().artists_btn.get(),
            &self.imp().folders_btn.get(),
            &self.imp().playlists_btn.get(),
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
            .queue()
            .bind_property("n-items", &self.imp().queue_len.get(), "label")
            .transform_to(|_, size: u32| Some(size.to_string()))
            .build();
    }

    pub fn set_view(&self, view_name: &str) {
        // TODO: something less dumb than this
        match view_name {
            "albums" => self.imp().albums_btn.set_active(true),
            "artists" => self.imp().artists_btn.set_active(true),
            "queue" => self.imp().queue_btn.set_active(true),
            _ => unimplemented!(),
        };
    }
}
