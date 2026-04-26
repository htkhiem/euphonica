use adw::subclass::prelude::*;
use ashpd::desktop::file_chooser::SelectedFiles;
use derivative::Derivative;
use gio::{ActionEntry, SimpleActionGroup};
use glib::{Binding, WeakRef, clone, closure_local, signal::SignalHandlerId};
use gtk::{
    BitsetIter, CompositeTemplate, ListItem, SignalListItemFactory, gdk, gio, glib, prelude::*,
};
use mpd::SaveMode;
use mpd::error::{Error as MpdError, ErrorCode as MpdErrorCode, ServerError};
use std::{
    cell::{Cell, OnceCell, RefCell},
    rc::Rc,
};

use super::Library;
use crate::{
    cache::Cache,
    client::Error as ClientError,
    common::{ContentStack, INode, ImageStack, RowAddButtons, RowEditButtons, Song, SongRow},
    utils::{format_secs_as_duration, tokio_runtime},
    window::EuphonicaWindow,
};

#[derive(Debug)]
pub enum InternalEditAction {
    ShiftBackward(u32),
    ShiftForward(u32),
    MovePos(
        /// From position
        u32, 
        /// To position
        u32
    ),
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
            InternalEditAction::MovePos(from_pos, to_pos) => {
                // We need to remove the item from the liststore, then add it back in.
                // After removal, if the item precedes the target pos, the target pos would
                // be one behind the original one.
                let local_new_pos = if from_pos < to_pos {
                    to_pos - 1
                } else {
                    to_pos
                };
                let to_move = list.item(from_pos).unwrap();
                list.remove(from_pos);
                list.insert(local_new_pos, &to_move);
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
            InternalEditAction::MovePos(from_pos, to_pos) => {
                // Mirrored version of the forward one
                let local_new_pos = if from_pos < to_pos {
                    to_pos - 1
                } else {
                    to_pos
                };
                let to_move = list.item(local_new_pos).unwrap();
                list.remove(local_new_pos);
                
                list.insert(from_pos, &to_move);
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
    use super::*;

    #[derive(Debug, CompositeTemplate, Derivative)]
    #[derivative(Default)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/playlist-content-view.ui")]
    pub struct PlaylistContentView {
        #[template_child]
        pub infobox_revealer: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub collapse_infobox: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub cover: TemplateChild<ImageStack>,
        #[template_child]
        pub subview_stack: TemplateChild<gtk::Stack>,
        #[template_child]
        pub content_stack: TemplateChild<ContentStack>,
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

        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub song_list: gio::ListStore,
        #[derivative(Default(value = "gio::ListStore::new::<Song>()"))]
        pub editing_song_list: gio::ListStore,
        #[derivative(Default(value = "gtk::MultiSelection::new(Option::<gio::ListStore>::None)"))]
        pub sel_model: gtk::MultiSelection,
        pub is_editing: Cell<bool>,
        pub history: RefCell<Vec<HistoryStep>>,
        pub history_idx: Cell<usize>, // Starts from 1, since 0 is reserved for the initial state (before the first action).

        pub playlist: RefCell<Option<INode>>,
        pub bindings: RefCell<Vec<Binding>>,
        pub cover_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        #[derivative(Default(value = "Cell::new(true)"))]
        pub selecting_all: Cell<bool>, // Enables queuing the entire playlist efficiently
        pub window: WeakRef<EuphonicaWindow>,
        pub library: WeakRef<Library>,

        // FIXME: Working around the scroll position bug. See src/player/queue_view.rs (same issue).
        pub last_scroll_pos: Cell<f64>,
        pub restore_last_pos: Cell<u8>,
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
            println!("Disposing playlist content view");
        }

        fn constructed(&self) {
            self.parent_constructed();

            self.song_list
                .bind_property("n-items", &self.track_count.get(), "label")
                .sync_create()
                .build();

            self.action_row.set_visible_child_name("queue-mode");
            self.subview_stack.set_visible_child_name("queue-mode");
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
                            .set_label(format!("Play {n_sel}").as_str());
                        this.append_queue_text
                            .set_label(format!("Queue {n_sel}").as_str());
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

            // Edit actions
            let obj = self.obj();
            let action_set_cover = ActionEntry::builder("set-cover")
                .activate(clone!(
                    #[weak]
                    obj,
                    #[upgrade_or]
                    (),
                    move |_, _, _| {
                        let (sender, receiver) = oneshot::channel();
                        tokio_runtime().spawn(async move {
                            let maybe_files = SelectedFiles::open_file()
                                .title("Select a new album art")
                                .modal(true)
                                .multiple(false)
                                .send()
                                .await
                                .expect("ashpd file open await failure")
                                .response();

                            sender.send(if let Ok(files) = maybe_files {
                                let uris = files.uris();
                                if !uris.is_empty() {
                                    Some(uris[0].to_string())
                                } else {
                                    None
                                }
                            } else {
                                println!("{maybe_files:?}");
                                None
                            });
                        });
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let Some(path) = receiver.await.expect("Broken oneshot receiver")
                                {
                                    obj.set_cover(path);
                                }
                            }
                        ));
                    }
                ))
                .build();
            let action_clear_cover = ActionEntry::builder("clear-cover")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                let title = obj.imp().title.label().to_string();
                                if !title.is_empty()
                                    && let Some(cache) = obj.imp().cache.get()
                                {
                                    obj.clear_cover();
                                    cache.clear_playlist_cover(title).await;
                                }
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = SimpleActionGroup::new();
            actions.add_action_entries([action_set_cover, action_clear_cover]);
            self.obj()
                .insert_action_group("playlist-content-view", Some(&actions));
        }
    }

    impl WidgetImpl for PlaylistContentView {}

    impl PlaylistContentView {
        fn update_btn_sensitivity(&self) {
            let curr_idx = self.history_idx.get();
            self.edit_undo.set_sensitive(curr_idx > 0);
            self.edit_redo
                .set_sensitive(curr_idx < self.history.borrow().len());
            self.edit_apply.set_sensitive(curr_idx > 0);
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
            self.subview_stack.set_visible_child_name("edit-mode");
            self.edit_apply.set_sensitive(false);
        }

        pub fn exit_edit_mode(&self, apply: bool) {
            glib::spawn_future_local(clone!(
                #[weak(rename_to = this)]
                self,
                async move {
                    if apply {
                        // Currently if the command list fails halfway we'll still
                        // clear the undo history.
                        if this.history_idx.get() > 0 {
                            let song_count = this.editing_song_list.n_items();
                            let mut song_list: Vec<Song> = Vec::with_capacity(song_count as usize);
                            for i in 0..song_count {
                                song_list.push(
                                    this.editing_song_list
                                        .item(i)
                                        .unwrap()
                                        .downcast_ref::<Song>()
                                        .unwrap()
                                        .clone(),
                                );
                            }
                            if let Err(e) = this
                                .library
                                .upgrade()
                                .unwrap()
                                .add_songs_to_playlist(
                                    this.playlist
                                        .borrow()
                                        .as_ref()
                                        .unwrap()
                                        .get_uri()
                                        .to_owned(),
                                    &song_list,
                                    SaveMode::Replace,
                                )
                                .await
                            {
                                dbg!(e);
                            };
                            this.history_idx.replace(0);
                        }
                        this.history.borrow_mut().clear();
                        this.edit_undo.set_sensitive(false);
                        this.edit_redo.set_sensitive(false);
                    }
                    // Just fade back, no need to clear the list (won't lag us
                    // since we're not rendering it)
                    this.action_row.set_visible_child_name("queue-mode");
                    this.subview_stack.set_visible_child_name("queue-mode");
                }
            ));
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
                self.update_btn_sensitivity();
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
                self.update_btn_sensitivity();
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
            self.update_btn_sensitivity();
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

fn bind_editing_row_by_expressions(row: &SongRow, item: &ListItem) {
    item.property_expression("item")
        .chain_property::<Song>("name")
        .bind(row, "name", gtk::Widget::NONE);

    row.set_first_attrib_icon_name(Some("library-music-symbolic"));
    item.property_expression("item")
        .chain_property::<Song>("album")
        .bind(row, "first-attrib-text", gtk::Widget::NONE);

    row.set_second_attrib_icon_name(Some("music-artist-symbolic"));
    item.property_expression("item")
        .chain_property::<Song>("artist")
        .bind(row, "second-attrib-text", gtk::Widget::NONE);

    row.set_third_attrib_icon_name(Some("hourglass-symbolic"));
    item.property_expression("item")
        .chain_property::<Song>("duration")
        .chain_closure::<String>(closure_local!(|_: Option<glib::Object>, dur: u64| {
            format_secs_as_duration(dur as f64)
        }))
        .bind(row, "third-attrib-text", gtk::Widget::NONE);

    item.property_expression("item")
        .chain_property::<Song>("quality-grade")
        .bind(row, "quality-grade", gtk::Widget::NONE);
}

impl PlaylistContentView {
    pub fn setup(&self, library: &Library, cache: Rc<Cache>, window: &EuphonicaWindow) {
        self.imp().window.set(Some(window));
        self.imp().library.set(Some(library));

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
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        let library = this.imp().library.upgrade().unwrap();
                        if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                            if this.imp().selecting_all.get() {
                                library
                                    .queue_playlist(
                                        playlist.get_name().unwrap().to_owned(),
                                        true,
                                        true,
                                    )
                                    .await;
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
                                library.queue_songs(&songs, true, true).await;
                            }
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
                        let library = this.imp().library.upgrade().unwrap();
                        if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                            if this.imp().selecting_all.get() {
                                library
                                    .queue_playlist(
                                        playlist.get_name().unwrap().to_owned(),
                                        false,
                                        false,
                                    )
                                    .await;
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
                                library.queue_songs(&songs, false, false).await;
                            }
                        }
                    }
                ));
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
                glib::spawn_future_local(clone!(#[weak] this, #[weak] new_name, async move {
                    this.imp().rename_menu_btn.set_active(false);
                    let library = this.imp().library.upgrade().unwrap();
                    let rename_from: Option<String>;
                    {
                        if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                            rename_from = playlist.get_name().map(str::to_owned);
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


                        match library.rename_playlist(rename_from, rename_to.clone()).await {
                            Ok(()) => {} // Wait for idle to trigger a playlist refresh
                            Err(ClientError::Mpd(e)) => if let MpdError::Server(ServerError {code, pos: _, command: _, detail: _}) = e {
                                this.imp().window.upgrade().unwrap().show_dialog(
                                    "Rename Failed",
                                    &(match code {
                                        MpdErrorCode::Exist => format!("There is already another playlist named \"{}\". Please pick another name.", &rename_to),
                                        MpdErrorCode::NoExist => "Internal error: tried to rename a nonexistent playlist".to_owned(),
                                        _ => format!("Internal error ({code:?})")
                                    })
                                );
                            }
                            Err(e) => {
                                this.imp().window.upgrade().unwrap().show_dialog("Rename Failed", &format!("{e:?}"));
                            }
                        }
                    }
                }));
            }
        ));

        delete_btn.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    async move {
                        let library = this.imp().library.upgrade().unwrap();
                        if let Some(playlist) = this.imp().playlist.borrow().as_ref() {
                            // Close popover and exit view
                            this.imp().delete_menu_btn.set_active(false);
                            this.imp()
                                .window
                                .upgrade()
                                .unwrap()
                                .get_playlist_view()
                                .pop();
                            let _ = library
                                .delete_playlist(playlist.get_name().unwrap().to_owned())
                                .await;
                        }
                    }
                ));
            }
        ));

        // Set up factory
        let factory = SignalListItemFactory::new();
        let editing_factory = SignalListItemFactory::new();

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
                let end_widget = RowAddButtons::new(&library);
                row.set_end_widget(Some(&end_widget.into()));
                item.set_child(Some(&row));
            }
        ));
        editing_factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache), None);
                row.add_css_class("shift-on-hover");
                bind_editing_row_by_expressions(&row, item);
                let end_widget = RowEditButtons::new(
                    item,
                    // Raise action
                    clone!(
                        #[weak]
                        this,
                        move |_, idx| {
                            this.shift_backward(idx);
                        }
                    ),
                    clone!(
                        #[weak]
                        this,
                        move |_, idx| {
                            this.shift_forward(idx);
                        }
                    ),
                    clone!(
                        #[weak]
                        this,
                        move |_, idx| {
                            this.remove(idx);
                        }
                    ),
                );
                row.set_end_widget(Some(&end_widget.into()));
                item.set_child(Some(&row));

                // Handle drag-n-drop (DnD)
                let drag_source = gtk::DragSource::new();
                drag_source.set_actions(gdk::DragAction::COPY); // TODO: probably not needed? not moving files across apps
                drag_source.connect_prepare(clone!(
                    #[weak]
                    item,
                    #[upgrade_or]
                    None,
                    move |_, _x, _y| {
                        // FIXME: nonzero hotspots cause the drag icon to fly off-screen.
                        // Pass the whole song GObject
                        if let Some(song) = item.item().and_downcast::<Song>() {
                            song.set_queue_pos(item.position());  // Not queue pos, but playlist order
                            Some(gdk::ContentProvider::for_value(&song.to_value()))
                        } else {
                            None
                        }
                    }
                ));
                drag_source.connect_drag_begin(clone!(
                    #[weak]
                    row,
                    #[weak]
                    item,
                    move |_source, drag| {
                        row.set_floating(true);
                        // To avoid problems with hotspot positioning quirks (caused by other rows changing padding upon hover)
                        // the icon will be a standalone copy of the original row.
                        // Additional benefit: we get to customise how it looks.
                        let drag_widget = SongRow::new(None, None);
                        // Give it the same size as the real row
                        drag_widget.set_size_request(row.width(), row.height());
                        drag_widget.set_thumbnail_visible(false);
                        bind_editing_row_by_expressions(&drag_widget, &item);
                        
                        // The drag icon version should have an opaque background for legibility when rendered over other rows.
                        // Adwaita already has a .card class that does that + adds rounded corners and drop shadows too.
                        // Looks nice IMO.
                        drag_widget.add_css_class("card");
                        let drag_icon = gtk::DragIcon::for_drag(&drag);
                        drag_icon.set_child(Some(&drag_widget));
                    }
                )); 
                drag_source.connect_drag_end(clone!(
                    #[weak]
                    row,
                    move |_, _, _| {
                        row.set_floating(false);
                    }
                ));
                row.add_controller(drag_source);
                // If another row is being held above this one in a DnD operation, make some space by increasing top
                // or bottom padding (depending on whether the mouse is over the upper or lower half of this row)
                let drop_controller = gtk::DropTarget::new(Song::static_type(), gdk::DragAction::COPY);
                drop_controller.connect_motion(clone!(
                    #[weak]
                    row,
                    #[upgrade_or]
                    gdk::DragAction::COPY,
                    move |_, _x, y| {
                        if !row.is_floating() {
                            let has_shift_up = row.has_css_class("shift-up");
                            let has_shift_down = row.has_css_class("shift-down");
                            let is_lower_half = y > row.height() as f64 / 2.0;

                            let should_shift_down = !is_lower_half;
                            let should_shift_up = is_lower_half;
                            if should_shift_down && !has_shift_down {
                                row.add_css_class("shift-down");
                            } else if !should_shift_down && has_shift_down {
                                row.remove_css_class("shift-down");
                            }
                            if should_shift_up && !has_shift_up {
                                row.add_css_class("shift-up");
                            } else if !should_shift_up && has_shift_up {
                                row.remove_css_class("shift-up");
                            }
                        }
                        gdk::DragAction::COPY
                    }
                ));
                drop_controller.connect_leave(clone!(
                    #[weak]
                    row,
                    move |_| {
                        if !row.is_floating() {
                            if row.has_css_class("shift-up") {
                                row.remove_css_class("shift-up");
                            }
                            if row.has_css_class("shift-down") {
                                row.remove_css_class("shift-down");
                            }
                        }
                    }
                ));

                drop_controller.connect_drop(clone!(
                    #[weak]
                    this,
                    #[weak]
                    row,
                    #[weak]
                    item,
                    #[upgrade_or]
                    false,
                    move |_, song, x, y| {
                        row.set_floating(false);
                        if !row.is_floating() {
                            if let Ok(song) = song.get::<Song>() {
                                // Get queue pos of row being dropped onto
                                let target_pos = item.position() + if y > row.height() as f64 / 2.0 {
                                    // If is lower half, place dropped song after this one
                                    1    
                                } else {
                                    0
                                };
                                this.move_pos(song.get_queue_pos(), target_pos);
                                true
                            } else {
                                false
                            }
                        } else {
                            // Ignore if dropped atop itself
                            false
                        }
                    }
                ));

                row.add_controller(drop_controller);
            }
        ));

        factory.connect_bind(|_, list_item| {
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

            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(Some(&item));
            child.on_bind(&item);
        });

        editing_factory.connect_bind(|_, list_item| {
            let item: Song = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Song>()
                .expect("The item has to be a common::Song.");

            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be an `SongRow`.");

            child.on_bind(&item);
        });

        factory.connect_unbind(|_, list_item| {
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be an `SongRow`.");
            child
                .end_widget()
                .and_downcast::<RowAddButtons>()
                .unwrap()
                .set_song(None);
            child.on_unbind();
        });

        editing_factory.connect_unbind(|_, list_item| {
            let child: SongRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be an `SongRow`.");
            child.on_unbind();
        });

        editing_factory.connect_teardown(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                // The above scroll bug only manifests after this, so now is the best time to set
                // the corresponding values.
                this.imp()
                    .last_scroll_pos
                    .set(this.imp().editing_content_scroller.vadjustment().value());
                this.imp().restore_last_pos.set(2);
            }
        ));

        // Set the factory of the list view
        self.imp().content.set_factory(Some(&factory));
        self.imp()
            .editing_content
            .set_factory(Some(&editing_factory));

        // Disgusting workaround until I can pinpoint whenever this is a GTK problem.
        self.imp()
            .editing_content_scroller
            .vadjustment()
            .connect_notify_local(
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
                            } else {
                                this.imp().restore_last_pos.set(checks_left - 1);
                                // this.imp().restore_last_pos.set(false);
                            }
                        }
                    }
                ),
            );
    }

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

        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            #[strong]
            playlist,
            async move {
                let name = playlist.get_uri().to_owned();
                // Fetch high resolution playlist cover
                this.imp().cover.show_spinner();
                match this
                    .imp()
                    .cache
                    .get()
                    .unwrap()
                    .get_playlist_cover(name.clone(), false, false)
                    .await
                {
                    Ok(Some(tex)) => {
                        this.update_cover(&tex);
                    }
                    Ok(None) => {
                        this.clear_cover();
                    }
                    Err(e) => {
                        dbg!(e);
                    }
                };

                this.imp().playlist.replace(Some(playlist));

                let content_stack = this.imp().content_stack.get();
                content_stack.show_spinner();
                let song_list = this.imp().song_list.clone();
                song_list.remove_all();
                let _ = this
                    .imp()
                    .library
                    .upgrade()
                    .unwrap()
                    .get_playlist_songs(name, &mut |songs| {
                        song_list.extend_from_slice(&songs);
                    })
                    .await;
                if song_list.n_items() > 0 {
                    content_stack.show_content();
                } else {
                    content_stack.show_placeholder();
                }
                this.imp().runtime.set_label(&format_secs_as_duration(
                    this.imp()
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
        ));
    }

    pub fn unbind(&self, clear_contents: bool) {
        for binding in self.imp().bindings.borrow_mut().drain(..) {
            binding.unbind();
        }
        if let Some(id) = self.imp().cover_signal_id.take()
            && let Some(cache) = self.imp().cache.get()
        {
            cache.get_cache_state().disconnect(id);
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

    /// Set a user-selected path as the new local avatar.
    pub fn set_cover(&self, path: String) {
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)]
            self,
            async move {
                let title = this.imp().title.label();
                if !title.is_empty()
                    && let Some(cache) = this.imp().cache.get()
                {
                    cache.set_playlist_cover(title.to_string(), &path).await;
                }
            }
        ));
    }

    #[inline]
    fn update_cover(&self, tex: &gdk::Texture) {
        // Set text in case there is no image
        self.imp().cover.show(tex);
    }

    #[inline]
    fn clear_cover(&self) {
        self.imp().cover.clear();
    }

    #[inline]
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

    pub fn move_pos(&self, from_pos: u32, to_pos: u32) {
        let step = HistoryStep {
            action: InternalEditAction::MovePos(from_pos, to_pos),
            song: None
        };
        step.forward(&self.imp().editing_song_list);
        self.imp().push_history(step);
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
