use super::{DynamicPlaylistView, Library, artist_tag::ArtistTag};
use crate::{
    cache::{Cache, placeholders::ALBUMART_PLACEHOLDER, sqlite},
    common::{ContentView, DynamicPlaylist, Song, SongRow, dynamic_playlist::AutoRefresh},
    utils::{self, format_secs_as_duration, get_time_ago_desc},
    window::EuphonicaWindow,
};
use adw::prelude::*;
use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use derivative::Derivative;
use gio::{ActionEntry, SimpleActionGroup};
use glib::WeakRef;
use glib::{clone, closure_local};
use gtk::{CompositeTemplate, ListItem, SignalListItemFactory, gio, glib};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};
use time::OffsetDateTime;

mod imp {
    use crate::common::{INodeType, inode::INodeInfo};

    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(
        resource = "/io/github/htkhiem/Euphonica/gtk/library/dynamic-playlist-content-view.ui"
    )]
    pub struct DynamicPlaylistContentView {
        #[template_child]
        pub delete_dialog: TemplateChild<adw::AlertDialog>,
        #[template_child]
        pub inner: TemplateChild<ContentView>,
        #[template_child]
        pub cover: TemplateChild<gtk::Image>,

        #[template_child]
        pub title: TemplateChild<gtk::Label>,

        #[template_child]
        pub last_refreshed: TemplateChild<gtk::Label>,
        #[template_child]
        pub rule_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub track_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub runtime: TemplateChild<gtk::Label>,

        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub refresh_btn: TemplateChild<gtk::Button>,

        #[template_child]
        pub content_spinner: TemplateChild<gtk::Stack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListView>,

        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<ArtistTag>()"))]
        pub artist_tags: gio::ListStore,

        pub dp: RefCell<Option<DynamicPlaylist>>,
        pub library: WeakRef<Library>,
        pub outer: WeakRef<DynamicPlaylistView>,
        pub window: WeakRef<EuphonicaWindow>,
        pub cache: OnceCell<Rc<Cache>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for DynamicPlaylistContentView {
        const NAME: &'static str = "EuphonicaDynamicPlaylistContentView";
        type Type = super::DynamicPlaylistContentView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            // klass.set_css_name("albumview");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for DynamicPlaylistContentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            println!("Disposing DPC view");
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.content
                .set_model(Some(&gtk::NoSelection::new(Some(self.song_list.clone()))));

            self.refresh_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let Some(dp) = this.dp.borrow().as_ref().cloned() {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            this,
                            async move {
                                this.obj().force_refresh(dp).await;
                            }
                        ));
                    }
                }
            ));

            self.replace_queue.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        async move {
                            if let (Some(library), Some(dp)) =
                                (this.library.upgrade(), this.dp.borrow_mut().as_ref())
                            {
                                this.obj().set_is_queuing(true);
                                if let Err(e) = library.queue_cached_dynamic_playlist(dp.name.to_owned(), true, true).await {
                                    dbg!(e);
                                }
                                this.obj().set_is_queuing(false);
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
                            if let (Some(library), Some(dp)) =
                                (this.library.upgrade(), this.dp.borrow_mut().as_ref())
                            {
                                this.obj().set_is_queuing(true);
                                if let Err(e) = library.queue_cached_dynamic_playlist(dp.name.to_owned(), false, false).await {
                                    dbg!(e);
                                }
                                this.obj().set_is_queuing(false);
                            }
                        }
                    ));
                }
            ));

            // Ellipsis menu actions
            let action_delete = ActionEntry::builder("delete")
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _, _| {
                        let name: Option<String>;
                        {
                            name = this.dp.borrow().as_ref().map(|dp| dp.name.to_string());
                        }
                        if let (Some(name), Some(outer)) = (name, this.outer.upgrade()) {
                            let dialog = this.delete_dialog.get();
                            let obj = this.obj().clone();
                            glib::spawn_future_local(async move {
                                let res = dialog.choose_future(&obj).await;
                                if res == "delete" {
                                    outer.delete(&name).await;
                                }
                            });
                        }
                    }
                ))
                .build();

            let action_export_json = ActionEntry::builder("export-json")
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        let dp: Option<DynamicPlaylist>;
                        {
                            // Copy & end borrow
                            dp = this.dp.borrow().to_owned();
                        }
                        if let Some(dp) = dp {
                            let name = dp.name.to_string();
                            let (sender, receiver) = oneshot::channel();
                            utils::tokio_runtime().spawn(async move {
                                let maybe_files = SelectedFiles::save_file()
                                    .title("Export Dynamic Playlist")
                                    .modal(true)
                                    .current_name(Some(format!("{}.edp.json", &name).as_str()))
                                    .send()
                                    .await
                                    .expect("ashpd file open await failure")
                                    .response();

                                sender.send(
                                    match maybe_files {
                                        Ok(files) => {
                                            let uris = files.uris();
                                            if !uris.is_empty() {
                                                Some(uris[0].to_string())
                                            } else {
                                                None
                                            }
                                        }
                                        Err(err) => {
                                            dbg!(err);
                                            None
                                        }
                                    }
                                ).expect("Broken oneshot sender");

                            });
                            glib::spawn_future_local(async move {
                                if let Some(path) = receiver.await.expect("Broken oneshot receiver") {
                                    if !path.is_empty() {
                                        // Assume ashpd always return filesystem spec
                                        let filepath =
                                            urlencoding::decode(if path.starts_with("file://") {
                                                &path[7..]
                                            } else {
                                                &path
                                            })
                                            .expect("Path must be in UTF-8")
                                            .into_owned();
                                        utils::export_to_json(&dp, &filepath)
                                            .expect("Unable to write file");
                                    }
                                }
                            });
                        }
                    }
                ))
                .build();

            let action_save_mpd = ActionEntry::builder("save-mpd")
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            this,
                            async move {
                                if let (Some(dp), Some(library)) =
                                    (this.dp.borrow().as_ref(), this.library.upgrade())
                                {
                                    let window = this.window.upgrade().unwrap();
                                    match library.save_dynamic_playlist_state(dp.name.to_owned()).await {
                                        Ok(Some(fixed_name)) => {
                                            window.goto_playlist(
                                                &INodeInfo::new(&fixed_name, None, INodeType::Playlist).into(),
                                            );
                                        }
                                        Ok(None) => {
                                            // TODO: translations
                                            window.send_simple_toast("No songs to save as fixed playlist", 5);
                                        }
                                        Err(e) => {
                                            dbg!(e);
                                        }
                                    }
                                }
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_delete, action_save_mpd, action_export_json]);
            self.obj()
                .insert_action_group("dp-content-view", Some(&actions));
        }
    }

    impl WidgetImpl for DynamicPlaylistContentView {}

    impl DynamicPlaylistContentView {}
}

glib::wrapper! {
    pub struct DynamicPlaylistContentView(ObjectSubclass<imp::DynamicPlaylistContentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for DynamicPlaylistContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl DynamicPlaylistContentView {
    pub fn get_library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().append_queue.set_sensitive(!queuing);
    }

    pub fn setup(
        &self,
        outer: &DynamicPlaylistView,
        library: &Library,
        cache: Rc<Cache>,
        window: &EuphonicaWindow,
    ) {
        self.imp().library.set(Some(library));
        self.imp().outer.set(Some(outer));
        self.imp().window.set(Some(window));

        let edit_btn = self.imp().edit_btn.get();
        edit_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            outer,
            move |_| {
                if let Some(dp) = this.imp().dp.take() {
                    outer.edit_playlist(dp.clone());
                }
            }
        ));

        // Set up factory
        let factory = SignalListItemFactory::new();

        // For now don't show album arts as most of the time songs in the same
        // album will have the same embedded art anyway.
        factory.connect_setup(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache), None);
                row.set_index_visible(false);
                row.set_thumbnail_visible(true);
                item.property_expression("item")
                    .chain_property::<Song>("track")
                    .bind(&row, "index", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("name")
                    .bind(&row, "name", gtk::Widget::NONE);

                row.set_first_attrib_icon_name(Some("library-music-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("album")
                    .bind(&row, "first-attrib-text", gtk::Widget::NONE);

                row.set_second_attrib_icon_name(Some("music-artist-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("artist")
                    .bind(&row, "second-attrib-text", gtk::Widget::NONE);

                row.set_third_attrib_icon_name(Some("hourglass-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("duration")
                    .chain_closure::<String>(closure_local!(|_: Option<glib::Object>, dur: u64| {
                        format_secs_as_duration(dur as f64)
                    }))
                    .bind(&row, "third-attrib-text", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("quality-grade")
                    .bind(&row, "quality-grade", gtk::Widget::NONE);
                // No queue buttons here. We currently only support queuing the entire DP at once.
                item.set_child(Some(&row));
            }
        ));
        // Tell factory how to bind `AlbumSongRow` to one of our Album GObjects
        factory.connect_bind(move |_, list_item| {
            // Get `Song` from `ListItem` (that is, the data side)
            let item: Song = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Song>()
                .expect("The item has to be a common::Song.");

            // Get `SongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be an `SongRow`.");
            // Download album art
            child.on_bind(&item);
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `AlbumSongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be an `SongRow`.");
            child.on_unbind();
        });

        // Set the factory of the list view
        self.imp().content.set_factory(Some(&factory));

        self.imp()
            .cache
            .set(cache)
            .expect("DynamicPlaylistContentView cannot bind to cache");
    }

    #[inline]
    fn clear_cover(&self) {
        self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
    }

    async fn update_cover(&self, name: String) {
        self.clear_cover();
        match self.imp().cache.get().unwrap().get_playlist_cover(name, true, false).await {
            Ok(Some(tex)) => {
                self.imp().cover.set_paintable(Some(&tex));
            }
            Ok(None) => {}
            Err(e) => {dbg!(e);}
        }
    }

    #[inline]
    async fn get_cached(&self, name: String) {
        if let Some(library) = self.imp().library.upgrade() {
            // Block queue actions while refreshing
            self.set_is_queuing(true);
            let spinner = self.imp().content_spinner.get();
            if spinner.visible_child_name().unwrap() != "spinner" {
                spinner.set_visible_child_name("spinner");
            }
            self.imp().song_list.remove_all();
            // Fetch from scratch & update cache
            let res = library.get_dynamic_playlist_songs_cached(name).await;
            spinner.set_visible_child_name("content");
            match res {
                // TODO: add empty StatusPAge
                Ok(songs) => {
                    self.imp().song_list.extend_from_slice(&songs);
                    self.imp().last_refreshed.set_label(&get_time_ago_desc(
                        OffsetDateTime::now_utc().unix_timestamp(),
                    ));
                }
                Err(e) => {
                    dbg!(e);
                }
            }
            self.set_is_queuing(false);
        }
    }

    #[inline]
    async fn force_refresh(&self, dp: DynamicPlaylist) {
        if let Some(library) = self.imp().library.upgrade() {
            // Block queue actions while refreshing
            self.set_is_queuing(true);
            let spinner = self.imp().content_spinner.get();
            if spinner.visible_child_name().unwrap() != "spinner" {
                spinner.set_visible_child_name("spinner");
            }
            self.imp().song_list.remove_all();
            // Fetch from scratch & update cache
            let res = library.get_dynamic_playlist_songs(dp, true).await;
            spinner.set_visible_child_name("content");
            match res {
                // TODO: add empty StatusPAge
                Ok(songs) => {
                    self.imp().song_list.extend_from_slice(&songs);
                    self.imp().last_refreshed.set_label(&get_time_ago_desc(
                        OffsetDateTime::now_utc().unix_timestamp(),
                    ));
                }
                Err(e) => {
                    dbg!(e);
                }
            }
            self.set_is_queuing(false);
        }
    }

    pub async fn bind_by_name(&self, name: String) {
        self.update_cover(name.clone()).await;
        match gio::spawn_blocking(move || sqlite::get_dynamic_playlist_info(&name))
            .await
            .unwrap()
        {
            Ok(Some(dp)) => {
                self.imp().title.set_label(&dp.name);
                // If we've got a cached version & it's not time for autorefresh
                // yet, use it. Else resolve rules from scratch.
                if let Some(last_refresh) = dp.last_refresh {
                    // Check whether we need to perform an auto-refresh
                    if dp.auto_refresh != AutoRefresh::None
                        && OffsetDateTime::now_utc().unix_timestamp() - last_refresh
                        > match dp.auto_refresh {
                            AutoRefresh::None => i64::MAX,
                            AutoRefresh::Hourly => 3600,
                            AutoRefresh::Daily => 86400,
                            AutoRefresh::Weekly => 86400 * 7,
                            AutoRefresh::Monthly => 86400 * 30,
                            AutoRefresh::Yearly => 86400 * 365,
                        }
                    {
                        if let Some(window) = self.imp().window.upgrade() {
                            window.send_simple_toast("Auto-refreshing...", 3);
                        }
                        self.force_refresh(dp.clone()).await;
                    } else {
                        self.get_cached(dp.name.clone()).await;
                        self.imp()
                            .last_refreshed
                            .set_label(&get_time_ago_desc(last_refresh));
                    }
                } else {
                    self.force_refresh(dp.clone()).await;
                }
                self.imp()
                    .rule_count
                    .set_label(&(dp.rules.len() + dp.ordering.len()).to_string());
                self.imp().dp.replace(Some(dp));
            }
            other => {
                let _ = dbg!(other);
            }
        }
    }

    pub fn unbind(&self) {
        self.imp().song_list.remove_all();
        self.imp().title.set_label("");
        self.imp().dp.take();
        self.clear_cover();
    }
}
