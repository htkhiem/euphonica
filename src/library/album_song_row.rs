use glib::{clone, closure, Object, SignalHandlerId};
use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate, Label};
use std::cell::{OnceCell, RefCell};

use crate::{common::Song, utils::format_secs_as_duration};

use super::Library;

mod imp {
    use crate::cache::placeholders::EMPTY_ARTIST_STRING;

    use super::*;
    use glib::{ParamSpec, ParamSpecInt64, ParamSpecString};
    use once_cell::sync::Lazy;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/album-song-row.ui")]
    pub struct AlbumSongRow {
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub track_index: TemplateChild<Label>,
        #[template_child]
        pub song_name: TemplateChild<Label>,
        // Compilation/best-of albums usually have different artists.
        #[template_child]
        pub artist_name: TemplateChild<Label>,
        #[template_child]
        pub duration: TemplateChild<Label>,
        // For unbinding the queue buttons when not bound to a song (i.e. being recycled)
        pub replace_queue_id: RefCell<Option<SignalHandlerId>>,
        pub append_queue_id: RefCell<Option<SignalHandlerId>>,
        pub library: OnceCell<Library>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for AlbumSongRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaAlbumSongRow";
        type Type = super::AlbumSongRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for AlbumSongRow {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecInt64::builder("track").build(),
                    ParamSpecString::builder("name").build(),
                    // ParamSpecString::builder("last_mod").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("duration").build(),
                    // ParamSpecInt64::builder("disc").build(),
                    ParamSpecString::builder("quality-grade").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "track" => self
                    .track_index
                    .label()
                    .parse::<i64>()
                    .unwrap_or(-1)
                    .to_value(),
                "name" => self.song_name.label().to_value(),
                // "last_mod" => obj.get_last_mod().to_value(),
                // Represented in MusicBrainz format, i.e. Composer; Performer, Performer,...
                // The composer part is optional.
                "artist" => self.artist_name.label().to_value(),
                "duration" => self.duration.label().to_value(),
                // "disc" => self.disc.get_label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "track" => {
                    if let Ok(v) = value.get::<i64>() {
                        if v >= 0 {
                            self.track_index.set_label(&v.to_string());
                            self.track_index.set_visible(true);
                        } else {
                            self.track_index.set_label("");
                            self.track_index.set_visible(false);
                        }
                    }
                }
                "name" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(name) = value.get::<&str>() {
                        self.song_name.set_label(name);
                    }
                }
                "artist" => {
                    if let Ok(tag) = value.get::<&str>() {
                        self.artist_name.set_label(tag);
                    } else {
                        self.artist_name.set_label(*EMPTY_ARTIST_STRING);
                    }
                }
                "duration" => {
                    // Pre-formatted please
                    if let Ok(dur) = value.get::<&str>() {
                        self.duration.set_label(dur);
                    }
                }
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
    impl WidgetImpl for AlbumSongRow {}

    // Trait shared by all boxes
    impl BoxImpl for AlbumSongRow {}
}

glib::wrapper! {
    pub struct AlbumSongRow(ObjectSubclass<imp::AlbumSongRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl AlbumSongRow {
    pub fn new(library: Library, item: &gtk::ListItem) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, item);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, item: &gtk::ListItem) {
        let _ = self.imp().library.set(library);
        item.property_expression("item")
            .chain_property::<Song>("track")
            .bind(self, "track", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("name")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("artist")
            .bind(self, "artist", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("duration")
            .chain_closure::<String>(closure!(|_: Option<Object>, dur: u64| {
                format_secs_as_duration(dur as f64)
            }))
            .bind(self, "duration", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("quality-grade")
            .bind(self, "quality-grade", gtk::Widget::NONE);
    }

    pub fn bind(&self, song: &Song) {
        // Bind the queue buttons
        let uri = song.get_uri().to_owned();
        if let Some(old_id) =
            self.imp()
                .replace_queue_id
                .replace(Some(self.imp().replace_queue.connect_clicked(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    uri,
                    move |_| {
                        if let Some(library) = this.imp().library.get() {
                            library.queue_uri(&uri, true, true, false);
                        }
                    }
                ))))
        {
            // Unbind old ID
            self.imp().replace_queue.disconnect(old_id);
        }
        if let Some(old_id) =
            self.imp()
                .append_queue_id
                .replace(Some(self.imp().append_queue.connect_clicked(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    uri,
                    move |_| {
                        if let Some(library) = this.imp().library.get() {
                            library.queue_uri(&uri, false, false, false);
                        }
                    }
                ))))
        {
            // Unbind old ID
            self.imp().append_queue.disconnect(old_id);
        }
    }

    pub fn unbind(&self) {
        if let Some(id) = self.imp().replace_queue_id.borrow_mut().take() {
            self.imp().replace_queue.disconnect(id);
        }
        if let Some(id) = self.imp().append_queue_id.borrow_mut().take() {
            self.imp().append_queue.disconnect(id);
        }
    }
}
