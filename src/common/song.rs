use core::time::Duration;
use std::{
    path::{Path},
    cell::{Cell, RefCell},
    ffi::OsStr
};
use gtk::glib;
use gtk::gdk::Texture;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use mpd::status::AudioFormat;

// Mostly for eyecandy
#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphoniaQualityGrade")]
pub enum QualityGrade {
    #[default]
    Unknown, // Catch-all
    Lossy,  // Anything not meeting the below
    CD,  // Lossless codec (FLAC, WavPack & Monkey's Audio for now) 44100-48000Hz 16bit.
    // While 48000Hz isn't technically Red Book, the "quality" should
    // be the same (unless resampled from 44100Hz CD).
    HiRes, // Lossless codec above 48000Hz and at least 24 bit depth.
    DSD // 150MB song files go brrr
}

impl QualityGrade {
    pub fn to_icon_name(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Lossy => None,
            Self::CD => Some("format-cd-symbolic"),
            Self::HiRes => Some("format-hires-symbolic"),
            Self::DSD => Some("format-dsd-symbolic")
        }
    }
}


// fn parse_date(datestr: &str) -> Option<NaiveDate> {
//     let mut comps = datestr.split("-");
//     if let Some(year_str) = comps.next() {
//         if let Ok(year) = year_str.parse::<i32>() {
//             if let Some(month_str) = comps.next() {
//                 if let Ok(month) = month_str.parse::<u32>() {
//                     if let Some(day_str) = comps.next() {
//                         if let Ok(day) = day_str.parse::<u32>() {
//                             return NaiveDate::from_ymd_opt(year, month, day);
//                         }
//                         return NaiveDate::from_ymd_opt(year, month, 1);
//                     }
//                     return NaiveDate::from_ymd_opt(year, month, 1);
//                 }
//                 return NaiveDate::from_ymd_opt(year, 1, 1);
//             }
//             return NaiveDate::from_ymd_opt(year, 1, 1);
//         }
//         return None;
//     }
//     None
// }

// We define our own Song struct for more convenient handling, especially with
// regards to optional fields and tags such as albums.

#[derive(Debug, Clone)]
pub struct SongInfo {
    // TODO: Might want to refactor to Into<Cow<'a, str>>
    uri: String,
    title: Option<String>,
    // last_mod: RefCell<Option<u64>>,
    artist: Option<String>,
    album_artist: Option<String>,
    duration: Option<Duration>, // Default to 0 if somehow the option in mpd's Song is None
    queue_id: Option<u32>,
    // range: Option<Range>,
    album: Option<String>,
    // TODO: add albumsort
    // pub release_date: RefCell<Option<u64>>,
    // TODO: Add more fields for managing classical music, such as composer, ensemble and movement number
    is_playing: Cell<bool>,
    thumbnail: Option<Texture>,
    quality_grade: QualityGrade
}

impl SongInfo {
    pub fn new() -> Self {
        Self::default()
    }
}

impl Default for SongInfo {
    fn default() -> Self {
        Self {
            uri: String::from(""),
            title: None,
            artist: None,
            album_artist: None,
            duration: None,
            queue_id: None,
            album: None,
            is_playing: Cell::new(false),
            thumbnail: None,
            quality_grade: QualityGrade::Unknown
        }
    }
}


mod imp {
    use glib::{
        ParamSpec,
        ParamSpecUInt,
        ParamSpecUInt64,
        ParamSpecBoolean,
        ParamSpecString,
        ParamSpecObject
    };
    use once_cell::sync::Lazy;
    use super::*;

    #[derive(Default, Debug)]
    pub struct Song {
        pub info: RefCell<SongInfo>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Song {
        const NAME: &'static str = "EuphoniaSong";
        type Type = super::Song;

        fn new() -> Self {
            Self {
                info: RefCell::new(SongInfo::default())
            }
        }
    }

    impl ObjectImpl for Song {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").read_only().build(),
                    ParamSpecString::builder("name").build(),
                    // ParamSpecString::builder("last_mod").build(),
                    ParamSpecString::builder("artist").build(),
                    ParamSpecString::builder("album-artist").build(),
                    ParamSpecUInt64::builder("duration").read_only().build(),
                    ParamSpecUInt::builder("queue-id").build(),
                    ParamSpecBoolean::builder("is-queued").read_only().build(),
                    ParamSpecString::builder("album").build(),
                    ParamSpecString::builder("quality-grade").read_only().build(),
                    // ParamSpecString::builder("release_date").build(),
                    ParamSpecBoolean::builder("is-playing").build(),
                    ParamSpecObject::builder::<Texture>("thumbnail")
                        .read_only()
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "uri" => obj.get_uri().to_value(),
                "name" => obj.get_name().to_value(),
                // "last_mod" => obj.get_last_mod().to_value(),
                "artist" => obj.get_artist().to_value(),
                "album-artist" => obj.get_album_artist().to_value(),
                "duration" => obj.get_duration().to_value(),
                "queue-id" => obj.get_queue_id().to_value(),
                "is-queued" => obj.is_queued().to_value(),
                "album" => obj.get_album().to_value(),
                // "release_date" => obj.get_release_date.to_value(),
                "is-playing" => obj.is_playing().to_value(),
                "quality-grade" => obj.get_quality_grade().to_value(),
                "thumbnail" => obj.get_thumbnail().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "name" => {
                    // Always set to title tag
                    if let Ok(name) = value.get::<&str>() {
                        self.info.borrow_mut().title.replace(name.to_owned());
                    }
                    obj.notify("name");
                }
                "artist" => {
                    if let Ok(a) = value.get::<&str>() {
                        self.info.borrow_mut().artist.replace(a.to_owned());
                    }
                    obj.notify("artist");
                }
                "album-artist" => {
                    if let Ok(a) = value.get::<&str>() {
                        self.info.borrow_mut().album_artist.replace(a.to_owned());
                    }
                    obj.notify("album-artist");
                }
                "album" => {
                    if let Ok(album) = value.get::<&str>() {
                        self.info.borrow_mut().album.replace(album.to_owned());
                    }
                    obj.notify("album");
                }
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct Song(ObjectSubclass<imp::Song>);
}

impl Song {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get_uri(&self) -> String {
        self.imp().info.borrow().uri.clone()
    }

    pub fn get_name(&self) -> Option<String> {
        // Get title tag or filename without extension in case there's no title tag.
        // Returns a clone since
        // 1. Song names are (usually) short
        // 2. There might be no name tag, in which case we'll have to extract from the path.
        // Prefer song name in tag over filename
        if let Some(title) = self.imp().info.borrow().title.as_ref() {
            return Some(title.clone());
        }
        // Else extract from URI
        else if let Some(stem) = Path::new(&self.get_uri()).file_stem() {
            return Some(String::from(stem.to_str().unwrap()));
        }
        None
    }

    pub fn get_duration(&self) -> u64 {
        if let Some(dur) = self.imp().info.borrow().duration.as_ref() {
            return dur.as_secs();
        }
        0
    }

    pub fn get_artist(&self) -> Option<String> {
        self.imp().info.borrow().artist.clone()
    }

    pub fn get_album_artist(&self) -> Option<String> {
        self.imp().info.borrow().album_artist.clone()
    }

    pub fn get_queue_id(&self) -> u32 {
        if let Some(id) = self.imp().info.borrow().queue_id {
            return id;
        }
        0
    }

    pub fn is_queued(&self) -> bool {
        self.imp().info.borrow().queue_id.is_some()
    }

    pub fn get_album(&self) -> Option<String> {
        self.imp().info.borrow().album.clone()
    }

    pub fn get_thumbnail(&self) -> Option<Texture> {
        self.imp().info.borrow().thumbnail.clone()
    }

    pub fn set_thumbnail(&self, tex: Option<Texture>) {
        {
            let mut info = self.imp().info.borrow_mut();
            info.thumbnail = tex;
        }
        self.notify("thumbnail");
    }

    pub fn is_playing(&self) -> bool {
        self.imp().info.borrow().is_playing.get()
    }

    pub fn set_is_playing(&self, val: bool) {
        let old_val = self.imp().info.borrow().is_playing.replace(val);
        if old_val != val {
            self.notify("is-playing");
        }
    }

    pub fn get_quality_grade(&self) -> QualityGrade {
        self.imp().info.borrow().quality_grade
    }
}

impl Default for Song {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<mpd::song::Song> for SongInfo {
    fn from(song: mpd::song::Song) -> Self {
        let mut res = Self {
            uri: song.file,
            title: song.title,
            artist: song.artist,
            album_artist: None,
            duration: song.duration,
            queue_id: None,
            album: None,
            is_playing: Cell::new(false),
            thumbnail: None,
            quality_grade: QualityGrade::Unknown
        };
        if let Some(place) = song.place {
            let _ = res.queue_id.replace(place.id.0);
        }

        // Search tags vector for additional fields we can use.
        // Again we're using iter() here to avoid cloning everything.
        // Limitation: MPD cannot parse DSD song format UNTIL PLAYED.
        // As such, DSD format description must be handled by the player controller,
        // working off the "format" attribute of the Status object.
        // The bits == 1 check only works with htkhiem's fork of rust-mpd with DSD correction
        let maybe_extension = Path::new(&res.uri).extension().and_then(OsStr::to_str);
        if let Some(extension) = maybe_extension {
            if ["dsf", "dff", "wsd"].contains(&extension) {
                // Is probably DSD
                res.quality_grade = QualityGrade::DSD;
            }
        }
        for (tag, val) in song.tags.into_iter() {
            match tag.as_str() {
                "Album" => {let _ = res.album.replace(val);},
                "AlbumArtist" => {let _ = res.album_artist.replace(val);},
                // "date" => res.imp().release_date.replace(Some(val.clone())),
                "Format" => {
                    if let Some(extension) = maybe_extension {
                        if let Ok(format) = val.parse::<AudioFormat>() {
                            if ["flac", "alac", "wv", "ape"].contains(&extension) {
                                // Is probably lossless PCM
                                if format.rate > 48000 && format.bits >= 24 {
                                    res.quality_grade = QualityGrade::HiRes;
                                }
                                else {
                                    res.quality_grade = QualityGrade::CD;
                                }
                            }
                            else {
                                res.quality_grade = QualityGrade::Lossy;
                            }
                        }
                    }
                },
                _ => {}
            }
        }
        res
    }
}

impl From<mpd::song::Song> for Song {
    fn from(song: mpd::song::Song) -> Self {
        let info = SongInfo::from(song);
        let res = glib::Object::new::<Self>();
        res.imp().info.replace(info);
        res
    }
}
