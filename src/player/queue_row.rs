use glib::{closure_local, signal::SignalHandlerId, Object};
use gtk::{
    glib::{self, clone},
    prelude::*,
    subclass::prelude::*,
    CompositeTemplate, Image, Label,
};
use std::{cell::RefCell, rc::Rc};

use crate::{
    cache::{placeholders::ALBUMART_PLACEHOLDER, Cache, CacheState},
    common::{AlbumInfo, Song},
};

use super::{controller::SwapDirection, Player};

mod imp {
    use std::cell::Cell;

    use crate::common::Marquee;
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecString, ParamSpecUInt};
    use gtk::{Button, Revealer};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/player/queue-row.ui")]
    pub struct QueueRow {
        #[template_child]
        pub thumbnail: TemplateChild<Image>,
        #[template_child]
        pub song_name: TemplateChild<Marquee>,
        #[template_child]
        pub album_name: TemplateChild<Label>,
        #[template_child]
        pub artist_name: TemplateChild<Label>,
        #[template_child]
        pub playing_indicator: TemplateChild<Revealer>,
        #[template_child]
        pub raise: TemplateChild<Button>,
        #[template_child]
        pub lower: TemplateChild<Button>,
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub remove: TemplateChild<Button>,
        pub queue_id: Cell<u32>,
        pub thumbnail_signal_id: RefCell<Option<SignalHandlerId>>,
        // pub marquee_tick_callback_id: RefCell<Option<TickCallbackId>>,
        // pub marquee_forward: Cell<bool>,
        // pub marquee_progress: Cell<f64>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for QueueRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaQueueRow";
        type Type = super::QueueRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for QueueRow {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecBoolean::builder("is-playing").build(),
                    ParamSpecUInt::builder("queue-id").build(),
                    // ParamSpecString::builder("duration").build(),
                    ParamSpecString::builder("quality-grade").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "name" => self.song_name.label().label().to_value(),
                "artist" => self.artist_name.label().to_value(),
                "album" => self.album_name.label().to_value(),
                "is-playing" => self.playing_indicator.is_child_revealed().to_value(),
                "queue-id" => self.queue_id.get().to_value(),
                // "duration" => self.duration.label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "name" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(name) = value.get::<&str>() {
                        self.song_name.label().set_label(name);
                    }
                }
                "album" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.album_name.set_label(name);
                    }
                }
                "artist" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.artist_name.set_label(name);
                    }
                }
                "is-playing" => {
                    if let Ok(p) = value.get::<bool>() {
                        self.playing_indicator.set_reveal_child(p);
                    }
                }
                "queue-id" => {
                    if let Ok(id) = value.get::<u32>() {
                        self.queue_id.replace(id);
                    }
                }
                // "duration" => {
                //     // Pre-formatted please
                //     if let Ok(dur) = value.get::<&str>() {
                //         self.duration.set_label(dur);
                //     }
                // }
                "quality-grade" => {
                    if let Ok(icon) = value.get::<&str>() {
                        self.quality_grade.set_icon_name(Some(icon));
                        self.quality_grade.set_visible(true);
                    } else {
                        self.quality_grade.set_icon_name(None);
                        self.quality_grade.set_visible(false);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for QueueRow {}

    // Trait shared by all boxes
    impl BoxImpl for QueueRow {}
}

glib::wrapper! {
    pub struct QueueRow(ObjectSubclass<imp::QueueRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl QueueRow {
    pub fn new(item: &gtk::ListItem, player: Player) -> Self {
        let res: Self = Object::builder().build();
        res.setup(item, player);
        res
    }

    #[inline(always)]
    pub fn setup(&self, item: &gtk::ListItem, player: Player) {
        // Bind controls
        self.imp().remove.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            move |_| {
                player.remove_song_id(this.imp().queue_id.get());
            }
        ));

        self.imp().raise.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            move |_| {
                player.swap_dir(this.imp().queue_id.get(), SwapDirection::Up);
            }
        ));

        self.imp().lower.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            move |_| {
                player.swap_dir(this.imp().queue_id.get(), SwapDirection::Down);
            }
        ));

        item.property_expression("item")
            .chain_property::<Song>("name")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("album")
            .bind(self, "album", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("artist")
            .bind(self, "artist", gtk::Widget::NONE);

        // item
        //     .property_expression("item")
        //     .chain_property::<Song>("duration")
        //     .chain_closure::<String>(closure_local!(|_: Option<Object>, dur: u64| {
        //         format_secs_as_duration(dur as f64)
        //     }))
        //     .bind(self, "duration", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("quality-grade")
            .bind(self, "quality-grade", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("is-playing")
            .bind(self, "is-playing", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("queue-id")
            .bind(self, "queue-id", gtk::Widget::NONE);

        // Bind marquee controller only once here
        // Run only while hovered
        let hover_ctl = gtk::EventControllerMotion::new();
        hover_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
        hover_ctl.connect_enter(clone!(
            #[weak(rename_to = this)]
            self,
            move |_, _, _| {
                this.imp().song_name.set_should_run_and_check(true);
            }
        ));
        hover_ctl.connect_leave(clone!(
            #[weak(rename_to = this)]
            self,
            move |_| {
                this.imp().song_name.set_should_run_and_check(false);
            }
        ));
        self.add_controller(hover_ctl);
    }

    fn update_thumbnail(&self, info: Option<&AlbumInfo>, cache: Rc<Cache>, schedule: bool) {
        if let Some(album) = info {
            if let Some(tex) = cache.load_cached_album_art(album, true, schedule) {
                self.imp().thumbnail.set_paintable(Some(&tex));
                return;
            }
        }
        self.imp()
            .thumbnail
            .set_paintable(Some(&*ALBUMART_PLACEHOLDER))
    }

    pub fn bind(&self, song: &Song, cache: Rc<Cache>) {
        // The string properties are bound using property expressions in setup().
        // Here we only need to manually bind to the cache controller to fetch album art.
        // Set once first (like sync_create)
        // We need schedule = True here since the QueueView only requested caching the entire
        // queue's worth of album arts once (at the beginning), and by now some might have been
        // evicted from the cache.
        self.update_thumbnail(song.get_album(), cache.clone(), true);
        let thumbnail_binding = cache.get_cache_state().connect_closure(
            "album-art-downloaded",
            false,
            closure_local!(
                #[weak(rename_to = this)]
                self,
                #[strong]
                song,
                #[weak]
                cache,
                move |_: CacheState, folder_uri: String| {
                    if let Some(album) = song.get_album() {
                        if album.uri == folder_uri {
                            // If we have been notified and yet the cache does not contain
                            // the corresponding art, then we either failed to fetch the
                            // art to begin with or the cache is experiencing serious thrashing.
                            // Do not attempt to re-schedule.
                            this.update_thumbnail(Some(album), cache, false);
                        }
                    }
                }
            ),
        );
        self.imp()
            .thumbnail_signal_id
            .replace(Some(thumbnail_binding));
    }

    pub fn unbind(&self, cache: Rc<Cache>) {
        if let Some(id) = self.imp().thumbnail_signal_id.take() {
            cache.get_cache_state().disconnect(id);
        }
    }
}
