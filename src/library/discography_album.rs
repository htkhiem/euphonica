use derivative::Derivative;
use gio::{ActionEntry, Menu, SimpleActionGroup};
use glib::{Object, WeakRef, clone};
use gtk::{CompositeTemplate, gio, glib, prelude::*, subclass::prelude::*};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    EuphonicaWindow,
    cache::Cache,
    common::{
        Album, ContentStack, TEXTURE_LOAD_DELAY_MS, ImageStack, RowAddButtons, Song, SongRow,
    },
    utils::format_secs_as_duration,
};

use super::{Library, add_to_playlist::AddToPlaylistButton};

// Wrapper around the common row object to implement song thumbnail fetch logic.
mod imp {
    use crate::utils::settings_manager;

    use super::*;

    #[derive(Derivative, CompositeTemplate)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/discography-album.ui")]
    pub struct DiscographyAlbum {
        #[template_child]
        pub inner: TemplateChild<gtk::Box>,
        #[template_child]
        pub cover: TemplateChild<ImageStack>,
        #[template_child]
        pub title_btn: TemplateChild<gtk::Button>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,
        #[template_child]
        pub title_arrow: TemplateChild<gtk::Image>,
        #[template_child]
        pub collapse_content_btn: TemplateChild<gtk::Button>,

        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub replace_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub queue_split_button: TemplateChild<adw::SplitButton>,
        #[template_child]
        pub queue_split_button_content: TemplateChild<adw::ButtonContent>,
        #[template_child]
        pub add_to_playlist: TemplateChild<AddToPlaylistButton>,
        #[template_child]
        pub sel_all: TemplateChild<gtk::Button>,
        #[template_child]
        pub sel_none: TemplateChild<gtk::Button>,

        #[template_child]
        pub content_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub content_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListBox>,

        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        pub cache: OnceCell<Rc<Cache>>,
        pub library: WeakRef<Library>,
        pub album: OnceCell<Album>,
        // Handle to the texture loading process. Can be used to abort early if unmap() is called
        // in quick succession (such as the case described in the comment block above all this mess,
        // or during fast scrolling).
        pub texture_load_handle: RefCell<Option<glib::JoinHandle<()>>>,
        pub window: WeakRef<EuphonicaWindow>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for DiscographyAlbum {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaDiscographyAlbum";
        type Type = super::DiscographyAlbum;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.set_layout_manager_type::<gtk::BoxLayout>();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for DiscographyAlbum {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            // ListBox native selection: connect to selected-rows-changed signal
            let content = self.content.get();
            content.connect_selected_rows_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().on_selection_changed();
                }
            ));

            self.title_btn.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let (Some(album), Some(win)) = (this.album.get(), this.window.upgrade()) {
                        win.goto_album(album);
                    }
                }
            ));

            // Select-all / clear-selection buttons
            self.sel_all.connect_clicked(clone!(
                #[weak]
                content,
                move |_| {
                    content.select_all();
                }
            ));
            self.sel_none.connect_clicked(clone!(
                #[weak]
                content,
                move |_| {
                    content.unselect_all();
                }
            ));

            // Insert-queue action for the split button menu
            let obj = self.obj();
            let action_insert_queue = ActionEntry::builder("insert-queue")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(async move {
                            obj.queue_selected(false, false, true).await;
                        });
                    }
                ))
                .build();
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_insert_queue]);
            self.obj()
                .insert_action_group("discography-album", Some(&actions));

            self.content_revealer.set_reveal_child(
                !settings_manager()
                    .child("ui")
                    .boolean("artist-collapse-discography-albums-on-load"),
            );

            let revealer = self.content_revealer.get();
            revealer
                .bind_property(
                    "reveal-child",
                    &self.collapse_content_btn.get(),
                    "icon-name",
                )
                .transform_to(|_, is_revealed: bool| {
                    Some(
                        if is_revealed {
                            "down-symbolic"
                        } else {
                            "up-symbolic"
                        }
                        .to_value(),
                    )
                })
                .bidirectional()
                .sync_create()
                .build();

            self.collapse_content_btn.connect_clicked(move |_| {
                revealer.set_reveal_child(!revealer.is_child_revealed());
            });
        }
    }

    impl WidgetImpl for DiscographyAlbum {}

    impl BoxImpl for DiscographyAlbum {}
}

// Common row widget for displaying a single song, used across the UI.
glib::wrapper! {
    pub struct DiscographyAlbum(ObjectSubclass<imp::DiscographyAlbum>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl DiscographyAlbum {
    pub fn new(
        album: Option<Album>, // pass None for untagged songs
        songs: &[Song],
        cache: Rc<Cache>,
        library: &Library,
        window: Option<&EuphonicaWindow>
    ) -> Self {
        let res: Self = Object::builder().build();
        let _ = res.imp().cache.set(cache);
        res.imp().library.set(Some(library));
        res.imp().window.set(window);
        if let Some(album) = album {
            res.imp().title.set_label(album.get_title());
            let _ = res.imp().album.set(album);
        } else {
            // TODO: translations
            res.imp().title.set_label("Untagged");
            res.imp().cover.set_visible(false);
            res.imp().title_btn.set_sensitive(false);
            res.imp().title_arrow.set_visible(false);
        }

        res.imp().song_list.extend_from_slice(songs);
        if !songs.is_empty() {
            res.imp().content_stack.show_content();
        } else {
            res.imp().content_stack.show_placeholder();
        }

        // Set up AddToPlaylistButton with ListBox row selection.
        res.imp()
            .add_to_playlist
            .bind_listbox(library, &res.imp().content, &res.imp().song_list);

        // Replace queue button (play selected / all)
        res.imp().replace_queue.connect_clicked(clone!(
            #[weak(rename_to = this)]
            res,
            move |_| {
                glib::spawn_future_local(async move {
                    this.queue_selected(true, true, false).await;
                });
            }
        ));

        // Queue split button (queue selected / all)
        res.imp().queue_split_button.connect_clicked(clone!(
            #[weak(rename_to = this)]
            res,
            #[upgrade_or]
            (),
            move |_| {
                glib::spawn_future_local(async move {
                    this.queue_selected(false, false, false).await;
                });
            }
        ));

        // Set up ListBox — bind directly to song_list (no MultiSelection needed)
        res.imp().content.bind_model(
            Some(&res.imp().song_list),
            clone!(
                #[weak]
                library,
                #[upgrade_or]
                SongRow::new(None, None).into(),
                move |song_obj| {
                    let song = song_obj
                        .downcast_ref::<Song>()
                        .expect("Must be a common::Song");
                    let row = SongRow::new(None, None);
                    row.set_index_visible(true);
                    row.set_index(&song.get_track().to_string());
                    row.set_thumbnail_visible(false);

                    row.set_name(song.get_name());
                    row.set_first_attrib_icon_name(Some("music-artist-symbolic"));
                    row.set_first_attrib_text(song.get_artist_tag());

                    row.set_second_attrib_icon_name(Some("hourglass-symbolic"));
                    row.set_second_attrib_text(Some(&format_secs_as_duration(
                        song.get_duration() as f64
                    )));

                    row.set_quality_grade(song.get_quality_grade());
                    let end_widget = RowAddButtons::new(&library);
                    end_widget.set_song(Some(song));
                    row.set_end_widget(Some(&end_widget.into()));
                    row.into()
                }
            ),
        );

        // Set up dynamic texture loading/unloading.
        // Prev implementation was painfully clunky. I've since discovered the map() and unmap()
        // signals. These fire as soon as a widget comes within/out of 10px of the viewport,
        // i.e. almost ideal for texture loading.
        // HOWEVER, map() seems to ALWAYS FIRE ONCE upon widget creation (maybe due to every new
        // widget being created somewhere within the viewport, then moved out? or something to do
        // with filtering being iterative?). Afterwards for widgets that end up outside of the
        // viewport, unmap() will be fired. To avoid flooding the cache controller on startup we
        // add a short delay on the map() signal. If unmap() comes in quick succession then we
        // can just cancel the load before it happens.
        res.connect_map(|res| {
            res.load_cover();
        });

        res.connect_unmap(|res| {
            res.unload_cover();
        });

        res
    }

    fn library(&self) -> Option<Library> {
        self.imp().library.upgrade()
    }

    fn album(&self) -> Option<&Album> {
        self.imp().album.get()
    }

    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().queue_split_button.set_sensitive(!queuing);
    }

    fn selected_songs(&self) -> Vec<Song> {
        let content = self.imp().content.get();
        let n_sel = content.selected_rows().len();
        let store = &self.imp().song_list;
        let total = store.n_items() as i32;

        let mut songs: Vec<Song> = Vec::with_capacity(total as usize);
        if n_sel == 0 || n_sel as i32 == total {
            for idx in 0..total {
                songs.push(store.item(idx as u32).and_downcast::<Song>().unwrap());
            }
        } else {
            for row in content.selected_rows() {
                songs.push(
                    store
                        .item(row.index() as u32)
                        .and_downcast::<Song>()
                        .unwrap(),
                );
            }
        }
        songs
    }

    async fn queue_selected(&self, replace: bool, play: bool, next: bool) {
        if let Some(library) = self.library() {
            self.set_is_queuing(true);
            let content = self.imp().content.get();
            let n_sel = content.selected_rows().len();
            let store = &self.imp().song_list;
            let total = store.n_items() as i32;
            if next {
                // Handled specially
                if let Err(e) = library.insert_songs_next(&self.selected_songs()).await {
                    dbg!(e);
                }
            } else if let (true, Some(album)) = (n_sel == 0 || n_sel as i32 == total, self.album())
            {
                // If has album & all tracks are selected, use queue_album.
                if let Err(e) = library
                    .queue_album(album.clone(), replace, play, None)
                    .await
                {
                    dbg!(e);
                }
            } else {
                // Catch-all: manually queue each song.
                if let Err(e) = library
                    .queue_songs(&self.selected_songs(), replace, play)
                    .await
                {
                    dbg!(e);
                }
            }
            self.set_is_queuing(false);
        }
    }

    fn load_cover(&self) {
        if let Some(handle) = self.imp().texture_load_handle.take() {
            handle.abort();
        }
        let this = self.clone();
        let _ = self
            .imp()
            .texture_load_handle
            .replace(Some(glib::spawn_future_local(async move {
                // Wait first to give unmap() a chance to cancel this outright
                glib::timeout_future(TEXTURE_LOAD_DELAY_MS).await;
                if let Some(album) = this.album() {
                    match this
                        .imp()
                        .cache
                        .get()
                        .unwrap()
                        .clone()
                        .get_album_cover(album.get_info(), false)
                        .await
                    {
                        Ok(Some(tex)) => {
                            this.imp().cover.show(&tex);
                        }
                        Ok(None) => {
                            this.imp().cover.clear();
                        }
                        Err(e) => {
                            this.imp().cover.clear();
                            eprintln!(
                                "Failed to read cover for album `{}` (URI `{}`):\n{:?}",
                                album.get_title(),
                                album.get_folder_uri(),
                                e
                            );
                        }
                    }
                }
            })));
    }

    /// Unloads the album art to reduce memory pressure.
    /// Album content stays loaded though as unloading them screws up the content height and thus the scroll pos;
    /// they also don't cost much in perf or memory, unlike the album art.
    pub fn unload_cover(&self) {
        if let Some(handle) = self.imp().texture_load_handle.take() {
            handle.abort();
        }
        let _ = self.imp().cover.clear();
    }

    pub fn on_selection_changed(&self) {
        let content = self.imp().content.get();
        let n_sel = content.selected_rows().len();
        let total = self.imp().song_list.n_items() as i32;

        if n_sel == 0 || n_sel as i32 == total {
            self.imp().replace_queue_text.set_label("Play all");
            self.imp().queue_split_button_content.set_label("Queue all");
            let queue_split_menu = Menu::new();
            queue_split_menu.append(
                Some("Queue all next"),
                Some("discography-album.insert-queue"),
            );
            self.imp()
                .queue_split_button
                .set_menu_model(Some(&queue_split_menu));
        } else {
            self.imp()
                .replace_queue_text
                .set_label(format!("Play {n_sel}").as_str());
            self.imp()
                .queue_split_button_content
                .set_label(format!("Queue {n_sel}").as_str());
            let queue_split_menu = Menu::new();
            queue_split_menu.append(
                Some(format!("Queue {n_sel} next").as_str()),
                Some("discography-album.insert-queue"),
            );
            self.imp()
                .queue_split_button
                .set_menu_model(Some(&queue_split_menu));
        }
    }

    pub fn set_narrow(&self, narrow: bool) {
        // Kinda like Adwaita breakpoints but implemented ourselves to reduce nesting.
        if narrow {
            self.imp().inner.set_orientation(gtk::Orientation::Vertical);
        } else {
            self.imp()
                .inner
                .set_orientation(gtk::Orientation::Horizontal);
        }
    }
}
