use adw::subclass::prelude::*;
use glib::{clone, closure_local, signal::SignalHandlerId, Binding};
use gtk::{gio, glib, prelude::*, BitsetIter, CompositeTemplate, ListItem, SignalListItemFactory};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use mpd::error::{Error as MpdError, ErrorCode as MpdErrorCode, ServerError};

use super::{Library, PlaylistSongRow};
use crate::{
    cache::Cache,
    client::ClientState,
    common::{INode, Song},
    utils::format_secs_as_duration,
    window::EuphonicaWindow,
};

#[derive(Debug)]
pub enum InternalEditAction {
    ShiftBackward(u32),
    ShiftForward(u32),
    Remove(u32),
}

#[derive(Debug)]
pub struct HistoryStep {
    pub action: InternalEditAction,
    pub song: Option<Song>, // for undoing removals
}

impl HistoryStep {
    fn shift(&self, list: &gio::ListStore, old: u32, backward: bool) {
        // Use splice to only emit one update signal, reducing visual jitter
        let src = if backward { old - 1 } else { old };
        let des = src + 1;
        let src_song = list.item(src).unwrap();
        let des_song = list.item(des).unwrap();
        list.splice(src, 2, &[des_song, src_song]);
    }

    pub fn forward(&self, list: &gio::ListStore) {
        match self.action {
            InternalEditAction::ShiftBackward(idx) => {
                self.shift(list, idx, true);
            }
            InternalEditAction::ShiftForward(idx) => {
                self.shift(list, idx, false);
            }
            InternalEditAction::Remove(idx) => {
                list.remove(idx);
            }
        }
    }

    pub fn backward(&self, list: &gio::ListStore) {
        match self.action {
            InternalEditAction::ShiftBackward(idx) => {
                self.shift(list, idx - 1, false);
            }
            InternalEditAction::ShiftForward(idx) => {
                self.shift(list, idx + 1, true);
            }
            InternalEditAction::Remove(idx) => {
                list.insert(idx, self.song.as_ref().unwrap());
            }
        }
    }
}

// Playlist edit logic:
// In order to reduce unnecessary updates, we pack all changes into one command list,
// and update the playlist exactly once on our side upon receiving the single playlist
// change idle signal resulting from that command list.
// To facilitate this, we have to enter an "edit mode" with a separate song ListStore
// and UI.
mod imp {
    use std::cell::Cell;

    use mpd::SaveMode;

    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/playlist-content-view.ui")]
    pub struct PlaylistContentView {
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub collapse_infobox: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub content: TemplateChild<gtk::ListView>,
        #[template_child]
        pub editing_content: TemplateChild<gtk::ListView>,
        #[template_child]
        pub content_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub editing_content_scroller: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub title: TemplateChild<gtk::Label>,

        #[template_child]
        pub last_mod: TemplateChild<gtk::Label>,
        #[template_child]
        pub track_count: TemplateChild<gtk::Label>,
        #[template_child]
        pub runtime: TemplateChild<gtk::Label>,

        #[template_child]
        pub action_row: TemplateChild<gtk::Stack>,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub replace_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub edit_playlist: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit_cancel: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit_undo: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit_redo: TemplateChild<gtk::Button>,
        #[template_child]
        pub edit_apply: TemplateChild<gtk::Button>,
        #[template_child]
        pub sel_all: TemplateChild<gtk::Button>,
        #[template_child]
        pub sel_none: TemplateChild<gtk::Button>,

        #[template_child]
        pub rename_menu_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub delete_menu_btn: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub rename: TemplateChild<gtk::Button>,
        #[template_child]
        pub new_name: TemplateChild<gtk::Entry>,
        #[template_child]
        pub delete: TemplateChild<gtk::Button>,

        pub song_list: gio::ListStore,
        pub editing_song_list: gio::ListStore,
        pub sel_model: gtk::MultiSelection,
        pub is_editing: Cell<bool>,
        pub history: RefCell<Vec<HistoryStep>>,
        pub history_idx: Cell<usize>, // Starts from 1, since 0 is reserved for the initial state (before the first action).

        pub playlist: RefCell<Option<INode>>,
        pub bindings: RefCell<Vec<Binding>>,
        pub cover_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub selecting_all: Cell<bool>, // Enables queuing the entire playlist efficiently
        pub window: OnceCell<EuphonicaWindow>,
        pub library: OnceCell<Library>,

        // FIXME: Working around the scroll position bug. See src/player/queue_view.rs (same issue).
        pub last_scroll_pos: Cell<f64>,
        pub restore_last_pos: Cell<u8>
    }

    impl Default for PlaylistContentView {
        fn default() -> Self {
            Self {
                title: TemplateChild::default(),
                last_mod: TemplateChild::default(),
                track_count: TemplateChild::default(),
                infobox_revealer: TemplateChild::default(),
                collapse_infobox: TemplateChild::default(),
                runtime: TemplateChild::default(),
                content_stack: TemplateChild::default(),
                content: TemplateChild::default(),
                editing_content: TemplateChild::default(),
                content_scroller: TemplateChild::default(),
                editing_content_scroller: TemplateChild::default(),
                song_list: gio::ListStore::new::<Song>(),
                editing_song_list: gio::ListStore::new::<Song>(),
                is_editing: Cell::new(false),
                history: RefCell::new(Vec::new()),
                history_idx: Cell::new(0),
                sel_model: gtk::MultiSelection::new(Option::<gio::ListStore>::None),
                action_row: TemplateChild::default(),
                replace_queue: TemplateChild::default(),
                append_queue: TemplateChild::default(),
                replace_queue_text: TemplateChild::default(),
                append_queue_text: TemplateChild::default(),
                edit_playlist: TemplateChild::default(),
                edit_cancel: TemplateChild::default(),
                edit_undo: TemplateChild::default(),
                edit_redo: TemplateChild::default(),
                edit_apply: TemplateChild::default(),
                sel_all: TemplateChild::default(),
                sel_none: TemplateChild::default(),
                rename_menu_btn: TemplateChild::default(),
                delete_menu_btn: TemplateChild::default(),
                rename: TemplateChild::default(),
                new_name: TemplateChild::default(),
                delete: TemplateChild::default(),
                bindings: RefCell::new(Vec::new()),
                cover_signal_id: RefCell::new(None),
                cache: OnceCell::new(),
                playlist: RefCell::new(None),
                selecting_all: Cell::new(true), // When nothing is selected, default to select-all
                window: OnceCell::new(),
                library: OnceCell::new(),
                last_scroll_pos: Cell::new(0.0),
                restore_last_pos: Cell::new(0)
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistContentView {
        const NAME: &'static str = "EuphonicaPlaylistContentView";
        type Type = super::PlaylistContentView;
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

    impl ObjectImpl for PlaylistContentView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.action_row.set_visible_child_name("queue-mode");
            self.content_stack.set_visible_child_name("queue-mode");
            self.edit_playlist.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.enter_edit_mode();
                }
            ));
            self.edit_cancel.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.exit_edit_mode(false);
                }
            ));
            self.edit_undo.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.undo();
                }
            ));
            self.edit_redo.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.redo();
                }
            ));
            self.edit_apply.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.exit_edit_mode(true);
                }
            ));

            self.sel_model.set_model(Some(&self.song_list.clone()));
            self.content.set_model(Some(&self.sel_model));
            self.editing_content
                .set_model(Some(&gtk::NoSelection::new(Some(
                    self.editing_song_list.clone(),
                ))));

            // Change button labels depending on selection state
            self.sel_model.connect_selection_changed(clone!(
                #[weak(rename_to = this)]
                self,
                move |sel_model, _, _| {
                    // TODO: this can be slow, might consider redesigning
                    let n_sel = sel_model.selection().size();
                    if n_sel == 0 || (n_sel as u32) == sel_model.model().unwrap().n_items() {
                        this.selecting_all.replace(true);
                        this.replace_queue_text.set_label("Play all");
                        this.append_queue_text.set_label("Queue all");
                    } else {
                        // TODO: l10n
                        this.selecting_all.replace(false);
                        this.replace_queue_text
                            .set_label(format!("Play {}", n_sel).as_str());
                        this.append_queue_text
                            .set_label(format!("Queue {}", n_sel).as_str());
                    }
                }
            ));

            let sel_model = self.sel_model.clone();
            self.sel_all.connect_clicked(clone!(
                #[weak]
                sel_model,
                move |_| {
                    sel_model.select_all();
                }
            ));
            self.sel_none.connect_clicked(clone!(
                #[weak]
                sel_model,
                move |_| {
                    sel_model.unselect_all();
                }
            ));
        }
    }

    impl WidgetImpl for PlaylistContentView {}

    impl PlaylistContentView {
        fn update_undo_redo_sensitivity(&self) {
            let curr_idx = self.history_idx.get();
            self.edit_undo.set_sensitive(curr_idx > 0);
            self.edit_redo
                .set_sensitive(curr_idx < self.history.borrow().len());
        }

        pub fn enter_edit_mode(&self) {
            // First, clear the editing song list
            if self.editing_song_list.n_items() > 0 {
                self.editing_song_list.remove_all();
            }
            // Copy the current playlist content to the temporary list store,
            // so we can revert in case user clicks Cancel.
            let mut songs: Vec<Song> = Vec::with_capacity(self.song_list.n_items() as usize);
            for i in 0..self.song_list.n_items() {
                songs.push(
                    self.song_list
                        .item(i)
                        .clone()
                        .and_downcast::<Song>()
                        .unwrap(),
                );
            }
            self.editing_song_list.extend_from_slice(&songs);

            // Everything is now in place; start fading
            self.action_row.set_visible_child_name("edit-mode");
            self.content_stack.set_visible_child_name("edit-mode");
        }

        pub fn exit_edit_mode(&self, apply: bool) {
            if apply {
                // Currently if the command list fails halfway we'll still
                // clear the undo history.
                if self.history_idx.get() > 0 {
                    let song_count = self.editing_song_list.n_items();
                    let mut song_list: Vec<Song> = Vec::with_capacity(song_count as usize);
                    for i in 0..song_count {
                        song_list.push(
                            self.editing_song_list
                                .item(i)
                                .unwrap()
                                .downcast_ref::<Song>()
                                .unwrap()
                                .clone(),
                        );
                    }
                    let _ = self.library.get().unwrap().add_songs_to_playlist(
                        self.playlist.borrow().as_ref().unwrap().get_uri(),
                        &song_list,
                        SaveMode::Replace,
                    );
                    self.history_idx.replace(0);
                }
                self.history.borrow_mut().clear();
                self.edit_undo.set_sensitive(false);
                self.edit_redo.set_sensitive(false);
                self.edit_apply.set_sensitive(false);
            }
            // Just fade back, no need to clear the list (won't lag us
            // since we're not rendering it)
            self.action_row.set_visible_child_name("queue-mode");
            self.content_stack.set_visible_child_name("queue-mode");
        }

        pub fn undo(&self) {
            // If not at 0, get the history step at history_idx, perform
            // the necessary edits on the editing_song_list to revert its
            // changes, then decrement history_idx.
            let curr_idx = self.history_idx.get();
            if curr_idx > 0 {
                let history = self.history.borrow();
                let step = &history[curr_idx - 1];
                step.backward(&self.editing_song_list);
                self.history_idx.replace(curr_idx - 1);
                self.update_undo_redo_sensitivity();
            }
        }

        pub fn redo(&self) {
            // If not at the latest history step, execute the action on
            // the editing_song_list, then increment history_idx.
            let curr_idx = self.history_idx.get();
            let history = self.history.borrow();
            if curr_idx < history.len() {
                // Current index is always 1-based, i.e. 1 points to the 0th element of the history vec.
                // Since redoing means executing the NEXT step, not the current, we need to get the
                // (curr_idx - 1 + 1)th step in the history.
                let step = &history[curr_idx];
                step.forward(&self.editing_song_list);
                self.history_idx.replace(curr_idx + 1);
                self.update_undo_redo_sensitivity();
            }
        }

        pub fn push_history(&self, step: HistoryStep) {
            // Truncate history to current index
            {
                let mut history = self.history.borrow_mut();
                let curr_idx = self.history_idx.get();
                let hist_len = history.len();
                if curr_idx < hist_len {
                    history.truncate(curr_idx);
                }
                history.push(step);
                self.history_idx.replace(history.len());
            }
            self.update_undo_redo_sensitivity();
        }
    }
}

glib::wrapper! {
    pub struct PlaylistContentView(ObjectSubclass<imp::PlaylistContentView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for PlaylistContentView {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl PlaylistContentView {
    pub fn setup(
        &self,
        library: Library,
        client_state: ClientState,
        cache: Rc<Cache>,
        window: EuphonicaWindow,
    ) {
        // cache.get_cache_state().connect_closure(
        //     "album-art-downloaded",
        //     false,
        //     closure_local!(
        //         #[weak(rename_to = this)]
        //         self,
        //         move |_: CacheState, folder_uri: String| {
        //             if let Some(album) = this.imp().album.borrow().as_ref() {
        //                 if folder_uri == album.get_uri() {
        //                     this.update_cover(album.get_info());
        //                 }
        //             }
        //         }
        //     )
        // );
        self.imp()
            .window
            .set(window)
            .expect("PlaylistContentView: Cannot set reference to window");
        self.imp()
            .library
            .set(library)
            .expect("PlaylistContentView: Cannot set reference to library controller");
        client_state.connect_closure(
            "playlist-songs-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                move |_: ClientState, name: String, songs: glib::BoxedAnyObject| {
                    if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                        if playlist.get_name() == Some(&name) {
                            this.add_songs(songs.borrow::<Vec<Song>>().as_ref());
                        }
                    }
                }
            ),
        );

        let _ = self.imp().cache.set(cache.clone());
        let infobox_revealer = self.imp().infobox_revealer.get();
        let collapse_infobox = self.imp().collapse_infobox.get();
        collapse_infobox
            .bind_property("active", &infobox_revealer, "reveal-child")
            .transform_to(|_, active: bool| Some(!active))
            .transform_from(|_, active: bool| Some(!active))
            .bidirectional()
            .sync_create()
            .build();

        infobox_revealer
            .bind_property("child-revealed", &collapse_infobox, "icon-name")
            .transform_to(|_, revealed| {
                if revealed {
                    return Some("up-symbolic");
                }
                Some("down-symbolic")
            })
            .sync_create()
            .build();

        let replace_queue_btn = self.imp().replace_queue.get();
        replace_queue_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                let library = this.imp().library.get().unwrap();
                if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                    if this.imp().selecting_all.get() {
                        library.queue_playlist(playlist.get_name().unwrap(), true, true);
                    } else {
                        let store = &this.imp().song_list;
                        // Get list of selected songs
                        let sel = &this.imp().sel_model.selection();
                        let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                        let (iter, first_idx) = BitsetIter::init_first(sel).unwrap();
                        songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                        iter.for_each(|idx| {
                            songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                        });
                        library.queue_songs(&songs, true, true);
                    }
                }
            }
        ));
        let append_queue_btn = self.imp().append_queue.get();
        append_queue_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                let library = this.imp().library.get().unwrap();
                if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                    if this.imp().selecting_all.get() {
                        library.queue_playlist(playlist.get_name().unwrap(), false, false);
                    } else {
                        let store = &this.imp().song_list;
                        // Get list of selected songs
                        let sel = &this.imp().sel_model.selection();
                        let mut songs: Vec<Song> = Vec::with_capacity(sel.size() as usize);
                        let (iter, first_idx) = BitsetIter::init_first(sel).unwrap();
                        songs.push(store.item(first_idx).and_downcast::<Song>().unwrap());
                        iter.for_each(|idx| {
                            songs.push(store.item(idx).and_downcast::<Song>().unwrap())
                        });
                        library.queue_songs(&songs, false, false);
                    }
                }
            }
        ));

        let rename_btn = self.imp().rename.get();
        let new_name = self.imp().new_name.get();
        let delete_btn = self.imp().delete.get();

        new_name.connect_closure(
            "changed",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                #[weak]
                rename_btn,
                move |entry: gtk::Entry| {
                    if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                        rename_btn.set_sensitive(
                            entry.text_length() > 0
                                && entry.buffer().text() != playlist.get_name().unwrap(),
                        );
                    } else {
                        rename_btn.set_sensitive(false);
                    }
                }
            ),
        );

        rename_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            new_name,
            move |_| {
                this.imp().rename_menu_btn.set_active(false);
                let library = this.imp().library.get().unwrap();
                let rename_from: Option<String>;
                {
                    if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                        rename_from = playlist.get_name().map(&str::to_owned);
                    }
                    else {
                        rename_from = None;
                    }
                }
                if let Some(rename_from) = rename_from {
                    let rename_to = new_name.buffer().text().as_str().to_owned();

                    // Temporarily rebind to a modified INode object. The actual updated INode,
                    // with correct last-modified time, will be given after the idle trigger.
                    let tmp_inode = this.imp().playlist.borrow().as_ref().unwrap().with_new_name(&rename_to);
                    this.unbind(false);
                    this.bind(tmp_inode);

                    match library.rename_playlist(&rename_from, &rename_to) {
                        Ok(()) => {} // Wait for idle to trigger a playlist refresh
                        Err(e) => match e {
                            Some(MpdError::Server(ServerError {code, pos: _, command: _, detail: _})) => {
                                this.imp().window.get().unwrap().show_dialog(
                                    "Rename Failed",
                                    &(match code {
                                        MpdErrorCode::Exist => format!("There is already another playlist named \"{}\". Please pick another name.", &rename_to),
                                        MpdErrorCode::NoExist => "Internal error: tried to rename a nonexistent playlist".to_owned(),
                                        _ => format!("Internal error ({:?})", code)
                                    })
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        ));

        delete_btn.connect_clicked(clone!(
            #[strong(rename_to = this)]
            self,
            move |_| {
                let library = this.imp().library.get().unwrap();
                if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                    // Close popover and exit view
                    this.imp().delete_menu_btn.set_active(false);
                    this.imp().window.get().unwrap().get_playlist_view().pop();
                    let _ = library.delete_playlist(playlist.get_name().unwrap());
                }
            }
        ));

        // Set up factory
        let factory = SignalListItemFactory::new();
        let editing_factory = SignalListItemFactory::new();

        // Create an empty `PlaylistSongRow` during setup
        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cache,
            move |_, list_item| {
                let library = this.imp().library.get().unwrap();
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = PlaylistSongRow::new(library.clone(), this.clone(), &item, cache);
                row.set_queue_controls_visible(true);
                item.set_child(Some(&row));
            }
        ));
        editing_factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cache,
            move |_, list_item| {
                let library = this.imp().library.get().unwrap();
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = PlaylistSongRow::new(library.clone(), this.clone(), &item, cache);
                row.set_edit_controls_visible(true);
                item.set_child(Some(&row));
            }
        ));

        // Tell factory how to bind `PlaylistSongRow` to one of our Playlist GObjects
        [&factory, &editing_factory].iter().for_each(move |f| {
            f.connect_bind(|_, list_item| {
                // Get `Song` from `ListItem` (that is, the data side)
                let item: &ListItem = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");

                // Get `PlaylistSongRow` from `ListItem` (the UI widget)
                let child: PlaylistSongRow = item
                    .child()
                    .and_downcast::<PlaylistSongRow>()
                    .expect("The child has to be an `PlaylistSongRow`.");

                // Within this binding fn is where the cached album art texture gets used.
                child.bind(item);
            });

            // When row goes out of sight, unbind from item to allow reuse with another.
            f.connect_unbind(|_, list_item| {
                // Get `PlaylistSongRow` from `ListItem` (the UI widget)
                let child: PlaylistSongRow = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .child()
                    .and_downcast::<PlaylistSongRow>()
                    .expect("The child has to be an `PlaylistSongRow`.");
                child.unbind();
            });
        });

        editing_factory.connect_teardown(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                // The above scroll bug only manifests after this, so now is the best time to set
                // the corresponding values.
                this.imp().last_scroll_pos.set(this.imp().editing_content_scroller.vadjustment().value());
                this.imp().restore_last_pos.set(2);
            }
        ));

        // Set the factory of the list view
        self.imp().content.set_factory(Some(&factory));
        self.imp()
            .editing_content
            .set_factory(Some(&editing_factory));

        // Disgusting workaround until I can pinpoint whenever this is a GTK problem.
        self.imp().editing_content_scroller.vadjustment().connect_notify_local(
            Some("value"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |adj, _| {
                    let checks_left = this.imp().restore_last_pos.get();
                    if checks_left > 0 {
                        let old_pos = this.imp().last_scroll_pos.get();
                        if adj.value() == 0.0 {
                            adj.set_value(old_pos);
                        }
                        else {
                            this.imp().restore_last_pos.set(checks_left - 1);
                            // this.imp().restore_last_pos.set(false);
                        }
                    }
                }
            )
        );
    }

    /// Returns true if an album art was successfully retrieved.
    /// On false, we will want to call cache.ensure_local_album_art()
    // fn update_cover(&self, info: &PlaylistInfo) -> bool {
    //     if let Some(cache) = self.imp().cache.get() {
    //         if let Some(tex) = cache.load_cached_album_art(info, false, true) {
    //             self.imp().cover.set_paintable(Some(&tex));
    //             return true;
    //         }
    //         else {
    //             self.imp().cover.set_paintable(Some(&*ALBUMART_PLACEHOLDER));
    //             return false;
    //         }
    //     }
    //     false
    // }

    pub fn bind(&self, playlist: INode) {
        let title_label = self.imp().title.get();
        let last_mod_label = self.imp().last_mod.get();
        let mut bindings = self.imp().bindings.borrow_mut();

        let title_binding = playlist
            .bind_property("uri", &title_label, "label")
            .sync_create()
            .build();
        // Save binding
        bindings.push(title_binding);

        let last_mod_binding = playlist
            .bind_property("last-modified", &last_mod_label, "label")
            .sync_create()
            .build();
        // Save binding
        bindings.push(last_mod_binding);

        // self.update_cover();
        self.imp().playlist.borrow_mut().replace(playlist);
    }

    pub fn unbind(&self, clear_contents: bool) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
        if let Some(id) = self.imp().cover_signal_id.take() {
            if let Some(cache) = self.imp().cache.get() {
                cache.get_cache_state().disconnect(id);
            }
        }
        if clear_contents {
            self.imp().song_list.remove_all();
        }
        // Always clear the editing song list & exit edit mode without saving
        self.imp().exit_edit_mode(false);
        if self.imp().editing_song_list.n_items() > 0 {
            self.imp().editing_song_list.remove_all();
        }
    }

    fn add_songs(&self, songs: &[Song]) {
        // To facilitate editing, each song needs to keep its own position within the playlist.
        // TODO: find a less fragile algo for this
        self.imp().song_list.extend_from_slice(songs);
        self.imp()
            .track_count
            .set_label(&self.imp().song_list.n_items().to_string());
        self.imp().runtime.set_label(&format_secs_as_duration(
            self.imp()
                .song_list
                .iter()
                .map(|item: Result<Song, _>| {
                    if let Ok(song) = item {
                        return song.get_duration();
                    }
                    0
                })
                .sum::<u64>() as f64,
        ));
    }

    pub fn current_playlist(&self) -> Option<INode> {
        self.imp().playlist.borrow().clone()
    }

    pub fn shift_backward(&self, idx: u32) {
        if idx > 0 {
            let step = HistoryStep {
                action: InternalEditAction::ShiftBackward(idx),
                song: None,
            };
            step.forward(&self.imp().editing_song_list);
            self.imp().push_history(step);
        }
    }

    pub fn shift_forward(&self, idx: u32) {
        let len = self.imp().editing_song_list.n_items();
        if len > 1 && idx < (len - 1) {
            let step = HistoryStep {
                action: InternalEditAction::ShiftForward(idx),
                song: None,
            };
            step.forward(&self.imp().editing_song_list);
            self.imp().push_history(step);
        }
    }

    pub fn remove(&self, idx: u32) {
        let step = HistoryStep {
            action: InternalEditAction::Remove(idx),
            song: Some(
                self.imp()
                    .editing_song_list
                    .item(idx)
                    .unwrap()
                    .clone()
                    .downcast::<Song>()
                    .unwrap(),
            ),
        };
        step.forward(&self.imp().editing_song_list);
        self.imp().push_history(step);
    }
}
