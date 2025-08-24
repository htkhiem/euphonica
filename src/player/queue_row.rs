use glib::{closure_local, signal::SignalHandlerId, Object};
use gtk::{
    gdk, glib::{self, clone}, prelude::*, subclass::prelude::*, CompositeTemplate, Image, Label
};
use std::{cell::RefCell, rc::Rc};

use crate::{
    cache::{placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, Cache, CacheState},
    common::{CoverSource, Marquee, Song, SongInfo},
    utils::strip_filename_linux,
};

use super::{controller::SwapDirection, Player};

mod imp {
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecString, ParamSpecUInt};
    use gtk::{Button, Revealer};
    use once_cell::sync::Lazy;
    use std::cell::{Cell, OnceCell};

    use crate::cache::placeholders::{EMPTY_ALBUM_STRING, EMPTY_ARTIST_STRING};

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
        pub signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId, SignalHandlerId)>>,
        pub song: RefCell<Option<Song>>,
        pub player: OnceCell<Player>,
        pub cache: OnceCell<Rc<Cache>>,
        pub thumbnail_source: Cell<CoverSource>,
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
                    } else {
                        self.album_name.set_label(*EMPTY_ALBUM_STRING);
                    }
                }
                "artist" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.artist_name.set_label(name);
                    } else {
                        self.artist_name.set_label(*EMPTY_ARTIST_STRING);
                    }
                }
                "is-playing" => {
                    if let Ok(p) = value.get::<bool>() {
                        self.playing_indicator.set_reveal_child(p);
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

        fn dispose(&self) {
            if let Some((playing_id, set_id, clear_id)) = self.signal_ids.take() {
                self.player.get().unwrap().disconnect(playing_id);
                let cache_state = self.cache.get().unwrap().get_cache_state();
                cache_state.disconnect(set_id);
                cache_state.disconnect(clear_id);
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
    pub fn new(item: &gtk::ListItem, player: Player, cache: Rc<Cache>) -> Self {
        let res: Self = Object::builder().build();
        res.setup(item, player, cache);
        res
    }

    fn update_playing_status(&self, maybe_queue_id: Option<u32>) {
        if let (Some(id), Some(own_id)) = (
            maybe_queue_id,
            self.imp().song.borrow().as_ref().map(|s| s.get_queue_id())
        ) {
            self.imp().playing_indicator.set_reveal_child(id == own_id);
        } else {
            self.imp().playing_indicator.set_reveal_child(false);
        }
    }

    #[inline(always)]
    pub fn setup(&self, item: &gtk::ListItem, player: Player, cache: Rc<Cache>) {
        let cache_state = cache.get_cache_state();
        let _ = self.imp().cache.set(cache.clone());
        let _ = self.imp().player.set(player.clone());
        // Bind controls
        self.imp().remove.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            move |_| {
                if let Some(song) = this.imp().song.borrow().as_ref() {
                    player.remove_song_pos(song.get_queue_pos());
                }
            }
        ));

        self.imp().raise.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            move |_| {
                if let Some(song) = this.imp().song.borrow().as_ref() {
                    player.swap_dir(song.get_queue_pos(), SwapDirection::Up);
                }
            }
        ));

        self.imp().lower.connect_clicked(clone!(
            #[weak(rename_to = this)]
            self,
            #[weak]
            player,
            move |_| {
                if let Some(song) = this.imp().song.borrow().as_ref() {
                    player.swap_dir(song.get_queue_pos(), SwapDirection::Down);
                }
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

        let _ = self.imp().signal_ids.replace(Some((
            player.connect_notify_local(
                Some("queue-id"),
                clone!(
                    #[weak(rename_to = this)]
                    self,
                    move |player, _| {
                        this.update_playing_status(player.queue_id());
                    }
                )
            ),
            cache_state.connect_closure(
                "album-art-downloaded",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String, thumb: bool, tex: gdk::Texture| {
                        if !thumb {
                            return;
                        }
                        // Match song URI first then folder URI. Only try to match by folder URI
                        // if we don't have a current thumbnail.
                        if let Some(song) = this.imp().song.borrow().as_ref() {
                            if uri.as_str() == song.get_uri() {
                                // Force update since we might have been using a folder cover
                                // temporarily
                                this.update_thumbnail(tex, CoverSource::Embedded);
                            } else if this.imp().thumbnail_source.get() != CoverSource::Embedded {
                                if strip_filename_linux(song.get_uri()) == uri {
                                    this.update_thumbnail(tex, CoverSource::Folder);
                                }
                            }
                        }
                    }
                ),
            ),
            cache_state.connect_closure(
                "album-art-cleared",
                false,
                closure_local!(
                    #[weak(rename_to = this)]
                    self,
                    move |_: CacheState, uri: String| {
                        if let Some(song) = this.imp().song.borrow().as_ref() {
                            match this.imp().thumbnail_source.get() {
                                CoverSource::Folder => {
                                    if strip_filename_linux(song.get_uri()) == uri {
                                        this.clear_thumbnail();
                                    }
                                }
                                CoverSource::Embedded => {
                                    if song.get_uri() == &uri {
                                        this.clear_thumbnail();
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                ),
            ),
        )));

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

    fn clear_thumbnail(&self) {
        self.imp().thumbnail_source.set(CoverSource::None);
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
    }

    fn schedule_thumbnail(&self, info: &SongInfo) {
        self.imp().thumbnail_source.set(CoverSource::Unknown);
        self.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
        if let Some((tex, is_embedded)) = self
            .imp()
            .cache
            .get()
            .unwrap()
            .clone()
            .load_cached_embedded_cover(info, true, true) {
                self.imp().thumbnail.set_paintable(Some(&tex));
                self.imp().thumbnail_source.set(
                    if is_embedded {CoverSource::Embedded} else {CoverSource::Folder}
                );
            }
    }

    fn update_thumbnail(&self, tex: gdk::Texture, src: CoverSource) {
        self.imp().thumbnail.set_paintable(Some(&tex));
        self.imp().thumbnail_source.set(src);
    }

    pub fn bind(&self, song: &Song) {
        // The string properties are bound using property expressions in setup().
        // Here we only need to manually bind to the cache controller to fetch album art.
        // No need to fetch here as QueueView has already done once for us.
        self.imp().song.replace(Some(song.clone()));
        self.update_playing_status(self.imp().player.get().unwrap().queue_id());
        self.schedule_thumbnail(song.get_info());
    }

    pub fn unbind(&self) {
        if let Some(_) = self.imp().song.take() {
            self.clear_thumbnail();
        }
    }
}
