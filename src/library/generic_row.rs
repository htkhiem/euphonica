use adw::prelude::*;
use glib::{Object, WeakRef, clone};
use gtk::{CompositeTemplate, gdk, glib, gio, subclass::prelude::*};
use std::cell::{OnceCell, RefCell};

use crate::{
    common::{INode, INodeType, SongInfoDialog},
    window::EuphonicaWindow,
};

use super::Library;

mod imp {
    use std::cell::Cell;

    use crate::common::INodeType;
    use glib::{ParamSpec, ParamSpecEnum, ParamSpecString};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/generic-row.ui")]
    pub struct GenericRow {
        #[template_child]
        pub thumbnail: TemplateChild<gtk::Image>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub last_modified: TemplateChild<gtk::Label>,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        pub uri: RefCell<String>,
        pub inode_type: Cell<INodeType>,
        // Only used while displaying a folder. For songs simply use a song MIME icon.
        // pub thumbnail_signal_id: RefCell<Option<SignalHandlerId>>,
        pub library: OnceCell<Library>,
        pub window: WeakRef<EuphonicaWindow>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for GenericRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaGenericRow";
        type Type = super::GenericRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for GenericRow {
        fn constructed(&self) {
            self.parent_constructed();

            self.replace_queue.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        async move {
                            if let Some(library) = this.library.get() {
                                match this.inode_type.get() {
                                    INodeType::Song => {
                                        if let Err(e) = library
                                            .queue_uri(
                                                this.uri.borrow().to_owned(),
                                                true,
                                                true,
                                                false,
                                            )
                                            .await
                                        {
                                            dbg!(e);
                                        }
                                    }
                                    INodeType::Folder => {
                                        if let Err(e) = library
                                            .queue_uri(
                                                this.uri.borrow().to_owned(),
                                                true,
                                                true,
                                                true,
                                            )
                                            .await
                                        {
                                            dbg!(e);
                                        }
                                    }
                                    INodeType::Playlist => {
                                        if let Err(e) = library
                                            .queue_playlist(
                                                this.title.label().to_string(),
                                                true,
                                                Some(0),
                                            )
                                            .await
                                        {
                                            dbg!(e);
                                        }
                                    }
                                    _ => unreachable!(),
                                }
                            }
                        }
                    ));
                }
            ));

            self.append_queue.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        async move {
                            if let Some(library) = this.library.get() {
                                match this.inode_type.get() {
                                    INodeType::Song => {
                                        if let Err(e) = library
                                            .queue_uri(
                                                this.uri.borrow().to_owned(),
                                                false,
                                                false,
                                                false,
                                            )
                                            .await
                                        {
                                            dbg!(e);
                                        }
                                    }
                                    INodeType::Folder => {
                                        if let Err(e) = library
                                            .queue_uri(
                                                this.uri.borrow().to_owned(),
                                                false,
                                                false,
                                                true,
                                            )
                                            .await
                                        {
                                            dbg!(e);
                                        }
                                    }
                                    INodeType::Playlist => {
                                        if let Err(e) = library
                                            .queue_playlist(
                                                this.title.label().to_string(),
                                                false,
                                                None,
                                            )
                                            .await
                                        {
                                            dbg!(e);
                                        }
                                    }
                                    _ => unreachable!(),
                                }
                            }
                        }
                    ));
                }
            ));
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").build(),
                    ParamSpecString::builder("last-modified").build(),
                    ParamSpecEnum::builder::<INodeType>("inode-type").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "uri" => self.uri.borrow().to_value(),
                "last-modified" => self.last_modified.label().to_value(),
                "inode-type" => self.inode_type.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "uri" => {
                    if let Ok(name) = value.get::<&str>() {
                        // Keep display name synchronised
                        if let Some(title) = name.split('/').next_back() {
                            self.title.set_label(title);
                        }
                        self.uri.replace(name.to_string());
                    } else {
                        self.title.set_label("");
                    }
                }
                "last-modified" => {
                    if let Ok(lm) = value.get::<&str>() {
                        self.last_modified.set_label(lm);
                    } else {
                        self.last_modified.set_label("");
                    }
                }
                "inode-type" => {
                    if let Ok(it) = value.get::<INodeType>() {
                        self.inode_type.replace(it);
                        self.thumbnail.set_icon_name(Some(it.icon_name()));
                        if it == INodeType::Folder
                            || it == INodeType::Song
                            || it == INodeType::Playlist
                        {
                            self.replace_queue.set_visible(true);
                            self.append_queue.set_visible(true);
                        } else {
                            self.replace_queue.set_visible(false);
                            self.append_queue.set_visible(false);
                        }
                        // TODO: playlists support
                    } else {
                        self.thumbnail
                            .set_icon_name(Some(INodeType::default().icon_name()));
                        self.replace_queue.set_visible(false);
                        self.append_queue.set_visible(false);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for GenericRow {}

    // Trait shared by all boxes
    impl BoxImpl for GenericRow {}
}

glib::wrapper! {
    pub struct GenericRow(ObjectSubclass<imp::GenericRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl GenericRow {
    pub fn new(library: Library, item: &gtk::ListItem, window: Option<&EuphonicaWindow>) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, item);

        // Right-click context menu. The inode type is expression-bound and may not be
        // set yet at this point, so the menu is only popped up for songs (checked on click).
        if let Some(window) = window {
            res.imp().window.set(Some(window));

            let song_info_action = gio::ActionEntry::builder("song-info")
                .activate(clone!(
                    #[weak]
                    res,
                    move |_, _, _| {
                        res.show_song_info();
                    }
                ))
                .build();
            let action_group = gio::SimpleActionGroup::new();
            action_group.add_action_entries([song_info_action]);
            res.insert_action_group("generic-row", Some(&action_group));

            let menu = gio::Menu::new();
            menu.append(Some("Song info"), Some("generic-row.song-info"));
            let context_menu = gtk::PopoverMenu::from_model(Some(&menu));
            // Adding the popover as a child anchors it to this row
            res.append(&context_menu);

            let click_ctl = gtk::GestureClick::new();
            click_ctl.set_button(gdk::BUTTON_SECONDARY);
            click_ctl.connect_released(clone!(
                #[weak]
                res,
                #[weak]
                context_menu,
                move |_, _, x, y| {
                    if res.imp().inode_type.get() != INodeType::Song {
                        return;
                    }
                    // Anchor the popover's arrow at the click position
                    context_menu.set_pointing_to(Some(&gdk::Rectangle::new(
                        x as i32,
                        y as i32,
                        1,
                        1,
                    )));
                    context_menu.popup();
                }
            ));
            // For touchscreen devices
            let long_press_ctl = gtk::GestureLongPress::new();
            long_press_ctl.connect_pressed(clone!(
                #[weak]
                res,
                #[weak]
                context_menu,
                move |_, x, y| {
                if res.imp().inode_type.get() != INodeType::Song {
                        return;
                    }
                    // Anchor the popover's arrow at the click position
                    context_menu.set_pointing_to(Some(&gdk::Rectangle::new(
                        x as i32,
                        y as i32,
                        1,
                        1,
                    )));
                    context_menu.popup();
            }));
            res.add_controller(click_ctl);
            res.add_controller(long_press_ctl);
        }

        res
    }

    /// Fetches the raw rust-mpd Song struct for this row's inode URI and opens a SongInfoDialog
    /// attached to the main window.
    pub fn show_song_info(&self) {
        // Only songs have raw MPD data to show
        if self.imp().inode_type.get() != INodeType::Song {
            return;
        }
        let window = match self.imp().window.upgrade() {
            Some(window) => window,
            None => return,
        };
        let library = match self.imp().library.get() {
            Some(library) => library.clone(),
            None => return,
        };
        let uri = self.imp().uri.borrow().to_string();
        glib::spawn_future_local(async move {
            match library.get_raw_mpd_song_by_uri(uri).await {
                Ok(Some(raw_song)) => {
                    let dialog = SongInfoDialog::from_mpd_song(raw_song, None);
                    dialog.present(Some(&window));
                }
                Ok(None) => {
                    // Song is no longer in the library, nothing to show
                }
                Err(e) => {
                    dbg!(e);
                }
            }
        });
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &gtk::ListItem) {
        let _ = self.imp().library.set(library);
        item.property_expression("item")
            .chain_property::<INode>("uri")
            .bind(self, "uri", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<INode>("last-modified")
            .bind(self, "last-modified", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<INode>("inode-type")
            .bind(self, "inode-type", gtk::Widget::NONE);

        // item
        //     .property_expression("item")
        //     .chain_property::<Song>("quality-grade")
        //     .bind(self, "quality-grade", gtk::Widget::NONE);
    }

    // fn update_thumbnail(&self, info: Option<&AlbumInfo>, cache: Rc<Cache>, schedule: bool) {
    //     if let Some(album) = info {
    //         // Should already have been downloaded by the album view
    //         if let Some(tex) = cache.load_cached_album_art(album, true, schedule) {
    //             self.imp().thumbnail.set_paintable(Some(&tex));
    //             return;
    //         }
    //     }
    //     self.imp().thumbnail.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
    // }

    // pub fn bind(&self, inode: &INode, _cache: Rc<Cache>) {
    //     Bind album art listener. Set once first (like sync_create)
    //     self.update_thumbnail(song.get_album(), cache.clone(), true);
    //     let thumbnail_binding = cache.get_cache_state().connect_closure(
    //         "album-art-downloaded",
    //         false,
    //         closure_local!(
    //             #[weak(rename_to = this)]
    //             self,
    //             #[strong]
    //             song,
    //             #[weak]
    //             cache,
    //             move |_: CacheState, folder_uri: String| {
    //                 if let Some(album) = song.get_album() {
    //                     if album.uri == folder_uri {
    //                         this.update_thumbnail(Some(album), cache, false);
    //                     }
    //                 }
    //             }
    //         )
    //     );
    //     self.imp().thumbnail_signal_id.replace(Some(thumbnail_binding));
    // }

    // pub fn unbind(&self) {
    //     if let Some(id) = self.imp().replace_queue_id.borrow_mut().take() {
    //         self.imp().replace_queue.disconnect(id);
    //     }
    //     if let Some(id) = self.imp().append_queue_id.borrow_mut().take() {
    //         self.imp().append_queue.disconnect(id);
    //     }
    // }
}
