use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gio::glib::closure_local;
use glib::clone;
use gtk::{CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection, gio, glib};
use mpd::{
    SaveMode,
    error::{Error as MpdError, ErrorCode as MpdErrorCode, ServerError},
};

use super::PlayerPane;

use crate::{
    cache::Cache,
    client::ClientState,
    common::{RowEditButtons, Song, SongRow},
    player::controller::SwapDirection,
    utils::LazyInit,
    window::EuphonicaWindow,
};

use super::Player;

mod imp {
    use std::{
        cell::{Cell, OnceCell},
        sync::OnceLock,
    };

    use ::glib::WeakRef;
    use glib::{Properties, subclass::Signal};

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
        pub content_stack: TemplateChild<gtk::Stack>,
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

        pub window: OnceCell<EuphonicaWindow>,

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
        pub initialized: Cell<bool>,
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
        }

        fn constructed(&self) {
            self.parent_constructed();
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
                        if let Some(player) = obj.imp().player.upgrade() {
                            player.rate_current_song(None);
                        }
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

impl QueueView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn setup_listview(&self, player: &Player, cache: Rc<Cache>) {
        // Enable/disable clear queue button depending on whether the queue is empty or not
        // Set selection mode
        // TODO: Allow click to jump to song
        let queue_model = player.queue().clone();
        let stack = self.imp().content_stack.get();
        queue_model
            .bind_property("n-items", &stack, "visible-child-name")
            .transform_to(|_, val: u32| {
                if val == 0 {
                    Some("empty")
                } else {
                    Some("queue")
                }
            })
            .sync_create()
            .build();
        let sel_model = SingleSelection::new(Some(queue_model));
        self.imp().queue.set_model(Some(&sel_model));

        // Set up factory
        let factory = SignalListItemFactory::new();

        factory.connect_setup(clone!(
            #[weak]
            player,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let row = SongRow::new(Some(cache.clone()), Some(&player));
                row.set_index_visible(false);
                row.set_playing_indicator_visible(true);

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

                item.property_expression("item")
                    .chain_property::<Song>("quality-grade")
                    .bind(&row, "quality-grade", gtk::Widget::NONE);
                let end_widget = RowEditButtons::new(
                    item,
                    // Raise action
                    clone!(
                        #[weak]
                        player,
                        #[upgrade_or]
                        (),
                        move |idx| {
                            player.swap_dir(idx, SwapDirection::Up);
                        }
                    ),
                    clone!(
                        #[weak]
                        player,
                        #[upgrade_or]
                        (),
                        move |idx| {
                            player.swap_dir(idx, SwapDirection::Down);
                        }
                    ),
                    clone!(
                        #[weak]
                        player,
                        #[upgrade_or]
                        (),
                        move |idx| {
                            player.remove_pos(idx);
                        }
                    ),
                );
                row.set_end_widget(Some(&end_widget.into()));
                item.set_child(Some(&row));
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
                let model = queue.model().expect("The model has to exist.");
                let song = model
                    .item(position)
                    .and_downcast::<Song>()
                    .expect("The item has to be a `common::Song`.");
                player.on_song_clicked(song);
            }
        ));
    }

    fn show_save_error_dialog(&self, name: String, player: Player) {
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
        diag.choose(
            self.imp().window.get().unwrap(),
            Option::<gio::Cancellable>::None.as_ref(),
            move |resp| match resp.as_str() {
                "append" => {
                    let _ = player.save_queue(&name, SaveMode::Append);
                }
                "overwrite" => {
                    let _ = player.save_queue(&name, SaveMode::Replace);
                }
                _ => {}
            },
        );
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
            move |_| {
                // Close the popover first, then save.
                save.set_active(false);
                let name = save_name.buffer().text().as_str().to_owned();
                match player.save_queue(&name, SaveMode::Create) {
                    Ok(()) => {}
                    Err(e) => {
                        if let Some(MpdError::Server(ServerError {
                            code: MpdErrorCode::Exist,
                            pos: _,
                            command: _,
                            detail: _,
                        })) = e
                        {
                            this.show_save_error_dialog(name, player);
                        }
                    }
                }
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
            move |btn| { player.set_consume(btn.is_active()) }
        ));

        clear_queue_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                player.clear_queue();
            }
        ));
    }

    pub fn setup(
        &self,
        player: &Player,
        cache: Rc<Cache>,
        client_state: &ClientState,
        window: EuphonicaWindow,
    ) {
        let _ = self.imp().window.set(window);
        self.setup_listview(player, cache);
        self.imp().player_pane.setup(player, client_state);
        self.bind_state(player);
        self.imp().player.set(Some(player));
    }
}

impl LazyInit for QueueView {
    fn populate(&self) {
        if let Some(player) = self.imp().player.upgrade() {
            if let Some(status) = player.client().get_status(true) {
                player.update_status(&status);
            }
        }
    }
}
