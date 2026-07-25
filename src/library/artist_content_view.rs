use adw::prelude::*;
use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use asyncified::Asyncified;
use chrono::{Datelike, NaiveDate};
use derivative::Derivative;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{Binding, WeakRef, clone, closure_local, signal::SignalHandlerId, subclass::Signal};
use gtk::{CompositeTemplate, ListItem, SignalListItemFactory, gdk, gio, glib, prelude::*};
use rustc_hash::{FxHashMap, FxHashSet};
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
    sync::OnceLock,
};

use super::{Library, tag_button::TagButton};
use crate::{
    cache::{Cache, CacheState, Error as CacheError, placeholders::EMPTY_ARTIST_STRING},
    common::{
        Album, Artist, ContentStack, RowAddButtons, Song, SongRow, split_genre_tag,
    },
    library::{Tag, add_to_playlist::AddToPlaylistButton, discography_year::DiscographyYear},
    meta_providers::models::{Wiki, artist_type_to_string},
    utils::{self, format_secs_as_duration, tokio_runtime},
    window::EuphonicaWindow,
};

mod imp {

    use adw::prelude::AdwDialogExt;
use chrono::NaiveDate;
use musicbrainz_rs::entity::artist::ArtistType;

use crate::{
        common::FadingScrolledWindow, library::{TagsSection, discography_album::DiscographyAlbum}, meta_providers::models::{ArtistMeta, artist_type_to_index, index_to_artist_type}, utils::g_cmp_options,
    };

    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/artist-content-view.ui")]
    pub struct ArtistContentView {
        #[template_child]
        pub scrolled_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub multi_layout_view: TemplateChild<adw::MultiLayoutView>,

        #[template_child]
        pub avatar: TemplateChild<adw::Avatar>,
        #[template_child]
        pub name: TemplateChild<gtk::Label>,
        #[template_child]
        pub wiki_line: TemplateChild<adw::WrapBox>,
        #[template_child]
        pub artist_type: TemplateChild<gtk::Label>,
        #[template_child]
        pub years: TemplateChild<gtk::Label>,
        #[template_child]
        pub iso_flag: TemplateChild<gtk::Image>,

        #[template_child]
        pub bio_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub add_bio_btn: TemplateChild<gtk::Button>,  // now opens the metadata editor dialog
        #[template_child]
        pub bio_fader: TemplateChild<FadingScrolledWindow>,
        #[template_child]
        pub bio_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub bio_link: TemplateChild<gtk::LinkButton>,
        #[template_child]
        pub bio_attrib: TemplateChild<gtk::Label>,

        // Edit dialog
        #[template_child]
        pub edit_metadata_dialog: TemplateChild<adw::Dialog>,
        #[template_child]
        pub begin_field: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub end_field: TemplateChild<adw::EntryRow>,  // 0 sanity naming
        #[template_child]
        pub type_field: TemplateChild<adw::ComboRow>,
        #[template_child]
        pub country_code_field: TemplateChild<adw::EntryRow>,  // no input validation just yet in case a new country comes up
        #[template_child]
        pub mbid_field: TemplateChild<adw::EntryRow>,  // no input validation just yet in case a new country comes up
        #[template_child]
        pub bio_link_field: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub bio_desc_field: TemplateChild<gtk::TextView>,
        #[template_child]
        pub bio_attrib_field: TemplateChild<adw::EntryRow>,
        #[template_child]
        pub metadata_save: TemplateChild<gtk::Button>,
        #[template_child]
        pub metadata_cancel: TemplateChild<gtk::Button>,

        #[template_child]
        pub genres_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub genres_box: TemplateChild<adw::WrapBox>,

        #[template_child]
        pub tags_widget: TemplateChild<TagsSection>,

        #[template_child]
        pub track_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub release_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub mbid_row: TemplateChild<gtk::Box>,
        #[template_child]
        pub mbid: TemplateChild<gtk::LinkButton>,
        #[template_child]
        pub copy_mbid: TemplateChild<gtk::Button>,

        // #[template_child]
        // pub all_songs_btn: TemplateChild<gtk::ToggleButton>,

        // All songs sub-view
        #[template_child]
        pub song_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub song_subview: TemplateChild<gtk::ListView>,
        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        #[derivative(Default(value = "gtk::MultiSelection::new(Option::<gio::ListStore>::None)"))]
        pub song_sel_model: gtk::MultiSelection,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub replace_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub add_to_playlist: TemplateChild<AddToPlaylistButton>,
        #[template_child]
        pub sel_all: TemplateChild<gtk::Button>,
        #[template_child]
        pub sel_none: TemplateChild<gtk::Button>,

        // Discography sub-view
        #[template_child]
        pub discography_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub discography_subview: TemplateChild<gtk::ListBox>,

        pub library: WeakRef<Library>,
        pub artist: RefCell<Option<Artist>>,
        pub window: WeakRef<EuphonicaWindow>,
        pub bindings: RefCell<Vec<Binding>>,
        pub avatar_signal_id: RefCell<Option<SignalHandlerId>>,
        pub avatar_set_id: RefCell<Option<SignalHandlerId>>,
        pub avatar_cleared_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        #[derivative(Default(value = "Cell::new(true)"))]
        pub selecting_all: Cell<bool>, // Enables queuing all songs from this artist efficiently
        pub meta: RefCell<Option<ArtistMeta>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ArtistContentView {
        const NAME: &'static str = "EuphonicaArtistContentView";
        type Type = super::ArtistContentView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for ArtistContentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            if let Some(cache) = self.cache.get() {
                let state = cache.get_cache_state();
                if let Some(id) = self.avatar_set_id.take() {
                    state.disconnect(id);
                }
                if let Some(id) = self.avatar_cleared_id.take() {
                    state.disconnect(id);
                }
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.copy_mbid.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let Some(s) = this.mbid.label() {
                        gdk::Display::default()
                            .unwrap()
                            .clipboard()
                            .set_text(s.as_str());
                        if let Some(win) = this.window.upgrade() {
                            win.send_simple_toast("MusicBrainz ID copied to clipboard", 3);
                        }
                    }
                }
            ));

            self.metadata_cancel.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.edit_metadata_dialog.force_close();
                }
            ));

            self.metadata_save.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    if let (Some(cache), Some(artist)) =
                        (this.cache.get(), this.artist.borrow().as_ref())
                    {
                        let mut new_meta = this.meta.take().unwrap_or_default();
                        // Update short fields
                        let start_date = this.begin_field.text();
                        if start_date.is_empty() {
                            new_meta.begin_date = None;
                        } else {
                            new_meta.begin_date = NaiveDate::parse_from_str(&start_date, "%Y-%m-%d").ok();
                        }

                        let end_date = this.end_field.text();
                        if end_date.is_empty() {
                            new_meta.end_date = None;
                        } else {
                            new_meta.end_date = NaiveDate::parse_from_str(&end_date, "%Y-%m-%d").ok();
                        }

                        new_meta.artist_type = index_to_artist_type(this.type_field.selected());

                        let mbid = this.mbid_field.text();
                        if mbid.is_empty() {
                            new_meta.mbid = None;
                        } else {
                            new_meta.mbid = Some(mbid.to_string());
                        }

                        let country_code = this.country_code_field.text();
                        if country_code.is_empty() {
                            new_meta.country = None;
                        } else {
                            new_meta.country = Some(country_code.to_string());
                        }

                        // Update bio
                        let buf = this.bio_desc_field.buffer();
                        let mut bio = new_meta.bio.clone().unwrap_or_default();
                        bio.content = buf
                            .text(&buf.start_iter(), &buf.end_iter(), false)
                            .as_str()
                            .to_owned();
                        let maybe_link = this.bio_link_field.text();
                        if !maybe_link.is_empty() {
                            bio.url = Some(maybe_link.as_str().to_owned());
                        } else {
                            bio.url = None;
                        }
                        bio.attribution = this.bio_attrib_field.text().as_str().to_owned();
                        new_meta.bio = Some(bio);

                        // Might want to make this async?
                        if let Err(e) = cache.set_artist_meta(artist.get_info(), &new_meta) {
                            dbg!(e);
                        }

                        this.edit_metadata_dialog.force_close();
                        // Refresh UI too
                        glib::spawn_future_local(clone!(
                            #[weak]
                            this,
                            async move {
                                this.obj().update_meta(false).await;
                            }
                        ));
                    }
                }
            ));

            self.metadata_cancel.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.bio_stack.show_content();
                }
            ));

            self.tags_widget.set_on_tag_added(clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.obj().write_tags();
                }
            ));
            self.tags_widget.set_on_tag_removed(clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.obj().write_tags();
                }
            ));
            self.tags_widget.set_on_add_btn_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move || {
                    this.obj().write_tags();
                }
            ));

            // Set up song subview
            self.song_sel_model.set_model(Some(&self.song_list.clone()));
            self.song_subview.set_model(Some(&self.song_sel_model));

            // Change button labels depending on selection state
            self.song_sel_model.connect_selection_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, _| this.on_song_selection_changed()
            ));

            let song_sel_model = self.song_sel_model.clone();
            self.sel_all.connect_clicked(clone!(
                #[weak]
                song_sel_model,
                move |_| {
                    song_sel_model.select_all();
                }
            ));
            self.sel_none.connect_clicked(clone!(
                #[weak]
                song_sel_model,
                move |_| {
                    song_sel_model.unselect_all();
                }
            ));

            // Set up discography subview
            let discography_subview = self.discography_subview.get();
            discography_subview.set_sort_func(|row1, row2| {
                g_cmp_options(
                    row1.child()
                        .and_downcast::<DiscographyYear>()
                        .expect("Has to be a DiscographyYear")
                        .year()
                        .as_ref(),
                    row2.child()
                        .and_downcast::<DiscographyYear>()
                        .expect("Has to be a DiscographyYear")
                        .year()
                        .as_ref(),
                    false,
                    false,
                )
             });
            self.multi_layout_view
                .connect_layout_name_notify(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_| {
                        this.update_discography_layout();
                    }
                ));

            self.song_list
                .bind_property("n-items", &self.track_count.get(), "label")
                .sync_create()
                .build();

            // Edit actions
            let obj = self.obj();
            let action_set_avatar = ActionEntry::builder("set-avatar")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        let (sender, receiver) = oneshot::channel();
                        tokio_runtime().spawn(async move {
                            let maybe_files = SelectedFiles::open_file()
                                .title("Select a new avatar")
                                .modal(true)
                                .multiple(false)
                                .send()
                                .await
                                .expect("ashpd file open await failure")
                                .response();

                            let _ = sender.send(if let Ok(files) = maybe_files {
                                let uris = files.uris();
                                if !uris.is_empty() {
                                    Some(uris[0].to_string())
                                } else {
                                    None
                                }
                            } else {
                                None
                            });
                        });
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let Some(tag) = receiver.await.expect("Broken oneshot receiver")
                                {
                                    obj.set_avatar(tag);
                                }
                            }
                        ));
                    }
                ))
                .build();

            let action_edit_bio = ActionEntry::builder("edit-metadata")
                .activate(clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |_, _, _| {
                        let meta = this.meta.borrow().clone().unwrap_or_default();
                        // Init dialog fields with existing values
                        this.begin_field.set_text(meta.begin_date.map(|d| d.format("%Y-%m-%d").to_string()).as_deref().unwrap_or_default());
                        this.end_field.set_text(meta.end_date.map(|d| d.format("%Y-%m-%d").to_string()).as_deref().unwrap_or_default());
                        this.type_field.set_selected(artist_type_to_index(&meta.artist_type));
                        this.country_code_field.set_text(meta.country.as_deref().unwrap_or_default());
                        this.mbid_field.set_text(meta.mbid.as_deref().unwrap_or_default());
                        if let Some(bio) = meta.bio.as_ref() {
                            this.bio_desc_field.buffer().set_text(bio.content.as_ref());
                            this.bio_link_field.set_text(bio.url.as_deref().unwrap_or_default());
                            this.bio_attrib_field.set_text(bio.attribution.as_ref());
                        } else {
                            this.bio_desc_field.buffer().set_text("");
                            this.bio_link_field.set_text("");
                            this.bio_attrib_field.set_text("");
                        }
                        this.edit_metadata_dialog.present(this.window.upgrade().as_ref());
                    }
                ))
                .build();

            let action_clear_avatar = ActionEntry::builder("clear-avatar")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let (Some(artist), Some(cache)) =
                                    (obj.artist(), obj.imp().cache.get())
                                    && let Err(e) = cache
                                        .clear_artist_avatar(artist.get_name().to_owned(), true)
                                        .await
                                {
                                    obj.show_cache_error("Couldn't clear avatar", e);
                                }
                            }
                        ));
                    }
                ))
                .build();

            let action_refetch_metadata = ActionEntry::builder("refetch-metadata")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                obj.update_meta(true).await;
                                obj.schedule_avatar(true).await;
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([
                action_set_avatar,
                action_clear_avatar,
                action_refetch_metadata,
                action_edit_bio
            ]);
            self.obj()
                .insert_action_group("artist-content-view", Some(&actions));

            // Metadata editor 
            self.begin_field.connect_changed(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.validate_metadata_entries();
                }
            ));
            self.end_field.connect_changed(clone!(
                #[weak]
                obj,
                move |_| {
                    obj.validate_metadata_entries();
                }
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("album-clicked")
                        .param_types([Album::static_type()])
                        .build()
                ]
            })
        }
    }

    impl WidgetImpl for ArtistContentView {}

    impl ArtistContentView {
        pub fn on_song_selection_changed(&self) {
            let sel_model = &self.song_sel_model;
            // TODO: self can be slow, might consider redesigning
            let n_sel = sel_model.selection().size();
            if n_sel == 0 || (n_sel as u32) == sel_model.model().unwrap().n_items() {
                self.selecting_all.replace(true);
                self.replace_queue_text.set_label("Play all");
                self.append_queue_text.set_label("Queue all");
            } else {
                // TODO: l10n
                self.selecting_all.replace(false);
                self.replace_queue_text
                    .set_label(format!("Play {n_sel}").as_str());
                self.append_queue_text
                    .set_label(format!("Queue {n_sel}").as_str());
            }
        }

        pub fn update_discography_layout(&self) {
            let narrow = self.multi_layout_view
                .layout_name()
                .map(|name| name.to_string())
                .as_deref()
                .unwrap_or("")
                == "narrow";
            let mut i: i32 = 0;
            loop {
                if let Some(albums_box) = self.discography_subview
                    .row_at_index(i)
                    .map(|r| r.child())
                    .flatten()
                    .and_downcast::<DiscographyYear>()
                    .map(|y| y.albums_box())
                {
                    let mut j: i32 = 0;
                    loop {
                        if let Some(album) = albums_box
                            .row_at_index(j as i32)
                            .map(|r| r.child())
                            .flatten()
                            .and_downcast::<DiscographyAlbum>()
                        {
                            album.set_narrow(narrow);
                            j += 1;
                        } else {
                            break;
                        }
                    }
                    i += 1;
                } else {
                    break;
                }
            }
        }
    }
}

glib::wrapper! {
    pub struct ArtistContentView(ObjectSubclass<imp::ArtistContentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ArtistContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl ArtistContentView {
    fn show_cache_error(&self, prefix: &str, err: CacheError) {
        if let Some(win) = self.imp().window.upgrade() {
            win.send_simple_toast(&format!("{}: {}", prefix, dbg!(err).message()), 3);
        }
    }

    fn artist(&self) -> Option<Artist> {
        self.imp().artist.borrow().as_ref().cloned()
    }

    fn highlight_entry_err(entry: &adw::EntryRow, is_err: bool) {
        if is_err {
            if !entry.has_css_class("error") {
                entry.add_css_class("error");
            }
        } else if entry.has_css_class("error") {
            entry.remove_css_class("error");
        }
    }

    fn validate_metadata_entries(&self) {
        // Start and date fields
        let start_field_text = self.imp().begin_field.text();
        let start_field_valid = start_field_text.is_empty() || NaiveDate::parse_from_str(&start_field_text, "%Y-%m-%d").is_ok();
        let end_field_text = self.imp().end_field.text();
        let end_field_valid = end_field_text.is_empty() || NaiveDate::parse_from_str(&end_field_text, "%Y-%m-%d").is_ok();

        Self::highlight_entry_err(&self.imp().begin_field.get(), !start_field_valid);
        Self::highlight_entry_err(&self.imp().end_field.get(), !end_field_valid);

        let valid = start_field_valid && end_field_valid;

        self.imp().metadata_save.set_sensitive(valid);
    }

    fn set_is_queuing(&self, queuing: bool) {
        self.imp().replace_queue.set_sensitive(!queuing);
        self.imp().append_queue.set_sensitive(!queuing);
    }

    /// Write the current tag list to the database.
    pub fn write_tags(&self) {
        if let (Some(cache), Some(artist)) =
            (self.imp().cache.get(), self.imp().artist.borrow().as_ref())
        {
            let tags = self.imp().tags_widget.get_tags();
            let name = artist.get_info().name.clone();
            if let Err(e) = cache.set_artist_tags(&name, &tags) {
                dbg!(e);
            }
        }
        if let Some(library) = self.imp().library.upgrade() {
            glib::spawn_future_local(async move {
                let _ = library.refresh_artist_tags().await;
            });
        }
    }

    fn set_show_meta(&self, show: bool) {
        self.imp().wiki_line.set_visible(show);
        self.imp().bio_stack.set_visible(show);
    }

    #[inline]
    pub fn update_bio(&self, bio: Option<&Wiki>) {
        if let Some(bio) = bio {
            let bio_text = self.imp().bio_text.get();
            let bio_link = self.imp().bio_link.get();
            let bio_attrib = self.imp().bio_attrib.get();
            self.imp().bio_stack.show_content();
            bio_text.set_label(&bio.content);
            if let Some(url) = bio.url.as_ref() {
                bio_link.set_visible(true);
                bio_link.set_uri(url);
            } else {
                bio_link.set_visible(false);
                bio_link.set_uri("");
            }
            bio_attrib.set_visible(true);
            bio_attrib.set_label(&bio.attribution);
            self.imp().bio_stack.show_content();
        } else {
            self.imp().bio_stack.show_placeholder();
        }
    }

    async fn update_meta(&self, overwrite: bool) {
        if let Some(artist) = self.artist() {
            // If the current artist is the "untitled" one (i.e. for songs without an artist tag),
            // don't attempt to update metadata.
            if artist.get_name().is_empty() {
                self.set_show_meta(false);
                self.imp().tags_widget.remove_all(false);
            } else {
                self.set_show_meta(true);
                self.imp().bio_stack.show_spinner();
                self.imp().tags_widget.remove_all(true);
                let cache = self.imp().cache.get().unwrap().clone();
                let res = cache
                    .get_artist_meta(
                        artist.get_info(),
                        true,
                        overwrite,
                        self.imp().window.upgrade().as_ref(),
                    )
                    .await;
                match res {
                    Ok(Some(meta)) => {
                        let mut should_show_wiki_line = false;
                        // Populate wiki line:
                        // Populate begin-end years
                        if meta.begin_date.is_some() || meta.end_date.is_some() {
                            should_show_wiki_line = true;
                            self.imp().years.set_visible(true);
                            // Always show beginning year (use ? if unknown)

                            let mut s =
                                meta.begin_date.map_or("?".into(), |d| d.year().to_string());
                            if let Some(end_year) =
                                meta.end_date.map(|d| format!(" - {}", d.year()))
                            {
                                s.push_str(&end_year);
                            } else {
                                s.push_str(" - current");
                            }
                            self.imp().years.set_label(&s);
                        } else {
                            self.imp().years.set_visible(false);
                        }

                        // Populate nationality. For this we'll use the new GtkSvg.
                        if let Some(country) = meta.country.as_deref() {
                            if let Some(svg) = utils::new_gtksvg_from_datafile(&format!(
                                "flags/{}.svg",
                                country.to_lowercase()
                            ))
                            .await
                            {
                                should_show_wiki_line = true;
                                self.imp().iso_flag.set_paintable(Some(&svg));
                                self.imp().iso_flag.set_visible(true);
                            } else {
                                self.imp().iso_flag.set_visible(false);
                            }
                        } else {
                            self.imp().iso_flag.set_visible(false);
                        }
                        self.imp().wiki_line.set_visible(should_show_wiki_line);

                        // Populate metadata box
                        let artist_type_str = artist_type_to_string(&meta.artist_type);
                        self.imp()
                            .artist_type
                            .set_label(if !artist_type_str.is_empty() {
                                artist_type_str
                            } else {
                                // this is a display-centric placeholder so we'll do it at the view level;
                                // the common func in models.rs should just return an empty string.
                                "-"
                            });
                        if let Some(mbid) = meta.mbid.as_deref() {
                            self.imp().mbid_row.set_visible(true);
                            self.imp().mbid.set_label(mbid);
                            self.imp()
                                .mbid
                                .set_uri(&format!("https://musicbrainz.org/artist/{}", mbid));
                        } else {
                            self.imp().mbid_row.set_visible(false);
                        }

                        // Populate bio
                        self.update_bio(meta.bio.as_ref());

                        // Load tags from DB
                        let tags = cache.get_artist_tags(artist.get_name());
                        if let Ok(tags) = tags {
                            if tags.is_empty() {
                                self.imp().tags_widget.show_placeholder();
                            } else {
                                for tag in tags {
                                    self.imp().tags_widget.add_tag(&Tag::new(
                                        tag.name,
                                        tag.url,
                                        tag.count,
                                        true,
                                        tag.set_by_user,
                                    ));
                                }
                            }
                        } else {
                            self.imp().tags_widget.show_placeholder();
                        }

                        let _ = self.imp().meta.replace(Some(meta));
                    }
                    Ok(None) => {
                        self.set_show_meta(false);
                        let _ = self.imp().meta.take();
                    }
                    Err(e) => {
                        self.set_show_meta(false);
                        let _ = self.imp().meta.take();
                        dbg!(e);
                    }
                }
            }
        }
    }

    #[inline(always)]
    fn setup_info_box(&self) {
        let cache = self.imp().cache.get().unwrap();
        let state = cache.get_cache_state();
        self.imp().avatar_set_id.replace(Some(state.connect_closure(
            "artist-avatar-set",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: CacheState, name: String, hires: gdk::Texture, _: gdk::Texture| {
                    if this.artist().is_some_and(|a| a.get_name() == name) {
                        this.update_avatar(Some(&hires));
                    }
                }
            ),
        )));
        self.imp()
            .avatar_cleared_id
            .replace(Some(state.connect_closure(
                "artist-avatar-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, tag: String| {
                        if this.artist().is_some_and(|a| a.get_name() == tag) {
                            this.update_avatar(None);
                        }
                    }
                ),
            )));
    }

    fn setup_song_subview(&self) {
        // Hook up buttons
        let replace_queue_btn = self.imp().replace_queue.get();
        replace_queue_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let Some(artist) = this.artist() {
                            this.set_is_queuing(true);
                            let library = this.imp().library.upgrade().unwrap();
                            if this.imp().selecting_all.get() {
                                if let Err(e) =
                                    library.queue_artist(&artist, false, true, true).await
                                {
                                    dbg!(e);
                                }
                            } else {
                                let store = &this.imp().song_list;
                                // Get list of selected songs
                                let sel = &this.imp().song_sel_model.selection();
                                let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                                let (iter, first_idx) = gtk::BitsetIter::init_first(sel).unwrap();
                                songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                                iter.for_each(|idx| {
                                    songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                                });
                                if let Err(e) = library.queue_songs(&songs, true, true).await {
                                    dbg!(e);
                                }
                            }
                            this.set_is_queuing(false);
                        }
                    }
                ));
            }
        ));
        let append_queue_btn = self.imp().append_queue.get();
        append_queue_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        if let Some(artist) = this.artist() {
                            this.set_is_queuing(true);
                            let library = this.imp().library.upgrade().unwrap();
                            if this.imp().selecting_all.get() {
                                library.queue_artist(&artist, false, false, false).await;
                            } else {
                                let store = &this.imp().song_list;
                                // Get list of selected songs
                                let sel = &this.imp().song_sel_model.selection();
                                let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                                let (iter, first_idx) = gtk::BitsetIter::init_first(sel).unwrap();
                                songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                                iter.for_each(|idx| {
                                    songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                                });
                                library.queue_songs(&songs, false, false).await;
                            }
                        }
                        this.set_is_queuing(false);
                    }
                ));
            }
        ));

        // Set up factory
        let library = self.imp().library.upgrade().unwrap();
        let cache = self.imp().cache.get().unwrap();
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak]
            library,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache), None);
                item.property_expression("item")
                    .chain_property::<Song>("name")
                    .bind(&row, "name", gtk::Widget::NONE);

                row.set_first_attrib_icon_name(Some("library-music-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("album")
                    .bind(&row, "first-attrib-text", gtk::Widget::NONE);

                row.set_second_attrib_icon_name(Some("hourglass-symbolic"));
                item.property_expression("item")
                    .chain_property::<Song>("duration")
                    .chain_closure::<String>(closure_local!(|_: Option<glib::Object>, dur: u64| {
                        format_secs_as_duration(dur as f64)
                    }))
                    .bind(&row, "second-attrib-text", gtk::Widget::NONE);

                item.property_expression("item")
                    .chain_property::<Song>("quality-grade")
                    .bind(&row, "quality-grade", gtk::Widget::NONE);
                let end_widget = RowAddButtons::new(&library);
                row.set_end_widget(Some(&end_widget.into()));
                item.set_child(Some(&row));
            }
        ));
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
                .expect("The child has to be a `SongRow`.");

            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(Some(&item));
            child.on_bind(&item);
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        factory.connect_unbind(move |_, list_item| {
            // Get `SongRow` from `ListItem` (the UI widget)
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be a `SongRow`.");
            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(None);
            child.on_unbind();
        });

        // Set the factory of the list view
        self.imp().song_subview.set_factory(Some(&factory));
    }

    pub fn setup(&self, library: &Library, cache: Rc<Cache>, window: &EuphonicaWindow) {
        self.imp()
            .cache
            .set(cache)
            .expect("Could not register artist content view with cache controller");
        self.imp().library.set(Some(library));
        self.imp().window.set(Some(window));
        self.imp().tags_widget.set_window(window);

        self.setup_info_box();
        self.setup_song_subview();
        // self.setup_discography_subview();

        self.imp()
            .add_to_playlist
            .bind_model(library, &self.imp().song_sel_model);
    }

    /// Set a user-selected path as the new local avatar.
    pub fn set_avatar(&self, path: String) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                if let (Some(artist), Some(cache)) = (this.artist(), this.imp().cache.get())
                    && let Err(e) = cache
                        .set_artist_avatar(artist.get_name().to_owned(), &path, true)
                        .await
                {
                    this.show_cache_error("Couldn't set cover", e);
                }
            }
        ));
    }

    #[inline]
    fn update_avatar(&self, tex: Option<&gdk::Texture>) {
        // Set text in case there is no image
        self.imp().avatar.set_custom_image(tex);
    }

    async fn schedule_avatar(&self, overwrite: bool) {
        self.update_avatar(None);
        if let Some(info) = self.artist().as_ref().map(|a| a.get_info()) {
            let cache = self.imp().cache.get().unwrap().clone();
            if overwrite {
                // Don't notify, else we'd interrupt the spinner
                if let Err(e) = cache.clear_artist_avatar(info.name.to_owned(), false).await {
                    self.show_cache_error("Couldn't clear avatar", e);
                }
            }
            match cache
                .get_artist_avatar(
                    info, false, true, // Content page is the one to fetch external sources
                )
                .await
            {
                Ok(maybe_tex) => {
                    self.update_avatar(maybe_tex.as_ref());
                }
                Err(e) => {
                    self.show_cache_error("Couldn't fetch avatar", e);
                }
            }
        }
    }

    pub fn bind(&self, artist: &Artist) {
        self.imp().on_song_selection_changed();
        let info = artist.get_info();
        self.imp().release_count.set_label("-");
        self.imp().avatar.set_text(Some(&info.name));

        let name_label = self.imp().name.get();
        let mut bindings = self.imp().bindings.borrow_mut();

        let name_binding = artist
            .bind_property("name", &name_label, "label")
            .transform_to(|_, s: Option<&str>| {
                Some(if s.is_none_or(|s| s.is_empty()) {
                    (*EMPTY_ARTIST_STRING).to_value()
                } else {
                    s.to_value()
                })
            })
            .sync_create()
            .build();
        // Save binding
        bindings.push(name_binding);

        // Save reference to artist object
        self.imp().artist.borrow_mut().replace(artist.clone());

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            artist,
            async move {
                let discography = this.imp().discography_subview.get();
                discography.remove_all();
                let library = this.imp().library.upgrade().unwrap();
                let discography_stack = this.imp().discography_stack.get();
                discography_stack.show_spinner();
                let song_stack = this.imp().song_stack.get();
                song_stack.show_spinner();
                let song_list = this.imp().song_list.clone();
                song_list.remove_all();
                let mut albums_by_year: FxHashMap<Option<i32>, Vec<Album>> = FxHashMap::default();
                // Collect genre strings from albums to pass to the background thread
                // FIXME: this might still block though
                let mut album_genres: Vec<Vec<String>> = Vec::new();
                // Important, MPD-side content first
                dbg!(artist.get_info());
                let _ = library
                    .get_artist_content(
                        &artist,
                        |album| {
                            // Collect genres first
                            album_genres.push(album.get_genres().iter().cloned().collect());
                            let maybe_year = album.get_release_date().map(|d| d.year());
                            if let Some(year_vec) = albums_by_year.get_mut(&maybe_year) {
                                year_vec.push(album);
                            } else {
                                albums_by_year.insert(maybe_year, vec![album]);
                            }
                        },
                        |songs| {
                            song_list.extend_from_slice(&songs);
                        },
                    )
                    .await;
                if albums_by_year.len() > 0 {
                    let release_count = albums_by_year.iter().map(|v| v.1.len()).sum::<usize>();
                    if release_count > 1000 {
                        this.imp().release_count.set_label(">1000");
                    } else {
                        this.imp().release_count.set_label(&release_count.to_string());
                    }
                    discography_stack.show_content();
                    let vp = this.imp().scrolled_window.get();
                    let win = this.imp().window.upgrade();
                    let count_years = albums_by_year.len();
                    for (maybe_year, albums) in albums_by_year.into_iter() {
                        discography.append(&DiscographyYear::new(
                            maybe_year,
                            albums,
                            this.imp().cache.get().unwrap().clone(),
                            &library,
                            win.as_ref(),
                            Some(&vp),
                        ))
                    }
                    // 1 more loop to clear selection highlight (can't do it in the above loop as the insertion position is dictated by sort_func)
                    for y in 0..count_years {
                        if let Some(row) = discography.row_at_index(y as i32) {
                            row.set_activatable(false);
                        }
                    }
                } else {
                    discography_stack.show_placeholder();
                }
                this.imp().update_discography_layout();

                if song_list.n_items() > 0 {
                    song_stack.show_content();
                } else {
                    song_stack.show_placeholder();
                }

                // Populate genres from albums
                let genres_stack = this.imp().genres_stack.get();
                let genres_box = this.imp().genres_box.get();
                let window = this.imp().window.upgrade().unwrap();

                let genres: Vec<String> = {
                    let asyncified = Asyncified::builder().channel_size(1).build_ok(|| ()).await;
                    asyncified
                        .call(move |_| {
                            let mut seen: FxHashSet<String> = FxHashSet::default();
                            for genre_list in album_genres {
                                for genre in genre_list {
                                    for split in split_genre_tag(&genre) {
                                        seen.insert(split.to_owned());
                                    }
                                }
                            }
                            let mut res: Vec<String> = seen.into_iter().collect();
                            res.sort_by_key(|a| a.to_lowercase());
                            res
                        })
                        .await
                };

                genres
                    .iter()
                    .map(|genre| {
                        TagButton::new(
                            &Tag::new(genre.clone(), None, None, false, false),
                            &genres_box,
                            &window,
                            |_| {},
                        )
                    })
                    .for_each(|tag| genres_box.append(&tag));

                if !genres.is_empty()
                    && genres_stack
                        .visible_child_name()
                        .is_some_and(|name| name == "empty")
                {
                    genres_stack.set_visible_child_name("content");
                }

                // The extra fluff later
                this.schedule_avatar(false).await;
                this.update_meta(false).await;
            }
        ));
    }

    pub fn unbind(&self) {
        for binding in self.imp().bindings.take().into_iter() {
            binding.unbind();
        }
        if let Some(id) = self.imp().avatar_signal_id.take()
            && let Some(cache) = self.imp().cache.get()
        {
            cache.get_cache_state().disconnect(id);
        }
        // Unset metadata widgets
        self.imp().avatar.set_text(None);
        self.clear_content();
        self.imp().discography_stack.show_placeholder();
        self.imp().song_stack.show_placeholder();
        self.imp().genres_box.remove_all();
        let genres_stack = self.imp().genres_stack.get();
        if genres_stack
            .visible_child_name()
            .is_some_and(|name| name == "content")
        {
            genres_stack.set_visible_child_name("empty");
        }
        self.set_show_meta(false);
    }

    fn clear_content(&self) {
        self.imp().song_list.remove_all();
        self.imp().discography_subview.remove_all();
    }
}
