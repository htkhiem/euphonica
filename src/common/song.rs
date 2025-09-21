use core::time::Duration;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use mpd::status::AudioFormat;
use mpris_server::{zbus::zvariant::ObjectPath, Time};
use std::{
    cell::{Cell, OnceCell},
    ffi::OsStr,
    path::Path
};
use time::{Date, Month, OffsetDateTime};

use crate::cache::{get_image_cache_path, sqlite};

use super::{artists_to_string, parse_mb_artist_tag, AlbumInfo, ArtistInfo};

// Mostly for eyecandy
#[derive(Clone, Copy, Debug, glib::Enum, PartialEq, Default)]
#[enum_type(name = "EuphonicaQualityGrade")]
pub enum QualityGrade {
    #[default]
    Unknown, // Catch-all
    Lossy, // Anything not meeting the below
    CD,    // Lossless codec (FLAC, WavPack & Monkey's Audio for now) 44100-48000Hz 16bit.
    // While 48000Hz isn't technically Red Book, the "quality" should
    // be the same (unless resampled from 44100Hz CD).
    HiRes, // Lossless codec above 48000Hz and at least 24 bit depth.
    DSD,   // 150MB song files go brrr
}

impl QualityGrade {
    pub fn to_icon_name(self) -> Option<&'static str> {
        match self {
            Self::Unknown => None,
            Self::Lossy => None,
            Self::CD => Some("format-cd-symbolic"),
            Self::HiRes => Some("format-hires-symbolic"),
            Self::DSD => Some("format-dsd-symbolic"),
        }
    }
}

fn parse_date(datestr: &str) -> Option<Date> {
    // MPD uses yyyy-MM-dd but the month and day may be optional.
    let mut comps = datestr.split('-');
    let mut year_val: Option<i32> = None;
    let mut month_val: Month = Month::January;
    let mut day_val: u8 = 1;

    let year_str = comps.next()?;
    if let Ok(year) = year_str.parse::<i32>() {
        let _ = year_val.replace(year);
    } else {
        return None;
    }

    if let Some(month_str) = comps.next() {
        if let Ok(month) = month_str.parse::<u8>() {
            if let Ok(month_enum) = Month::try_from(month) {
                month_val = month_enum;
            }
        }
    }
    if let Some(day_str) = comps.next() {
        if let Ok(day) = day_str.parse::<u8>() {
            day_val = day;
        }
    }
    if let Ok(date) = Date::from_calendar_date(year_val.unwrap(), month_val, day_val) {
        return Some(date);
    }
    None
}

/// We define our own Song struct for more convenient handling, especially with
/// regards to optional fields and tags such as albums.
#[derive(Debug, Clone)]
pub struct SongInfo {
    // TODO: Might want to refactor to Into<Cow<'a, str>>
    pub uri: String,
    pub title: String, // Might just be filename
    // last_mod: RefCell<Option<u64>>,
    pub artists: Vec<ArtistInfo>,
    pub artist_tag: Option<String>, // Original tag, with all the linkages and formatting
    pub duration: Option<Duration>, // Default to 0 if somehow the option in mpd's Song is None
    queue_id: Option<u32>,
    queue_pos: Option<u32>,  // Only set once at creation. Subsequent updates are kept in the Song GObject.
    // range: Option<Range>,
    pub album: Option<AlbumInfo>,
    track: Cell<i64>,
    disc: Cell<i64>,
    // TODO: add albumsort
    // Store Date instead of string to save a tiny bit of memory.
    // Also gives us formatting flexibility in the future.
    release_date: Option<Date>,
    // TODO: Add more fields for managing classical music, such as composer, ensemble and movement number
    quality_grade: QualityGrade,
    // MusicBrainz stuff
    mbid: Option<String>,
    last_modified: Option<String>,
    pub last_played: Option<OffsetDateTime>
}

impl SongInfo {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_album_info(self) -> Option<AlbumInfo> {
        self.album
    }

    pub fn into_artist_infos(self) -> Vec<ArtistInfo> {
        self.artists
    }
}

impl Default for SongInfo {
    fn default() -> Self {
        Self {
            uri: String::from(""),
            title: String::from("Untitled Song"),
            artists: Vec::new(),
            artist_tag: None,
            duration: None,
            queue_id: None,
            queue_pos: None,
            album: None,
            track: Cell::new(-1), // negative values indicate no track index
            disc: Cell::new(-1),
            release_date: None,
            quality_grade: QualityGrade::Unknown,
            mbid: None,
            last_modified: None,
            last_played: None
        }
    }
}

mod imp {
    use super::*;
    use glib::{
        ParamSpec, ParamSpecBoolean, ParamSpecInt64, ParamSpecObject, ParamSpecString, ParamSpecUInt, ParamSpecUInt64
    };
    use once_cell::sync::Lazy;

    /// The GObject Song wrapper.
    /// By nesting info inside another struct, we enforce tag editing to be
    /// atomic. Tag editing is performed by first cloning the whole SongInfo
    /// struct to a mutable variable, modify it, then create a new Song wrapper
    /// from the modified SongInfo struct (no copy required this time).
    /// This design also avoids a RefCell.
    #[derive(Debug)]
    pub struct Song {
        pub info: OnceCell<SongInfo>,
        pub is_playing: Cell<bool>
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Song {
        const NAME: &'static str = "EuphonicaSong";
        type Type = super::Song;

        fn new() -> Self {
            Self {
                info: OnceCell::new(),
                is_playing: Cell::new(false)
            }
        }
    }

    impl Default for Song {
        fn default() -> Self {
            Self::new()
        }
    }

    impl ObjectImpl for Song {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").read_only().build(),
                    ParamSpecString::builder("name").read_only().build(),
                    // ParamSpecString::builder("last_mod").build(),
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecUInt64::builder("duration").read_only().build(),
                    ParamSpecUInt::builder("queue-id").build(),
                    ParamSpecUInt::builder("queue-pos").build(),
                    ParamSpecBoolean::builder("is-queued").read_only().build(),
                    ParamSpecBoolean::builder("is-playing").read_only().build(),
                    ParamSpecString::builder("album").read_only().build(),
                    ParamSpecInt64::builder("track").read_only().build(),
                    ParamSpecInt64::builder("disc").read_only().build(),
                    ParamSpecObject::builder::<glib::BoxedAnyObject>("release-date")
                        .read_only()
                        .build(), // boxes Option<time::Date>
                    ParamSpecString::builder("quality-grade")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("last-modified")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("last-played-desc")
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
                // Represented in MusicBrainz format, i.e. Composer; Performer, Performer,...
                // The composer part is optional.
                "artist" => obj.get_artist_tag().to_value(),
                "duration" => obj.get_duration().to_value(),
                "queue-id" => obj.get_queue_id().to_value(),
                "queue-pos" => obj.get_queue_pos().to_value(),
                "is-queued" => obj.is_queued().to_value(),
                "is-playing" => obj.is_playing().to_value(),
                "album" => obj.get_album_title().to_value(),
                "track" => obj.get_track().to_value(),
                "disc" => obj.get_disc().to_value(),
                "release-date" => glib::BoxedAnyObject::new(obj.get_release_date()).to_value(),
                // "release_date" => obj.get_release_date.to_value(),
                "quality-grade" => obj.get_quality_grade().to_icon_name().to_value(),
                "last-modified" => obj.get_last_modified().to_value(),
                "last-played-desc" => obj.get_last_played_desc().to_value(),
                _ => unimplemented!(),
            }
        }

        // fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
        //     let obj = self.obj();
        //     match pspec.name() {
        //         _ => unimplemented!(),
        //     }
        // }
    }
}

glib::wrapper! {
    pub struct Song(ObjectSubclass<imp::Song>);
}

impl Song {
    pub fn new() -> Self {
        Self::default()
    }

    // ALL of the getters below require that the info field be initialised!
    pub fn get_info(&self) -> &SongInfo {
        &self.imp().info.get().unwrap()
    }

    pub fn get_uri(&self) -> &str {
        &self.get_info().uri
    }

    pub fn get_name(&self) -> &str {
        &self.get_info().title
    }

    pub fn get_last_modified(&self) -> Option<&str> {
        self.get_info().last_modified.as_deref()
    }

    pub fn get_last_played_desc(&self) -> Option<String> {
        // TODO: translations
        if let Some(then) = self.get_last_played() {
            let now = OffsetDateTime::now_utc();
            let diff_days = (
                now.unix_timestamp() - then.unix_timestamp()
            ) as f64 / 86400.0;
            if diff_days <= 0.0 {
                None
            }
            else {
                if diff_days >= 365.0 {
                    let years = (diff_days / 365.0).floor() as u32;
                    if years == 1 {
                        Some("last year".to_owned())
                    }
                    else {
                        Some(format!("{years} years ago"))
                    }
                }
                else if diff_days >= 30.0 {
                    // Just let a month be 30 days long on average :)
                    let months = (diff_days / 30.0).floor() as u32;
                    if months == 1 {
                        Some("last month".to_owned())
                    }
                    else {
                        Some(format!("{months} years ago"))
                    }
                }
                else if diff_days >= 2.0 {
                    Some(format!("{diff_days:.0} days ago"))
                }
                else if diff_days >= 1.0 {
                    Some("yesterday".to_owned())
                }
                else {
                    Some("today".to_owned())
                }
            }
        }
        else {
            None
        }
    }

    pub fn get_last_played(&self) -> Option<OffsetDateTime> {
        self.get_info().last_played.clone()
    }

    pub fn get_duration(&self) -> u64 {
        if let Some(dur) = self.get_info().duration.as_ref() {
            return dur.as_secs();
        }
        0
    }

    pub fn get_artists(&self) -> &[ArtistInfo] {
        &self.get_info().artists
    }

    /// Get artist names separated by commas. If the first artist listed is a composer,
    /// the next separator will be a semicolon instead. The quality of this output depends
    /// on whether all delimiters are specified by the user.
    pub fn get_artist_str(&self) -> Option<String> {
        artists_to_string(&self.get_info().artists)
    }

    /// Get the original artist tag before any parsing.
    pub fn get_artist_tag(&self) -> Option<&str> {
        self.get_info().artist_tag.as_deref()
    }

    pub fn get_queue_id(&self) -> u32 {
        self.get_info().queue_id.unwrap_or(0)
    }

    pub fn get_queue_pos(&self) -> u32 {
        self.get_info().queue_pos.unwrap_or(0)
    }

    pub fn is_queued(&self) -> bool {
        self.get_info().queue_id.is_some()
    }

    pub fn get_album(&self) -> Option<&AlbumInfo> {
        self.get_info().album.as_ref()
    }

    pub fn get_album_title(&self) -> Option<&str> {
        if let Some(album) = &self.get_info().album {
            Some(album.title.as_ref())
        } else {
            None
        }
    }

    pub fn get_track(&self) -> i64 {
        self.get_info().track.get()
    }

    pub fn get_disc(&self) -> i64 {
        self.get_info().disc.get()
    }

    pub fn is_playing(&self) -> bool {
        self.imp().is_playing.get()
    }

    pub fn set_is_playing(&self, new: bool) {
        let old = self.imp().is_playing.replace(new);
        if old != new {
            self.notify("is-playing");
        }
    }

    pub fn get_quality_grade(&self) -> QualityGrade {
        self.get_info().quality_grade
    }

    pub fn get_release_date(&self) -> Option<Date> {
        self.get_info().release_date
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.get_info().mbid.as_deref()
    }

    pub fn get_mpris_metadata(&self) -> mpris_server::Metadata {
        let mut meta = mpris_server::Metadata::builder()
            .title(self.get_name())
            .trackid(ObjectPath::from_string_unchecked(format!(
                "/io/github/htkhiem/Euphonica/{}",
                self.get_queue_id()
            )))
            .length(Time::from_secs(self.get_duration() as i64))
            .build();
        if let Some(album) = self.get_album() {
            meta.set_album(Some(&album.title));
            if album.artists.len() > 0 {
                meta.set_album_artist(Some(
                    album
                        .artists
                        .iter()
                        .map(|a| a.name.as_ref())
                        .collect::<Vec<&str>>(),
                ));
            }
        }
        let artists = self.get_artists();
        if artists.len() > 0 {
            meta.set_artist(Some(
                artists
                    .iter()
                    .map(|a| a.name.as_ref())
                    .collect::<Vec<&str>>(),
            ))
        }

        // Album art, if available
        if let Some(thumbnail_name) = sqlite::find_cover_by_uri(self.get_uri(), true).expect("Sqlite DB error") {
            let mut thumbnail_path = get_image_cache_path();
            thumbnail_path.push(thumbnail_name);
            if thumbnail_path.exists() {
                let path_string =
                    "file://".to_owned() + &thumbnail_path.into_os_string().into_string().unwrap();
                meta.set_art_url(Some(path_string));
            }
        }

        // TODO: disc & track num
        meta
    }
}

impl Default for Song {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<mpd::song::Song> for SongInfo {
    fn from(song: mpd::song::Song) -> Self {
        let artists: Vec<ArtistInfo>;
        if let Some(artist_str) = &song.artist {
            // TODO: Find a way to detect classical works
            artists = parse_mb_artist_tag(artist_str)
                .iter()
                .map(|s| ArtistInfo::new(s, None, false))
                .collect();
        } else {
            artists = Vec::with_capacity(0);
        }
        let name: String;
        if let Some(title) = song.title {
            name = title;
        }
        // Else extract from URI
        else if let Some(stem) = Path::new(&song.file).file_stem() {
            name = String::from(stem.to_str().unwrap());
        } else {
            name = String::from("");
        }
        let mut res = Self {
            uri: song.file,
            title: name,
            artists,
            artist_tag: song.artist,
            duration: song.duration,
            queue_id: None,
            queue_pos: None,
            album: None,
            track: Cell::new(-1),
            disc: Cell::new(-1),
            release_date: None,
            quality_grade: QualityGrade::Unknown,
            mbid: None,
            last_modified: song.last_mod,
            last_played: None
        };

        if let Some(place) = song.place {
            let _ = res.queue_id.replace(place.id.0);
            let _ = res.queue_pos.replace(place.pos);
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
        let mut albumsort: Option<String> = None;
        let mut artist_mbids: Vec<String> = Vec::new();
        let mut artistsorts: Vec<String> = Vec::new();
        let mut albumartist: Option<String> = None;
        let mut albumartistsort: Option<String> = None;
        let mut album_artist_mbids: Vec<String> = Vec::new();
        let mut album_mbid: Option<String> = None;
        for (tag, val) in song.tags.into_iter() {
            match tag.to_lowercase().as_str() {
                "album" => {
                    if res.album.is_none() {
                        let _ = res.album.replace(AlbumInfo::new(
                            &res.uri,
                            &val,
                            None,
                            None,
                            None,
                            Vec::with_capacity(0),
                            res.quality_grade.clone(),
                        ));
                    } else {
                        println!("[WARNING] Multiple Album tags found. Only keeping the first one.");
                    }
                }
                "albumsort" => {
                    albumsort.replace(val);
                }
                "albumartist" => {
                    albumartist.replace(val);
                }
                "artistsort" => {
                    artistsorts.push(val);
                }
                "albumartistsort" => {
                    albumartistsort.replace(val);
                }
                // "date" => res.imp().release_date.replace(Some(val.clone())),
                "format" => {
                    if let Some(extension) = maybe_extension {
                        if let Ok(format) = val.parse::<AudioFormat>() {
                            if ["flac", "alac", "wv", "ape"].contains(&extension) {
                                // Is probably lossless PCM
                                if format.rate > 48000 && format.bits >= 24 {
                                    res.quality_grade = QualityGrade::HiRes;
                                } else {
                                    res.quality_grade = QualityGrade::CD;
                                }
                            } else {
                                res.quality_grade = QualityGrade::Lossy;
                            }
                        }
                    }
                }
                "originaldate" => {
                    res.release_date = parse_date(val.as_ref());
                }
                "track" => {
                    if let Ok(idx) = val.parse::<i64>() {
                        let _ = res.track.replace(idx);
                    }
                }
                "disc" => {
                    if let Ok(idx) = val.parse::<i64>() {
                        let _ = res.disc.replace(idx);
                    }
                }
                // Beets might use uppercase versions of these keys but we're
                // converting all to lowercase
                "musicbrainz_trackid" => {
                    let _ = res.mbid.replace(val);
                }
                "musicbrainz_albumid" => {
                    // Can encounter this before initialising the album object
                    if album_mbid.is_none() {
                        let _ = album_mbid.replace(val);
                    } else {
                        println!("[WARNING] Multiple musicbrainz_albumid tags found. Only keeping the first one.");
                    }
                }
                "musicbrainz_artistid" => {
                    // Can encounter this multiple times and/or before
                    // initialising the artist objects
                    artist_mbids.push(val);
                }
                "musicbrainz_albumartistid" => {
                    // Can encounter this multiple times and/or before
                    // initialising the albumartist objects
                    album_artist_mbids.push(val);
                }
                _ => {}
            }
        }

        // Assume the artist IDs and artistsort tags are given in the same order as the artist tags
        for (idx, id) in artist_mbids.drain(..).enumerate() {
            if idx < res.artists.len() {
                let _ = res.artists[idx].mbid.replace(id);
            }
        }

        for (idx, tag) in artistsorts.drain(..).enumerate() {
            if idx < res.artists.len() {
                let _ = res.artists[idx].sort_tag.replace(tag);
            }
        }

        if let Some(album) = res.album.as_mut() {
            album.mbid = album_mbid;
            album.albumsort = albumsort;
            album.albumartistsort = albumartistsort;
            album.release_date = res.release_date.clone();
            // Assume the albumartist IDs are given in the same order as the albumartist tags
            if let Some(album_artist_str) = albumartist.as_ref() {
                album.add_artists_from_string(album_artist_str);
            }
            album.albumartist = albumartist;
            for (idx, id) in album_artist_mbids.drain(..).enumerate() {
                if idx < album.artists.len() {
                    let _ = album.artists[idx].mbid.replace(id);
                }
            }
        }

        res
    }
}

impl From<mpd::song::Song> for Song {
    fn from(song: mpd::song::Song) -> Self {
        let info = SongInfo::from(song);
        Self::from(info)
    }
}

impl From<SongInfo> for Song {
    fn from(info: SongInfo) -> Self {
        let res = glib::Object::new::<Self>();
        let _ = res.imp().info.set(info);
        res
    }
}
