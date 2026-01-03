use glib::{
    Object, ParamSpec, ParamSpecBoolean, ParamSpecObject, ParamSpecString, SignalHandlerId,
    WeakRef, clone, closure_local,
};
use gtk::{CompositeTemplate, gdk, glib, prelude::*, subclass::prelude::*};
use once_cell::sync::Lazy;
use std::{
    cell::{OnceCell, RefCell},
    rc::Rc,
};

use crate::{
    cache::{Cache, CacheState},
    common::{QualityGrade, ImageStack, ImageState, Marquee, Song},
    player::Player,
};

// Wrapper around the common row object to implement song thumbnail fetch logic.
mod imp {
    use super::*;

    #[derive(Default, CompositeTemplate)]
    #[template(resource = "/io/github/htkhiem/Euphonica/gtk/song-row.ui")]
    pub struct SongRow {
        #[template_child]
        pub playing_indicator: TemplateChild<gtk::Revealer>,
        #[template_child]
        pub index: TemplateChild<gtk::Label>,
        #[template_child]
        pub quality_grade: TemplateChild<gtk::Image>,
        #[template_child]
        pub center_box: TemplateChild<gtk::CenterBox>,
        #[template_child]
        pub thumbnail: TemplateChild<ImageStack>,
        #[template_child]
        pub name: TemplateChild<Marquee>,
        #[template_child]
        pub first_attrib_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub first_attrib_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub second_attrib_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub second_attrib_text: TemplateChild<gtk::Label>,
        #[template_child]
        pub third_attrib_icon: TemplateChild<gtk::Image>,
        #[template_child]
        pub third_attrib_text: TemplateChild<gtk::Label>,
        pub song: WeakRef<Song>,
        pub thumbnail_signal_ids: RefCell<Option<(SignalHandlerId, SignalHandlerId)>>,
        pub playing_signal_id: RefCell<Option<SignalHandlerId>>,
        pub cache: OnceCell<Rc<Cache>>,
        pub player: WeakRef<Player>
    }

    // The central trait for subclassing a GObject
    #[glib::object_subclass]
    impl ObjectSubclass for SongRow {
        // `NAME` needs to match `class` attribute of template
        const NAME: &'static str = "EuphonicaSongRow";
        type Type = super::SongRow;
        type ParentType = gtk::Box;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    // Trait shared by all GObjects
    impl ObjectImpl for SongRow {
        fn constructed(&self) {
            self.parent_constructed();

            // Run marquee only while hovered
            let hover_ctl = gtk::EventControllerMotion::new();
            hover_ctl.set_propagation_phase(gtk::PropagationPhase::Capture);
            hover_ctl.connect_enter(clone!(
                #[weak(rename_to = this)]
                self,
                move |_, _, _| {
                    this.name.set_should_run_and_check(true);
                }
            ));
            hover_ctl.connect_leave(clone!(
                #[weak(rename_to = this)]
                self,
                move |_| {
                    this.name.set_should_run_and_check(false);
                }
            ));
            self.obj().add_controller(hover_ctl);
        }
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecBoolean::builder("playing-indicator-visible").build(),
                    ParamSpecBoolean::builder("is-playing").build(),
                    ParamSpecBoolean::builder("index-visible").build(),
                    ParamSpecString::builder("index").build(),
                    ParamSpecBoolean::builder("thumbnail-visible").build(),
                    ParamSpecString::builder("name").build(),
                    ParamSpecString::builder("quality-grade").build(),
                    ParamSpecString::builder("first-attrib-icon-name").build(),
                    ParamSpecString::builder("second-attrib-icon-name").build(),
                    ParamSpecString::builder("third-attrib-icon-name").build(),
                    ParamSpecString::builder("first-attrib-text").build(),
                    ParamSpecString::builder("second-attrib-text").build(),
                    ParamSpecString::builder("third-attrib-text").build(),
                    ParamSpecObject::builder::<gtk::Widget>("end-widget").build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            match pspec.name() {
                "playing-indicator-visible" => self.playing_indicator.is_visible().to_value(),
                "is-playing" => self.playing_indicator.is_child_revealed().to_value(),
                "index-visible" => self.thumbnail.is_visible().to_value(),
                "index" => self.index.label().to_value(),
                "thumbnail-visible" => self.thumbnail.is_visible().to_value(),
                "name" => self.name.label().label().to_value(),
                "quality-grade" => self.quality_grade.icon_name().to_value(),
                "first-attrib-icon-name" => self.first_attrib_icon.icon_name().to_value(),
                "second-attrib-icon-name" => self.second_attrib_icon.icon_name().to_value(),
                "third-attrib-icon-name" => self.third_attrib_icon.icon_name().to_value(),
                "first-attrib-text" => self.first_attrib_text.label().to_value(),
                "second-attrib-text" => self.second_attrib_text.label().to_value(),
                "third-attrib-text" => self.third_attrib_text.label().to_value(),
                "end-widget" => self.center_box.end_widget().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "playing-indicator-visible" => {
                    if let Ok(vis) = value.get::<bool>() {
                        self.playing_indicator.set_visible(vis);
                    }
                }
                "is-playing" => {
                    if let Ok(vis) = value.get::<bool>() {
                        self.playing_indicator.set_reveal_child(vis);
                    }
                }
                "index-visible" => {
                    if let Ok(vis) = value.get::<bool>() {
                        self.index.set_visible(vis);
                    }
                }
                "index" => {
                    if let Ok(idx) = value.get::<&str>() {
                        self.index.set_label(idx);
                    }
                }
                "thumbnail-visible" => {
                    if let Ok(vis) = value.get::<bool>() {
                        self.thumbnail.set_visible(vis);
                    }
                }
                "name" => {
                    if let Ok(name) = value.get::<&str>() {
                        self.name.label().set_label(name);
                    }
                }
                "quality-grade" => {
                    let maybe_icon = value.get::<&str>();
                    self.quality_grade.set_visible(maybe_icon.is_ok());
                    self.quality_grade.set_icon_name(maybe_icon.ok());
                }
                "first-attrib-icon-name" => {
                    obj.set_first_attrib_icon_name(value.get::<&str>().ok());
                }
                "second-attrib-icon-name" => {
                    obj.set_second_attrib_icon_name(value.get::<&str>().ok());
                }
                "third-attrib-icon-name" => {
                    obj.set_third_attrib_icon_name(value.get::<&str>().ok());
                }
                "first-attrib-text" => {
                    obj.set_first_attrib_text(value.get::<&str>().ok());
                }
                "second-attrib-text" => {
                    obj.set_second_attrib_text(value.get::<&str>().ok());
                }
                "third-attrib-text" => {
                    obj.set_third_attrib_text(value.get::<&str>().ok());
                }
                "end-widget" => {
                    obj.set_end_widget(value.get::<gtk::Widget>().ok().as_ref());
                }
                _ => unimplemented!(),
            }
        }

        fn dispose(&self) {
            if let (Some(cache), Some((set_id, clear_id))) =
                (self.cache.get(), self.thumbnail_signal_ids.take())
            {
                let cache_state = cache.get_cache_state();
                cache_state.disconnect(set_id);
                cache_state.disconnect(clear_id);
            }
            if let (Some(player), Some(id)) = (self.player.upgrade(), self.playing_signal_id.take())
            {
                player.disconnect(id);
            }
        }
    }

    // Trait shared by all widgets
    impl WidgetImpl for SongRow {}

    // Trait shared by all boxes
    impl BoxImpl for SongRow {}
}

// Common row widget for displaying a single song, used across the UI.
glib::wrapper! {
    pub struct SongRow(ObjectSubclass<imp::SongRow>)
    @extends gtk::Box, gtk::Widget,
    @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget, gtk::Orientable;
}

impl SongRow {
    pub fn new(
        // If not given, will not set up thumbnail fetching
        cache: Option<Rc<Cache>>,
        // If not given, will not set up is-playing indicator
        player: Option<&Player>,
    ) -> Self {
        let res: Self = Object::builder().build();
        if let Some(cache) = cache {
            let cache_state = cache.get_cache_state();
            let _ = res.imp().cache.set(cache);
            let _ = res.imp().thumbnail_signal_ids.replace(Some((
                cache_state.connect_closure(
                    "folder-cover-set",
                    false,
                    closure_local!(
                        #[weak]
                        res,
                        move |_: CacheState, uri: String, _: gdk::Texture, thumb: gdk::Texture| {
                            // This signal is only emitted when an album cover is set manually.
                            // This only affects folder-level arts, so only use them when we currently
                            // don't have any art.
                            if res.imp().thumbnail.get_state() == ImageState::Empty
                                && res.imp().song.upgrade().is_some_and(|s| s.get_folder_uri() == uri) {
                                    res.imp().thumbnail.show(&thumb);
                                }

                        }
                    ),
                ),
                cache_state.connect_closure(
                    "folder-cover-cleared",
                    false,
                    closure_local!(
                        #[weak]
                        res,
                        move |_: CacheState, uri: &str| {
                            if res.imp().song.upgrade().is_some_and(|s| s.get_folder_uri() == uri) {
                                res.imp().thumbnail.clear();
                            }
                        }
                    ),
                ),
            )));
        }

        if let Some(player) = player {
            res.imp().player.set(Some(player));

            let _ = res
                .imp()
                .playing_signal_id
                .replace(Some(player.connect_notify_local(
                    Some("queue-id"),
                    clone!(
                        #[weak]
                        res,
                        move |player, _| {
                            res.update_playing_indicator(player);
                        }
                    ),
                )));
            res.update_playing_indicator(player);
        }

        res
    }

    fn update_playing_indicator(&self, player: &Player) {
        match (
            player.queue_id(),
            self.imp().song.upgrade().map(|s| s.get_queue_id()),
        ) {
            (Some(id), Some(own_id)) => {
                self.set_is_playing(id == own_id);
            }
            _ => {
                self.set_is_playing(false);
            }
        }
    }

    async fn schedule_thumbnail(&self) {
        if let (Some(cache), Some(song)) = (
            self.imp().cache.get(),
            self.imp().song.upgrade()
        ) {
            self.imp().thumbnail.show_spinner();
            match cache.clone().get_song_cover(song.get_info(), true, true).await {
                Ok(Some(tex)) => self.imp().thumbnail.show(&tex),
                Ok(None) => self.imp().thumbnail.clear(),
                Err(e) => {
                    dbg!(e);
                    self.imp().thumbnail.clear();
                }
            }
        }
    }

    pub fn on_bind(&self, song: &Song) {
        self.imp().song.set(Some(song));
        glib::spawn_future_local(clone!(
            #[weak(rename_to = this)] self,
            async move {
                this.schedule_thumbnail().await;
            }
        ));
    }

    pub fn on_unbind(&self) {
        self.imp().thumbnail.clear();
        self.imp().song.set(None);
    }

    pub fn set_playing_indicator_visible(&self, vis: bool) {
        self.imp().playing_indicator.set_visible(vis);
    }

    pub fn set_is_playing(&self, playing: bool) {
        self.imp().playing_indicator.set_reveal_child(playing);
    }

    pub fn set_index_visible(&self, vis: bool) {
        self.imp().index.set_visible(vis);
    }

    pub fn set_index(&self, val: &str) {
        self.imp().index.set_label(val);
    }

    pub fn set_name(&self, name: &str) {
        self.imp().name.label().set_label(name);
    }

    pub fn set_thumbnail_visible(&self, vis: bool) {
        self.imp().thumbnail.set_visible(vis);
    }

    pub fn set_quality_grade(&self, grade: QualityGrade) {
        let icon_name = grade.to_icon_name();
        self.imp().quality_grade.set_visible(icon_name.is_some());
        self.imp().quality_grade.set_icon_name(icon_name);
    }

    pub fn set_first_attrib_icon_name(&self, val: Option<&str>) {
        self.imp().first_attrib_icon.set_visible(val.is_some());
        self.imp().first_attrib_icon.set_icon_name(val);
    }

    pub fn set_second_attrib_icon_name(&self, val: Option<&str>) {
        self.imp().second_attrib_icon.set_visible(val.is_some());
        self.imp().second_attrib_icon.set_icon_name(val);
    }

    pub fn set_third_attrib_icon_name(&self, val: Option<&str>) {
        self.imp().third_attrib_icon.set_visible(val.is_some());
        self.imp().third_attrib_icon.set_icon_name(val);
    }

    pub fn set_first_attrib_text(&self, val: Option<&str>) {
        self.imp().first_attrib_text.set_visible(val.is_some());
        self.imp().first_attrib_text.set_label(val.unwrap_or(""));
    }

    pub fn set_second_attrib_text(&self, val: Option<&str>) {
        self.imp().second_attrib_text.set_visible(val.is_some());
        self.imp().second_attrib_text.set_label(val.unwrap_or(""));
    }

    pub fn set_third_attrib_text(&self, val: Option<&str>) {
        self.imp().third_attrib_text.set_visible(val.is_some());
        self.imp().third_attrib_text.set_label(val.unwrap_or(""));
    }

    pub fn set_end_widget(&self, widget: Option<&gtk::Widget>) {
        self.imp().center_box.set_end_widget(widget);
    }

    pub fn end_widget(&self) -> Option<gtk::Widget> {
        self.imp().center_box.end_widget()
    }
}
