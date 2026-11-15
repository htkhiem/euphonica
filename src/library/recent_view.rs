use std::cell::OnceCell;
use std::cell::RefCell;
use std::rc::Rc;
use std::{cell::Cell, sync::OnceLock};

use adw::prelude::*;
use adw::subclass::prelude::*;
use chrono::DateTime;
use chrono::Local;
use chrono::Timelike;
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection, gio,
    glib::{self},
};

use glib::{Properties, SignalHandlerId, WeakRef, clone, closure_local, subclass::Signal};

use super::{AlbumCell, ArtistCell, Library};
use crate::common::FadingScrolledWindow;
use crate::library::recommendations;
use crate::{
    cache::Cache,
    client::Result as ClientResult,
    common::{
        Album, Artist, ContentStack, RowAddButtons, RowDoublePressAction, Song, SongRow,
        marquee::MarqueeWrapMode,
    },
    player::Player,
    utils::LazyInit,
    window::EuphonicaWindow,
};
use derivative::Derivative;
use mpd::Stats;
mod imp {

    use super::*;

    #[derive(Debug, CompositeTemplate, Properties, Derivative)]
    #[derivative(Default)]
    #[properties(wrapper_type = super::RecentView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/recent-view.ui")]
    pub struct RecentView {
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,

        // Greetings
        #[template_child]
        pub greeting: TemplateChild<gtk::Label>,
        #[template_child]
        pub albums_badge: TemplateChild<gtk::Button>,
        #[template_child]
        pub albums_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub artists_badge: TemplateChild<gtk::Button>,
        #[template_child]
        pub artists_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub songs_badge: TemplateChild<gtk::Button>,
        #[template_child]
        pub songs_count: TemplateChild<gtk::Label>,

        // Rediscover
        #[template_child]
        pub refresh_rediscover: TemplateChild<gtk::Button>,
        #[template_child]
        pub rediscover_content_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub rediscover_albums: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub rediscover_artists: TemplateChild<adw::WrapBox>,
        // Keep its content alive
        pub rediscover_albums_list: RefCell<Vec<Album>>,
        pub rediscover_artists_list: RefCell<Vec<Artist>>,

        // Recent
        #[template_child]
        pub recent_content_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub clear_history: TemplateChild<gtk::Button>,
        #[template_child]
        pub recent_albums: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub recent_artists: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub recent_songs: TemplateChild<gtk::ListBox>,
        // Keep its content alive
        pub recent_albums_list: RefCell<Vec<Album>>,
        pub recent_artists_list: RefCell<Vec<Artist>>,
        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub recent_songs_list: gio::ListStore,

        pub library: WeakRef<Library>,
        pub cache: OnceCell<Rc<Cache>>,
        pub player: WeakRef<Player>,
        pub history_changed_id: RefCell<Option<SignalHandlerId>>,

        #[property(get, set)]
        pub collapsed: Cell<bool>,

        pub initialized: Cell<bool>,
        pub window: WeakRef<EuphonicaWindow>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RecentView {
        const NAME: &'static str = "EuphonicaRecentView";
        type Type = super::RecentView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            // Let the builder know what a fadingscrolledwindow is
            // Previously we'd put it as a templatechild but this is cleaner
            FadingScrolledWindow::ensure_type();
            Self::bind_template(klass);
            klass.set_layout_manager_type::<gtk::BinLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for RecentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            if let Some(id) = self.history_changed_id.take()
                && let Some(player) = self.obj().imp().player.upgrade()
            {
                player.disconnect(id);
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

            // Rediscover section
            self.refresh_rediscover.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |btn| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        #[weak]
                        btn,
                        async move {
                            btn.set_sensitive(false);
                            this.obj().refresh_rediscover().await;
                            btn.set_sensitive(true);
                        }
                    ));
                }
            ));

            // Recent section
            self.clear_history.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |btn| {
                    glib::spawn_future_local(clone!(
                        #[weak]
                        this,
                        #[weak]
                        btn,
                        async move {
                            btn.set_sensitive(false);
                            if let Err(e) =
                                this.library.upgrade().unwrap().clear_history().await
                            {
                                dbg!(e);
                            } else {
                                this.obj().on_history_changed();
                            }
                            btn.set_sensitive(true);
                        }
                    ));
                }
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    impl WidgetImpl for RecentView {}
}

glib::wrapper! {
    pub struct RecentView(ObjectSubclass<imp::RecentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for RecentView {
    fn default() -> Self {
        Self::new()
    }
}

impl RecentView {
    pub fn new() -> Self {
        let res: Self = glib::Object::new();

        res
    }

    pub fn setup(
        &self,
        library: &Library,
        player: &Player,
        cache: Rc<Cache>,
        window: &EuphonicaWindow,
    ) {
        self.imp().library.set(Some(library));
        self.imp().player.set(Some(player));
        let _ = self.imp().cache.set(cache.clone());
        self.imp().window.set(Some(window));

        self.imp()
            .history_changed_id
            .replace(Some(player.connect_closure(
                "history-changed",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: Player| {
                        this.on_history_changed();
                    }
                ),
            )));
        self.setup_recent_songs(cache);
    }

    pub fn on_history_changed(&self) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                this.refresh_recent().await;
            }
        ));
    }

    async fn refresh_rediscover(&self) {
        if let (Some(library), Some(cache), Some(win)) = (
            self.imp().library.upgrade(),
            self.imp().cache.get(),
            self.imp().window.upgrade(),
        ) {
            self.imp().rediscover_content_stack.show_placeholder();
            let rediscover_albums = self.imp().rediscover_albums.get();
            let rediscover_artists = self.imp().rediscover_artists.get();
            rediscover_albums.remove_all();
            rediscover_artists.remove_all();

            if let Ok((albums, artists)) = library.get_rediscover_albums_and_artists().await {
                if albums.is_empty() && artists.is_empty() {
                    self.imp().rediscover_content_stack.show_placeholder();
                    return;
                }
                self.imp().rediscover_content_stack.show_spinner();
                for album in albums.iter() {
                    // RecentView has to use WrapBoxes for layout reasons. Since WrapBoxes don't do selection models like ListBoxes or GridViews we'll
                    // have to capture clicks ourselves by wrapping each AlbumCell inside a button. Sucks I know but on the bright side the ugly
                    // always-on highlight is gone.
                    let cell = AlbumCell::new_static(
                        album,
                        Some(&library),
                        cache.clone(),
                        Some(MarqueeWrapMode::Scroll),
                    );
                    let cell_btn = gtk::Button::builder()
                        .css_classes(["padding-0", "flat"])
                        .child(&cell)
                        .build();
                    cell_btn.connect_clicked(clone!(
                        #[weak]
                        win,
                        #[weak]
                        album,
                        move |_| {
                            win.goto_album(&album);
                        }
                    ));
                    rediscover_albums.append(&cell_btn);
                }
                let _ = self.imp().rediscover_albums_list.replace(albums);
                for artist in artists.iter() {
                    // RecentView has to use WrapBoxes for layout reasons. Since WrapBoxes don't do selection models like ListBoxes or GridViews we'll
                    // have to capture clicks ourselves by wrapping each AlbumCell inside a button. Sucks I know but on the bright side the ugly
                    // always-on highlight is gone.
                    let cell = ArtistCell::new_static(artist, cache.clone());
                    let cell_btn = gtk::Button::builder()
                        .css_classes(["padding-0", "flat"])
                        .child(&cell)
                        .build();
                    cell_btn.connect_clicked(clone!(
                        #[weak]
                        win,
                        #[weak]
                        artist,
                        move |_| {
                            win.goto_artist(&artist);
                        }
                    ));
                    rediscover_artists.append(&cell_btn);
                }
                let _ = self.imp().rediscover_artists_list.replace(artists);
                self.imp().rediscover_content_stack.show_content();
            } else {
                self.imp().rediscover_content_stack.show_placeholder();
            }
        }
    }

    async fn refresh_recent(&self) {
        if let (Some(library), Some(cache), Some(win)) = (
            self.imp().library.upgrade(),
            self.imp().cache.get(),
            self.imp().window.upgrade(),
        ) {
            self.imp().recent_content_stack.show_placeholder();
            let recent_albums = self.imp().recent_albums.get();
            let recent_artists = self.imp().recent_artists.get();
            let recent_songs = &self.imp().recent_songs_list;  // this one is a model :)
            recent_albums.remove_all();
            recent_artists.remove_all();
            recent_songs.remove_all();

            if let Ok((albums, artists, songs)) = library.get_recent().await {
                // If there are no songs then there is nothing else either.
                if songs.is_empty() {
                    self.imp().recent_content_stack.show_placeholder();
                    return;
                }
                self.imp().recent_content_stack.show_spinner();
                for album in albums.iter() {
                    // RecentView has to use WrapBoxes for layout reasons. Since WrapBoxes don't do selection models like ListBoxes or GridViews we'll
                    // have to capture clicks ourselves by wrapping each AlbumCell inside a button. Sucks I know but on the bright side the ugly
                    // always-on highlight is gone.
                    let cell = AlbumCell::new_static(
                        album,
                        Some(&library),
                        cache.clone(),
                        Some(MarqueeWrapMode::Scroll),
                    );
                    let cell_btn = gtk::Button::builder()
                        .css_classes(["padding-0", "flat"])
                        .child(&cell)
                        .build();
                    cell_btn.connect_clicked(clone!(
                        #[weak]
                        win,
                        #[weak]
                        album,
                        move |_| {
                            win.goto_album(&album);
                        }
                    ));
                    recent_albums.append(&cell_btn);
                }
                let _ = self.imp().recent_albums_list.replace(albums);
                for artist in artists.iter() {
                    // RecentView has to use WrapBoxes for layout reasons. Since WrapBoxes don't do selection models like ListBoxes or GridViews we'll
                    // have to capture clicks ourselves by wrapping each AlbumCell inside a button. Sucks I know but on the bright side the ugly
                    // always-on highlight is gone.
                    let cell = ArtistCell::new_static(&artist, cache.clone());
                    let cell_btn = gtk::Button::builder()
                        .css_classes(["padding-0", "flat"])
                        .child(&cell)
                        .build();
                    cell_btn.connect_clicked(clone!(
                        #[weak]
                        win,
                        #[weak]
                        artist,
                        move |_| {
                            win.goto_artist(&artist);
                        }
                    ));
                    recent_artists.append(&cell_btn);
                }
                let _ = self.imp().recent_artists_list.replace(artists);
                recent_songs.extend_from_slice(&songs);
                self.imp().recent_content_stack.show_content();
            }
        }
    }

    fn setup_recent_songs(&self, cache: Rc<Cache>) {
        let library = self.imp().library.upgrade().unwrap();
        let song_list = self.imp().recent_songs_list.clone();
        let window = self.imp().window.upgrade();

        self.imp()
            .recent_songs
            .bind_model(Some(&song_list), move |obj| {
                let song = obj.downcast_ref::<Song>().unwrap();
                let row = SongRow::new(Some(cache.clone()), None, window.as_ref(), Some(&library));
                // Manually bind attributes here
                row.set_name(song.get_name());
                row.set_quality_grade(song.get_quality_grade());
                row.set_first_attrib_text(song.get_album_title());
                row.set_second_attrib_text(Some(&song.get_artist_string()));
                row.set_duration(song.get_last_played_desc().as_deref());

                let end_widget = RowAddButtons::new(&library);
                end_widget.set_song(Some(song));
                row.set_end_widget(&end_widget.into());

                row.on_bind(song);
                row.into()
            });

        // Setup double-click action
        self.imp().recent_songs.connect_row_activated(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, row| {
                let idx = row.index() as u32;
                let action = RowDoublePressAction::from_settings();
                if let Some(window) = this.imp().window.upgrade() {
                    window.maybe_show_double_press_toast();
                }
                match action {
                    RowDoublePressAction::DoNothing => {}
                    RowDoublePressAction::Append
                    | RowDoublePressAction::AppendAndPlay
                    | RowDoublePressAction::Replace
                    | RowDoublePressAction::ReplaceAndPlay => {
                        let Some(library) = this.imp().library.upgrade() else {
                            return;
                        };
                        let song = this.imp().recent_songs_list
                            .item(idx)
                            .and_downcast::<Song>()
                            .expect("The item has to be a `common::Song`.");
                        let replace = matches!(
                            action,
                            RowDoublePressAction::Replace | RowDoublePressAction::ReplaceAndPlay
                        );
                        // Append-and-play starts playback at the appended song, which
                        // lands at the end of the (not-yet-extended) queue.
                        let play_from = match action {
                            RowDoublePressAction::AppendAndPlay => Some(library.queue_len()),
                            RowDoublePressAction::ReplaceAndPlay => Some(0),
                            _ => None,
                        };
                        glib::spawn_future_local(async move {
                            if let Err(e) = library.queue_songs(&[song], replace, play_from).await {
                                dbg!(e);
                            }
                        });
                    }
                    RowDoublePressAction::ReplaceViewAndPlay => {
                        let Some(library) = this.imp().library.upgrade() else {
                            return;
                        };
                        // Queue all songs in the view, in displayed order.
                        let store = &this.imp().recent_songs_list;
                        let songs: Vec<Song> = (0..store.n_items())
                            .map(|i| store.item(i).and_downcast::<Song>().unwrap())
                            .collect();
                        glib::spawn_future_local(async move {
                            if let Err(e) = library.queue_songs(&songs, true, Some(idx)).await {
                                dbg!(e);
                            }
                        });
                    }
                }
            }
        ));
    }
}

impl LazyInit for RecentView {
    fn populate(&self) {
        // Update greeting every time every time we navigate to it
        let greeting = self.imp().greeting.get();
        let hour = Local::now().hour();
        if hour >= 5 && hour < 12 {
            greeting.set_label("Good morning");
        } else if hour >= 12 && hour < 19 {
            greeting.set_label("Good afternoon");
        } else {
            greeting.set_label("Good evening");
        }
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let Some(l) = this.imp().library.upgrade() {
                    if let Ok(stats) = l.stats().await {
                        this.imp().albums_count.set_label(&stats.albums.to_string());
                        this.imp().artists_count.set_label(&stats.artists.to_string());
                        this.imp().songs_count.set_label(&stats.songs.to_string());
                    }
                }
                // Do this only once
                if !this.imp().initialized.replace(true) {
                    this.refresh_rediscover().await;
                    this.refresh_recent().await;
                }
            }
        ));
    }
}
