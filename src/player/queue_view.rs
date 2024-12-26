use std::rc::Rc;

use adw::subclass::prelude::*;
use gtk::{
    prelude::*,
    gio,
    glib,
    CompositeTemplate,
    SingleSelection,
    SignalListItemFactory,
    ListItem,
};
use glib::clone;
use super::PlayerPane;

use crate::{
    cache::Cache,
    common::Song
};

use super::{
    QueueRow,
    Player,
};

mod imp {
    use std::cell::Cell;

    use glib::Properties;

    use super::*;

    #[derive(Debug, Properties, Default, CompositeTemplate)]
    #[properties(wrapper_type = super::QueueView)]
    #[template(resource = "/org/euphonica/Euphonica/gtk/player/queue-view.ui")]
    pub struct QueueView {
        #[template_child]
        pub queue_pane_view: TemplateChild<adw::NavigationSplitView>,
        #[template_child]
        pub content_stack: TemplateChild<gtk::Stack>,
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
        #[property(get, set)]
        pub collapsed: Cell<bool>,
        #[property(get, set)]
        pub show_content: Cell<bool>
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
            obj
                .bind_property("collapsed", &self.queue_pane_view.get(), "collapsed")
                .sync_create()
                .build();

            self.queue_pane_view
                .bind_property(
                    "show-content",
                    obj.as_ref(),
                    "show-content"
                )
                .bidirectional()
                .sync_create()
                .build();
        }
    }

    impl WidgetImpl for QueueView {}
}

glib::wrapper! {
    pub struct QueueView(ObjectSubclass<imp::QueueView>)
        @extends gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for QueueView {
    fn default() -> Self {
        glib::Object::new()
    }
}

fn format_song_count(count: u32) -> Option<String> {
    // TODO: translatable
    if count == 0 {
        None
    }
    else {
        if count == 1 {
            Some(String::from("1 song"))
        }
        else {
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
                }
                else {
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
            move |_, list_item| {
            let item = list_item
                .downcast_ref::<ListItem>()
                .expect("Needs to be ListItem");
            let queue_row = QueueRow::new(&item, player);
            item.set_child(Some(&queue_row));
        }));
        // Tell factory how to bind `QueueRow` to one of our Song GObjects
        factory.connect_bind(clone!(
            #[weak]
            cache,
            move |_, list_item| {
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
                child.bind(&item, cache.clone());
            })
        );

        // When row goes out of sight, unbind from item to allow reuse with another.
        // Remember to also unset the thumbnail widget's texture to potentially free it from memory.
        factory.connect_unbind(clone!(
            #[weak]
            cache,
            move |_, list_item| {
                // Get `QueueRow` from `ListItem` (the UI widget)
                let child: QueueRow = list_item
                    .downcast_ref::<ListItem>()
                    .expect("Needs to be ListItem")
                    .child()
                    .and_downcast::<QueueRow>()
                    .expect("The child has to be a `QueueRow`.");
                child.unbind(cache);
            })
        );

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

    pub fn bind_state(&self, player: Player) {
        let player_queue = player.queue();
        let queue_title = self.imp().queue_title.get();
        let clear_queue_btn = self.imp().clear_queue.get();
        let consume = self.imp().consume.get();
        player_queue
            .bind_property(
                "n-items",
                &clear_queue_btn,
                "sensitive"
            )
            .transform_to(|_, size: u32| {Some(size > 0)})
            .sync_create()
            .build();

        player_queue
            .bind_property(
                "n-items",
                &queue_title,
                "subtitle"
            )
            // TODO: l10n
            .transform_to(|_, size: u32| {format_song_count(size)})
            .sync_create()
            .build();

        consume
            .bind_property(
                "active",
                &player,
                "consume"
            )
            .bidirectional()
            .sync_create()
            .build();

        clear_queue_btn.connect_clicked(clone!(#[weak] player, move |_| {
            player.clear_queue();
        }));
    }

    pub fn setup(&self, player: Player, cache: Rc<Cache>) {
        self.setup_listview(player.clone(), cache);
        self.imp().player_pane.setup(player.clone());
        self.bind_state(player);
    }
}
