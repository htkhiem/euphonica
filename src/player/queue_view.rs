use adw::prelude::*;
use adw::subclass::prelude::*;
use gtk::{
    CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection, gio, gdk,
    glib::{self, Properties, WeakRef, clone, closure_local, subclass::Signal},
};
use mpd::{
    SaveMode,
    error::{Error as MpdError, ErrorCode as MpdErrorCode, ServerError},
};
use std::{cell::Cell, rc::Rc, sync::OnceLock};

use super::PlayerPane;

use crate::{
    cache::Cache,
    client::{ClientState, Error as ClientError},
    common::{ContentStack, RowEditButtons, Song, SongRow},
    player::controller::SwapDirection,
    utils::LazyInit,
    window::EuphonicaWindow,
};

use super::Player;

mod imp {
    use super::*;

    #[derive(Debug, Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::QueueView)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/queue-view.ui")]
    pub struct QueueView {
        #[template_child]
        pub show_sidebar: TemplateChild<gtk::Button>,
        #[template_child]
        pub queue_pane_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub content_stack: TemplateChild<ContentStack>,
        #[template_child]
        pub scrolled_window: TemplateChild<gtk::ScrolledWindow>,
        #[template_child]
        pub queue: TemplateChild<gtk::ListView>,
        #[template_child]
        pub queue_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub player_pane: TemplateChild<PlayerPane>,
        #[template_child]
        pub consume: TemplateChild<gtk::ToggleButton>,
        #[template_child]
        pub clear_queue: TemplateChild<gtk::Button>,

        #[template_child]
        pub save: TemplateChild<gtk::MenuButton>,
        #[template_child]
        pub save_name: TemplateChild<gtk::Entry>,
        #[template_child]
        pub save_confirm: TemplateChild<gtk::Button>,

        pub window: WeakRef<EuphonicaWindow>,

        #[property(get, set)]
        pub pane_collapsed: Cell<bool>,
        #[property(get, set)]
        pub collapsed: Cell<bool>,
        #[property(get, set)]
        pub show_content: Cell<bool>,

        // FIXME: ScrolledWindow resets scroll position upon item removal.
        // This is especially annoying in that the scroll position might be
        // reset to zero many times, negating our first restores.
        // Our current workaround is to:
        // - Only restore when the value hits zero.
        // - Stop trying to do so once the value has changed twice without
        // either being zero (indicating user scrolling).
        // Disgusting I know but it works for now without being too
        // noticeable.
        pub last_scroll_pos: Cell<f64>,
        pub restore_last_pos: Cell<u8>,

        pub player: WeakRef<Player>,

        // Avoid multiple inits running concurrently
        pub initializing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for QueueView {
        const NAME: &'static str = "EuphonicaQueueView";
        type Type = super::QueueView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            Self::bind_template(klass);

            klass.set_layout_manager_type::<gtk::BinLayout>();
            // klass.set_css_name("QueueView");
            klass.set_accessible_role(gtk::AccessibleRole::Group);
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    #[glib::derived_properties]
    impl ObjectImpl for QueueView {
        fn dispose(&self) {
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
            println!("Disposing queue view");
        }

        fn constructed(&self) {
            self.parent_constructed();
            self.content_stack.show_placeholder();
            let obj = self.obj();
            obj.bind_property("pane-collapsed", &self.queue_pane_view.get(), "collapsed")
                .sync_create()
                .build();

            self.queue_pane_view
                .bind_property("show-content", obj.as_ref(), "show-content")
                .bidirectional()
                .sync_create()
                .build();

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

            let action_clear_rating = gio::ActionEntry::builder("clear-rating")
                .activate(clone!(
                    #[weak]
                    obj,
                    move |_, _, _| {
                        glib::spawn_future_local(clone!(
                            #[weak]
                            obj,
                            async move {
                                if let Some(player) = obj.imp().player.upgrade() {
                                    player.rate_current_song(None).await;
                                }
                            }
                        ));
                    }
                ))
                .build();

            // Create a new action group and add actions to it
            let actions = gio::SimpleActionGroup::new();
            actions.add_action_entries([action_clear_rating]);
            self.obj().insert_action_group("queue-view", Some(&actions));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| vec![Signal::builder("show-sidebar-clicked").build()])
        }
    }

    impl WidgetImpl for QueueView {}
}

glib::wrapper! {
    pub struct QueueView(ObjectSubclass<imp::QueueView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gio::ActionMap, gio::ActionGroup;
}

impl Default for QueueView {
    fn default() -> Self {
        glib::Object::new()
    }
}

fn format_song_count(count: u32) -> Option<String> {
    // TODO: translatable
    if count == 0 {
        Some(String::from("Empty"))
    } else if count == 1 {
        Some(String::from("1 song"))
    } else {
        Some(format!("{count} songs"))
    }
}

fn bind_row_by_expressions(row: &SongRow, item: &ListItem) {
    row.set_index_visible(false);
    row.set_playing_indicator_visible(true);

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

    item.property_expression("item")
        .chain_property::<Song>("quality-grade")
        .bind(row, "quality-grade", gtk::Widget::NONE);
}

impl QueueView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn setup_listview(&self, player: &Player, cache: Rc<Cache>) {
        // Enable/disable clear queue button depending on whether the queue is empty or not
        // Set selection mode
        // TODO: Allow click to jump to song
        let queue_model = player.queue().clone();
        let sel_model = SingleSelection::new(Some(queue_model));
        self.imp().queue.set_model(Some(&sel_model));

        // Set up factory
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache.clone()), Some(&player));
                bind_row_by_expressions(&row, item);
                // row.set_index_visible(false);
                // row.set_playing_indicator_visible(true);

                // item.property_expression("item")
                //     .chain_property::<Song>("name")
                //     .bind(&row, "name", gtk::Widget::NONE);

                // row.set_first_attrib_icon_name(Some("library-music-symbolic"));
                // item.property_expression("item")
                //    .chain_property::<Song>("album")
                //     .bind(&row, "first-attrib-text", gtk::Widget::NONE);
                // row.set_second_attrib_icon_name(Some("music-artist-symbolic"));
                // item.property_expression("item")
                //     .chain_property::<Song>("artist")
                //     .bind(&row, "second-attrib-text", gtk::Widget::NONE);

                // item.property_expression("item")
                //     .chain_property::<Song>("quality-grade")
                //     .bind(&row, "quality-grade", gtk::Widget::NONE);
                let end_widget = RowEditButtons::new(
                    item,
                    // Raise action
                    clone!(
                        #[weak]
                        player,
                        move |btn, idx| {
                            glib::spawn_future_local(clone!(
                                #[weak]
                                btn,
                                #[weak]
                                player,
                                async move {
                                    btn.set_sensitive(false);
                                    player.swap_dir(idx, SwapDirection::Up).await;
                                    btn.set_sensitive(true);
                                }
                            ));
                        }
                    ),
                    clone!(
                        #[weak]
                        player,
                        move |btn, idx| {
                            glib::spawn_future_local(clone!(
                                #[weak]
                                btn,
                                #[weak]
                                player,
                                async move {
                                    btn.set_sensitive(false);
                                    player.swap_dir(idx, SwapDirection::Down).await;
                                    btn.set_sensitive(true);
                                }
                            ));
                        }
                    ),
                    clone!(
                        #[weak]
                        player,
                        move |btn, idx| {
                            glib::spawn_future_local(clone!(
                                #[weak]
                                btn,
                                #[weak]
                                player,
                                async move {
                                    btn.set_sensitive(false);
                                    player.remove_pos(idx).await;
                                    btn.set_sensitive(true);
                                }
                            ));
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
                    this,
                    #[weak]
                    item,
                    #[upgrade_or]
                    None,
                    move |_, _x, _y| {
                        // FIXME: nonzero hotspots cause the drag icon to fly off-screen.
                        // Pass the whole song GObject
                        if let Some(song) = item.item().and_downcast::<Song>() {
                            song.set_queue_pos(item.position());  // Ensure the Song object contains the up-to-date queue pos for local updating
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
                    this,
                    #[weak]
                    item,
                    move |_source, drag| {
                        row.set_floating(true);
                        // To avoid problems with hotspot positioning quirks (caused by other rows changing padding upon hover)
                        // the icon will be a standalone copy of the original row.
                        // Additional benefit: we get to customise how it looks.
                        let drag_widget = SongRow::new(Some(cache.clone()), None);
                        // Give it the same size as the real row
                        drag_widget.set_size_request(row.width(), row.height());
                        drag_widget.set_thumbnail_visible(false);
                        bind_row_by_expressions(&drag_widget, &item);
                        
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
                    row,
                    #[weak]
                    item,
                    #[weak]
                    player,
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
                                glib::spawn_future_local(async move {
                                    player.move_to(&song, target_pos).await;
                                });
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
        factory.connect_bind(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .item()
                    .and_downcast::<Song>()
                    .expect("The item has to be a common::Song.");
                let child = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .child()
                    .and_downcast::<SongRow>()
                    .expect("The child has to be a `SongRow`.");

                // Within this binding fn is where the cached album art texture gets used.
                child.on_bind(&item);

                this.imp()
                    .last_scroll_pos
                    .set(this.imp().scrolled_window.vadjustment().value());
                this.imp().restore_last_pos.set(2);
            }
        ));

        // When row goes out of sight, unbind from item to allow reuse with another.
        // Remember to also unset the thumbnail widget's texture to potentially free it from memory.
        factory.connect_unbind(move |_, list_item| {
            let child = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<SongRow>()
                .expect("The child has to be a `SongRow`.");
            child.on_unbind();
        });

        factory.connect_teardown(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                // The above scroll bug also manifests after this, so now is the best time to set
                // the corresponding values.
                this.imp()
                    .last_scroll_pos
                    .set(this.imp().scrolled_window.vadjustment().value());
                this.imp().restore_last_pos.set(2);
            }
        ));

        // Set the factory of the list view
        self.imp().queue.set_factory(Some(&factory));

        // Setup click action
        self.imp().queue.connect_activate(clone!(
            #[weak]
            player,
            move |queue, position| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    #[weak]
                    queue,
                    async move {
                        let model = queue.model().expect("The model has to exist.");
                        let song = model
                            .item(position)
                            .and_downcast::<Song>()
                            .expect("The item has to be a `common::Song`.");
                        player.on_song_clicked(song).await;
                    }
                ));
            }
        ));
    }

    async fn show_save_error_dialog(&self, name: String, player: Player) {
        // TODO: translatable
        let diag = adw::AlertDialog::builder()
            .heading("Playlist Exists")
            .body(format!("A playlist named \"{}\" already exists. Would you like to overwrite or append to it?", &name))
            .build();
        diag.add_response("cancel", "_Cancel");
        diag.add_response("append", "_Append");
        diag.add_response("overwrite", "_Overwrite");
        diag.set_response_appearance("append", adw::ResponseAppearance::Suggested);
        diag.set_response_appearance("overwrite", adw::ResponseAppearance::Destructive);
        match diag
            .choose_future(self.imp().window.upgrade().as_ref())
            .await
            .as_str()
        {
            "append" => {
                player.save_queue(name, SaveMode::Append).await;
            }
            "overwrite" => {
                player.save_queue(name, SaveMode::Replace).await;
            }
            _ => {}
        };
    }

    /// Determine whether to present an empty placeholder or queue contents
    pub fn update_stack(&self, queue: &gio::ListStore) {
        if queue.n_items() > 0 {
            self.imp().content_stack.show_content();
        } else {
            self.imp().content_stack.show_placeholder();
        }
    }

    pub fn bind_state(&self, player: &Player) {
        let player_queue = player.queue();
        let queue_title = self.imp().queue_title.get();
        let clear_queue_btn = self.imp().clear_queue.get();
        let consume = self.imp().consume.get();
        let save = self.imp().save.get();
        let save_name = self.imp().save_name.get();
        let save_confirm = self.imp().save_confirm.get();
        player_queue
            .bind_property("n-items", &clear_queue_btn, "sensitive")
            .transform_to(|_, size: u32| Some(size > 0))
            .sync_create()
            .build();

        player_queue
            .bind_property("n-items", &queue_title, "subtitle")
            // TODO: l10n
            .transform_to(|_, size: u32| format_song_count(size))
            .sync_create()
            .build();

        player_queue.connect_notify_local(
            Some("n-items"),
            clone!(
                #[weak(rename_to = this)]
                self,
                move |queue, _| {
                    this.update_stack(queue);
                }
            ),
        );

        player
            .bind_property("supports-playlists", &save, "visible")
            .sync_create()
            .build();

        // Disgusting workaround until I can pinpoint whenever this is a GTK problem.
        self.imp()
            .scrolled_window
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

        save_name.connect_closure(
            "changed",
            false,
            closure_local!(
                #[weak]
                save_confirm,
                move |entry: gtk::Entry| { save_confirm.set_sensitive(entry.text_length() > 0) }
            ),
        );

        save_confirm.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            #[weak]
            save,
            #[weak]
            save_name,
            move |_| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    this,
                    #[weak]
                    player,
                    #[weak]
                    save,
                    async move {
                        // Close the popover first, then save.
                        save.set_active(false);
                        let name = save_name.buffer().text().as_str().to_owned();
                        match player.save_queue(name.clone(), SaveMode::Create).await {
                            Ok(()) => {}
                            Err(ClientError::Mpd(MpdError::Server(ServerError {
                                code: MpdErrorCode::Exist,
                                pos: _,
                                command: _,
                                detail: _,
                            }))) => this.show_save_error_dialog(name, player).await,
                            Err(e) => {
                                dbg!(e);
                            }
                        }
                    }
                ));
            }
        ));

        player
            .bind_property("consume", &consume, "icon-name")
            .transform_to(|_, is_consuming: bool| {
                if is_consuming {
                    Some("consume-on-symbolic")
                } else {
                    Some("consume-off-symbolic")
                }
            })
            .sync_create()
            .build();

        player
            .bind_property("consume", &consume, "tooltip-text")
            .transform_to(|_, is_consuming: bool| {
                // TODO: translatable
                if !is_consuming {
                    Some("Consume mode: off")
                } else {
                    Some("Consume mode: on. Songs will be removed from the queue once played.")
                }
            })
            .sync_create()
            .build();

        // Don't use bidirectional here or we'll fire once on UI init, erroneously resetting the state.
        player
            .bind_property("consume", &consume, "active")
            .sync_create()
            .build();

        consume.connect_clicked(clone!(
            #[weak]
            player,
            move |btn| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    #[weak]
                    btn,
                    async move {
                        btn.set_sensitive(false);
                        player.set_consume(btn.is_active()).await;
                        btn.set_sensitive(true);
                    }
                ));
            }
        ));

        clear_queue_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |btn| {
                glib::spawn_future_local(clone!(
                    #[weak]
                    player,
                    #[weak]
                    btn,
                    async move {
                        btn.set_sensitive(false);
                        player.clear_queue().await;
                        btn.set_sensitive(true);
                    }
                ));
            }
        ));
    }

    pub fn setup(
        &self,
        player: &Player,
        cache: Rc<Cache>,
        client_state: &ClientState,
        window: &EuphonicaWindow,
    ) {
        self.imp().window.set(Some(window));
        self.setup_listview(player, cache.clone());
        self.imp().player_pane.setup(player, cache, client_state);
        self.bind_state(player);
        self.imp().player.set(Some(player));
    }
}

impl LazyInit for QueueView {
    fn populate(&self) {
        if let Some(player) = self.imp().player.upgrade()
            && !player.queue_is_initialized()
            && !self.imp().initializing.get()
        {
            self.imp().initializing.set(true);
            glib::spawn_future_local(clone!(
                #[weak]
                player,
                #[weak(rename_to = this)]
                self,
                async move {
                    let stack = this.imp().content_stack.get();
                    stack.show_spinner();
                    player.update_queue().await;
                    this.imp().initializing.set(false);
                    this.update_stack(player.queue());
                }
            ));
        }
    }
}
