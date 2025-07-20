use std::rc::Rc;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gio::glib::closure_local;
use glib::clone;
use gtk::{
    gio, glib, CompositeTemplate, ListItem, SignalListItemFactory, SingleSelection,
};
use mpd::{
    error::{Error as MpdError, ErrorCode as MpdErrorCode, ServerError},
    SaveMode,
};

use super::PlayerPane;

use crate::{cache::Cache, common::Song, window::EuphonicaWindow};

use super::{Player, QueueRow};

mod imp {
    use std::{cell::{Cell, OnceCell}, sync::OnceLock};

    use glib::{subclass::Signal, Properties};

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
        pub restore_last_pos: Cell<u8>
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
                .bind_property(
                    "collapsed",
                    &self.show_sidebar.get(),
                    "visible"
                )
                .sync_create()
                .build();

            self.show_sidebar.connect_clicked(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.obj().emit_by_name::<()>("show-sidebar-clicked", &[]);
                }
            ));
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: OnceLock<Vec<Signal>> = OnceLock::new();
            SIGNALS.get_or_init(|| {
                vec![
                    Signal::builder("show-sidebar-clicked").build(),
                ]
            })
        }
    }

    impl WidgetImpl for QueueView {}
}

glib::wrapper! {
    pub struct QueueView(ObjectSubclass<imp::QueueView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
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
    } else {
        if count == 1 {
            Some(String::from("1 song"))
        } else {
            Some(format!("{} songs", count))
        }
    }
}

impl QueueView {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn setup_listview(&self, player: Player, cache: Rc<Cache>) {
        // Enable/disable clear queue button depending on whether the queue is empty or not
        // Set selection mode
        // TODO: Allow click to jump to song
        let queue_model = player.queue();
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

        // Create an empty `QueueRow` during setup
        factory.connect_setup(clone!(
            #[weak]
            player,
            #[weak]
            cache,
            move |_, list_item| {
                let item = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem");
                let queue_row = QueueRow::new(&item, player, cache);
                item.set_child(Some(&queue_row));
            }
        ));
        // Tell factory how to bind `QueueRow` to one of our Song GObjects
        factory.connect_bind(move |_, list_item| {
            // Get `Song` from `ListItem` (that is, the data side)
            let item: Song = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .item()
                .and_downcast::<Song>()
                .expect("The item has to be a common::Song.");

            // Get `QueueRow` from `ListItem` (the UI widget)
            let child: QueueRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<QueueRow>()
                .expect("The child has to be a `QueueRow`.");

            // Within this binding fn is where the cached album art texture gets used.
            child.bind(&item);
        });

        // When row goes out of sight, unbind from item to allow reuse with another.
        // Remember to also unset the thumbnail widget's texture to potentially free it from memory.
        factory.connect_unbind(move |_, list_item| {
            // Get `QueueRow` from `ListItem` (the UI widget)
            let child: QueueRow = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem")
                .child()
                .and_downcast::<QueueRow>()
                .expect("The child has to be a `QueueRow`.");
            child.unbind();
        });

        factory.connect_teardown(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _| {
                // The above scroll bug only manifests after this, so now is the best time to set
                // the corresponding values.
                this.imp().last_scroll_pos.set(this.imp().scrolled_window.vadjustment().value());
                this.imp().restore_last_pos.set(2);
            }
        ));

        // Set the factory of the list view
        self.imp().queue.set_factory(Some(&factory));

        // Setup click action
        self.imp().queue.connect_activate(move |queue, position| {
            let model = queue.model().expect("The model has to exist.");
            let song = model
                .item(position)
                .and_downcast::<Song>()
                .expect("The item has to be a `common::Song`.");
            player.on_song_clicked(song);
        });
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

    pub fn bind_state(&self, player: Player) {
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
        self.imp().scrolled_window.vadjustment().connect_notify_local(
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
                    Err(e) => match e {
                        Some(MpdError::Server(ServerError {
                            code: MpdErrorCode::Exist,
                            pos: _,
                            command: _,
                            detail: _,
                        })) => {
                            this.show_save_error_dialog(name, player);
                        }
                        _ => {}
                    },
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

        consume
            .bind_property("active", &player, "consume")
            .bidirectional()
            .sync_create()
            .build();

        clear_queue_btn.connect_clicked(clone!(
            #[weak]
            player,
            move |_| {
                player.clear_queue();
            }
        ));
    }

    pub fn setup(&self, player: Player, cache: Rc<Cache>, window: EuphonicaWindow) {
        let _ = self.imp().window.set(window);
        self.setup_listview(player.clone(), cache);
        self.imp().player_pane.setup(&player);
        self.bind_state(player);
    }
}
