use glib::{clone, closure_local, Object, SignalHandlerId};
use gtk::{glib, prelude::*, subclass::prelude::*, CompositeTemplate};
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{placeholders::ALBUMART_THUMBNAIL_PLACEHOLDER, Cache, CacheState},
    common::{AlbumInfo, Song},
    utils::format_secs_as_duration,
};

use super::{Library, PlaylistContentView};

mod imp {
    use std::cell::Cell;

    use crate::library::PlaylistContentView;
    use glib::{ParamSpec, ParamSpecBoolean, ParamSpecString};
    use once_cell::sync::Lazy;

    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/library/playlist-song-row.ui")]
    pub struct PlaylistSongRow {
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub replace_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub append_queue: TemplateChild<gtk::Button>,
        #[template_child]
        pub raise: TemplateChild<gtk::Button>,
        #[template_child]
        pub lower: TemplateChild<gtk::Button>,
        #[template_child]
        pub remove: TemplateChild<gtk::Button>,
        #[template_child]
        pub thumbnail: TemplateChild<gtk::Image>,
        #[template_child]
        pub playlist_order: TemplateChild<gtk::Label>,
        #[template_child]
        pub song_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub artist_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub album_name: TemplateChild<gtk::Label>,
        #[template_child]
        pub duration: TemplateChild<gtk::Label>,
        // For unbinding the queue buttons when not bound to a song (i.e. being recycled)
        pub replace_queue_id: RefCell<Option<SignalHandlerId>>,
        pub append_queue_id: RefCell<Option<SignalHandlerId>>,
        pub thumbnail_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub raise_signal_id: RefCell<Option<SignalHandlerId>>,
        pub lower_signal_id: RefCell<Option<SignalHandlerId>>,
        pub remove_signal_id: RefCell<Option<SignalHandlerId>>,
        pub library: OnceCell<Library>,
        pub content_view: OnceCell<PlaylistContentView>,
        pub queue_controls_visible: Cell<bool>,
        pub edit_controls_visible: Cell<bool>,
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for PlaylistSongRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaPlaylistSongRow";
        type Type = super::PlaylistSongRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for PlaylistSongRow {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            for elem in [self.replace_queue.get(), self.append_queue.get()] {
                obj.bind_property("queue-controls-visible", &elem, "visible")
                    .sync_create()
                    .build();
            }

            for elem in [self.raise.get(), self.lower.get(), self.remove.get()] {
                obj.bind_property("edit-controls-visible", &elem, "visible")
                    .sync_create()
                    .build();
            }
        }

        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("order").build(),
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecString::builder("duration").build(),
                    // ParamSpecInt64::builder("disc").build(),
                    ParamSpecString::builder("quality-grade").build(),
                    ParamSpecBoolean::builder("queue-controls-visible").build(),
                    ParamSpecBoolean::builder("edit-controls-visible").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "order" => self.playlist_order.label().to_value(),
                "name" => self.song_name.label().to_value(),
                // "last_mod" => obj.get_last_mod().to_value(),
                "album" => self.album_name.label().to_value(),
                "artist" => self.artist_name.label().to_value(),
                "duration" => self.duration.label().to_value(),
                // "disc" => self.disc.get_label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                "queue-controls-visible" => self.queue_controls_visible.get().to_value(),
                "edit-controls-visible" => self.edit_controls_visible.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            match pspec.name() {
                "order" => {
                    // TODO: Handle no-name case here instead of in Song GObject for flexibility
                    if let Ok(new) = value.get::<&str>() {
                        self.playlist_order.set_label(new);
                        self.obj().notify("order");
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
                    }
                }
                "album" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.album_name.set_label(name);
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
                "queue-controls-visible" => {
                    if let Ok(new) = value.get::<bool>() {
                        self.obj().set_queue_controls_visible(new);
                    }
                }
                "edit-controls-visible" => {
                    if let Ok(new) = value.get::<bool>() {
                        self.obj().set_edit_controls_visible(new);
                    }
                }
                _ => unimplemented!(),
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for PlaylistSongRow {}

    // Trait shared by all boxes
    impl BoxImpl for PlaylistSongRow {}
}

glib::wrapper! {
    pub struct PlaylistSongRow(ObjectSubclass<imp::PlaylistSongRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl PlaylistSongRow {
    pub fn new(library: Library, view: PlaylistContentView, item: &gtk::ListItem) -> Self {
        let res: Self = Object::builder().build();
        res.setup(library, view, item);
        res
    }

    #[inline(always)]
    pub fn setup(&self, library: Library, view: PlaylistContentView, item: &gtk::ListItem) {
        let _ = self.imp().library.set(library);
        let _ = self.imp().content_view.set(view);
        item.property_expression("item")
            .chain_property::<Song>("queue-pos")
            .chain_closure::<String>(closure_local!(|_: Option<Object>, val: u32| {
                val.to_string()
            }))
            .bind(self, "order", gtk::Widget::NONE);
        item.property_expression("item")
            .chain_property::<Song>("name")
            .bind(self, "name", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("artist")
            .bind(self, "artist", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("album")
            .bind(self, "album", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("duration")
            .chain_closure::<String>(closure_local!(|_: Option<Object>, dur: u64| {
                format_secs_as_duration(dur as f64)
            }))
            .bind(self, "duration", gtk::Widget::NONE);

        item.property_expression("item")
            .chain_property::<Song>("quality-grade")
            .bind(self, "quality-grade", gtk::Widget::NONE);
    }

    fn update_thumbnail(&self, info: Option<&AlbumInfo>, cache: Rc<Cache>, schedule: bool) {
        if let Some(album) = info {
            // Should already have been downloaded by the album view
            if let Some(tex) = cache.load_cached_cover(album, true, schedule) {
                self.imp().thumbnail.set_paintable(Some(&tex));
                return;
            }
        }
        self.imp()
            .thumbnail
            .set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
    }

    pub fn bind(&self, song: &Song, cache: Rc<Cache>) {
        // Bind album art listener. Set once first (like sync_create)
        self.update_thumbnail(song.get_album(), cache.clone(), true);
        let cache_state = cache.get_cache_state();
        self.imp()
            .thumbnail_signal_ids
            .replace(Some((
                cache_state.connect_closure(
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
                                    this.update_thumbnail(Some(album), cache, false);
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
                        #[strong]
                        song,
                        move |_: CacheState, folder_uri: String| {
                            if let Some(album) = song.get_album() {
                                if album.uri == folder_uri {
                                    this.imp().thumbnail.set_paintable(Some(&*ALBUMART_THUMBNAIL_PLACEHOLDER));
                                }
                            }
                        }
                    ),
                )
            )));

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

        if let Some(old_id) =
            self.imp()
                .raise_signal_id
                .replace(Some(self.imp().raise.connect_clicked(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    song,
                    move |_| {
                        if let Some(view) = this.imp().content_view.get() {
                            view.shift_backward(song.get_queue_pos());
                        }
                    }
                ))))
        {
            // Unbind old ID
            self.imp().raise.disconnect(old_id);
        }

        if let Some(old_id) =
            self.imp()
                .lower_signal_id
                .replace(Some(self.imp().lower.connect_clicked(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    song,
                    move |_| {
                        if let Some(view) = this.imp().content_view.get() {
                            view.shift_forward(song.get_queue_pos());
                        }
                    }
                ))))
        {
            // Unbind old ID
            self.imp().lower.disconnect(old_id);
        }

        if let Some(old_id) =
            self.imp()
                .remove_signal_id
                .replace(Some(self.imp().remove.connect_clicked(clone!(
                    #[weak(rename_to = this)]
                    self,
                    #[strong]
                    song,
                    move |_| {
                        if let Some(view) = this.imp().content_view.get() {
                            view.remove(song.get_queue_pos());
                        }
                    }
                ))))
        {
            // Unbind old ID
            self.imp().remove.disconnect(old_id);
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

    pub fn get_queue_controls_visible(&self) -> bool {
        self.imp().queue_controls_visible.get()
    }

    pub fn set_queue_controls_visible(&self, new: bool) {
        let old = self.imp().queue_controls_visible.replace(new);
        if old != new {
            self.notify("queue-controls-visible");
        }
    }

    pub fn get_edit_controls_visible(&self) -> bool {
        self.imp().edit_controls_visible.get()
    }

    pub fn set_edit_controls_visible(&self, new: bool) {
        let old = self.imp().edit_controls_visible.replace(new);
        if old != new {
            self.notify("edit-controls-visible");
        }
    }
}
