use crate::cache::sqlite;
use crate::common::{DSD_EXTENSIONS, LOSSLESS_PCM_EXTENSIONS, parse_date};
use crate::utils::{get_image_cache_path, get_time_ago_desc, strip_filename_linux};
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use itertools::Itertools;
use mpd::song::Song as SongInfo;
use mpris_server::{Time, zbus::zvariant::ObjectPath};
use once_arc::OnceArc;
use std::cell::Ref;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::{
    cell::{Cell, OnceCell},
    ffi::OsStr,
    path::Path,
};
use time::{Date, OffsetDateTime};

use super::Stickers;
use super::{AlbumInfo, ArtistInfo};

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
    pub fn of_mpd_song(song: &mpd::song::Song) -> Self {
        let path = Path::new(&song.file);
        // Parse quality grade
        let mut quality_grade = QualityGrade::Unknown;
        let mut maybe_lossless_pcm = false;
        if let Some(extension) = path.extension().and_then(OsStr::to_str) {
            if DSD_EXTENSIONS.contains(&extension) {
                // Is probably DSD
                quality_grade = QualityGrade::DSD;
            } else if LOSSLESS_PCM_EXTENSIONS.contains(&extension) {
                maybe_lossless_pcm = true;
            }
        }
        if let Some(format) = &song.format {
            if maybe_lossless_pcm {
                if format.rate > 48000 && format.bits >= 24 {
                    quality_grade = QualityGrade::HiRes;
                } else {
                    quality_grade = QualityGrade::CD;
                }
            } else {
                quality_grade = QualityGrade::Lossy;
            }
        }
        quality_grade
    }

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

/// Starting from v1.0 most of the song data pre-processing has been moved up to the rust-mpd fork.
/// As such, the Song GObject now wraps mpd::song::Song directly.
// #[derive(Debug, Clone, Derivative)]
// #[derivative(Default)]
// pub struct SongInfo {
//     // TODO: Might want to refactor to Into<Cow<'a, str>>
//     #[derivative(Default(value = "String::from(\"\")"))]
//     pub uri: String,
//     #[derivative(Default(value = "String::from(\"Untitled Song\")"))]
//     pub title: String, // Might just be filename
//     // last_mod: RefCell<Option<u64>>,
//     pub artists: Vec<ArtistInfo>,
//     pub duration: Option<Duration>, // Default to 0 if somehow the option in mpd's Song is None
//     pub queue_id: Option<u32>,
//     pub queue_pos: Option<u32>, // Only set once at creation. Subsequent updates are kept in the Song GObject.
//     // range: Option<Range>,
//     pub album: Option<AlbumInfo>,
//     pub track: Option<i64>,
//     pub disc: Option<i64>,
//     // TODO: add albumsort
//     // Store Date instead of string to save a tiny bit of memory.
//     // Also gives us formatting flexibility in the future.
//     pub release_date: Option<Date>,
//     // TODO: Add more fields for managing classical music, such as composer, ensemble and movement number
//     quality_grade: QualityGrade,
//     // MusicBrainz stuff
//     pub mbid: Option<String>,
//     pub last_modified: Option<String>,
//     pub last_played: Option<OffsetDateTime>,
// }

// impl SongInfo {
//     pub fn new() -> Self {
//         Self::default()
//     }

//     pub fn into_album_info(self) -> Option<AlbumInfo> {
//         self.album
//     }

//     pub fn into_artist_infos(self) -> Vec<ArtistInfo> {
//         self.artists
//     }

//     /// Get something to serve as this song's internal ID for
//     /// filtering/comparison purposes.
//     /// Order of preference:
//     /// 1. MBID
//     /// 2. URI
//     pub fn get_comp_id(&self) -> &str {
//         self.mbid.as_deref().unwrap_or(self.uri.as_ref())
//     }

//     /// Efficient replacement for the "original artist tag" field which doesn't work correctly for songs
//     /// with multiple Artist tags.
//     pub fn artist_names(&self) -> impl Iterator<Item = &str> {
//         self.artists.iter().map(|ai| ai.name.as_str())
//     }
// }

mod imp {
    use std::cell::RefCell;

    use super::*;
    use glib::{
        ParamSpec, ParamSpecBoolean, ParamSpecChar, ParamSpecInt64, ParamSpecObject,
        ParamSpecString, ParamSpecUInt, ParamSpecUInt64,
    };
    use once_arc::InitOnceArc;
    use once_cell::sync::Lazy;

    /// The GObject Song wrapper.
    /// By nesting info inside another struct, we enforce tag editing to be
    /// atomic. Tag editing is performed by first cloning the whole SongInfo
    /// struct to a mutable variable, modify it, then create a new Song wrapper
    /// from the modified SongInfo struct (no copy required this time).
    /// This design also avoids a RefCell.
    pub struct Song {
        /// OnceArc is roughly a more efficient OnceLock<Arc<>> that solves many problems by only
        /// allowing it to be set once.
        /// OnceArc is safe here as initial assignment happens exactly on new(), i.e. not lazy.
        /// The sole reason why this has to be "Once" is that this inner struct is created before
        /// new()'s code runs, so it always starts empty. Same issue with a tonne of other fields
        /// (Cells, RefCells, etc).
        /// The "Arc" side of OnceArc is needed since many of its users delegate work to threadpools.
        pub info: OnceArc<SongInfo>,
        pub quality_grade: Cell<QualityGrade>,
        /// Cached name resolution. Not Arc since the lazy init is synchronous.
        pub name: OnceCell<String>,
        /// Cached concatenation of artist names.
        pub artist_string: OnceCell<String>,
        /// Cached parsed ArtistInfo structs. Use InitOnceArc instead of OnceArc to ensure only
        /// one init job can run at any time. Said init job will be BLOCKING so clone the whole
        /// InitOnceArc to a child thread first before running!
        /// The init job itself keeps a ref to the above rust-mpd Song.
        pub artists: InitOnceArc<Vec<ArtistInfo>>,
        /// Cached parsed AlbumInfo. Nested inside an Option as we need to record three states:
        /// Haven't been parsed yet
        /// Parsed but no album info found
        /// Parsed into album info.
        /// The album itself is also lazy. The initial parsing to get AlbumInfo is therefore light-
        /// weight enough to be synchronous on the main thread.
        pub album: InitOnceArc<Option<AlbumInfo>>,
        pub stickers: RefCell<Stickers>,
        pub queue_pos: Cell<u32>,
        pub is_playing: Cell<bool>,
        pub last_played: OnceCell<OffsetDateTime>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Song {
        const NAME: &'static str = "EuphonicaSong";
        type Type = super::Song;

        fn new() -> Self {
            Self {
                info: OnceArc::new(),
                quality_grade: Cell::default(),
                name: OnceCell::new(),
                artist_string: OnceCell::new(),
                artists: InitOnceArc::new(),
                album: InitOnceArc::new(),
                stickers: RefCell::new(Stickers::default()),
                queue_pos: Cell::new(0),
                is_playing: Cell::new(false),
                last_played: OnceCell::new(),
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
            // Only existing or synchronously-initialised fields here.
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").read_only().build(),
                    ParamSpecString::builder("name").read_only().build(),
                    // All artist tags concatenated with ", ".
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecUInt64::builder("duration").read_only().build(),
                    ParamSpecChar::builder("rating").read_only().build(),
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
                "artist" => obj.get_artist_string().to_value(),
                "duration" => obj.get_duration().to_value(),
                "rating" => obj.get_rating().unwrap_or(-1).to_value(),
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
    }
}

glib::wrapper! {
    pub struct Song(ObjectSubclass<imp::Song>);
}

impl Song {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set_stickers(&self, stickers: Stickers) {
        self.imp().stickers.replace(stickers);
    }

    pub fn stickers(&self) -> Ref<'_, Stickers> {
        self.imp().stickers.borrow()
    }

    pub fn get_rating(&self) -> Option<i8> {
        self.stickers().rating
    }

    pub fn set_rating(&self, new: Option<i8>) {
        let old = self.get_rating();
        if new != old {
            self.imp().stickers.borrow_mut().rating = new;
            self.notify("rating");
        }
    }

    // ALL of the getters below require that the info field be initialised!
    pub fn get_inner(&self) -> &SongInfo {
        self.imp().info.get(Ordering::Relaxed).unwrap()
    }

    pub fn get_inner_arc(&self) -> Arc<SongInfo> {
        self.imp().info.load(Ordering::Relaxed).unwrap()
    }

    pub fn get_uri(&self) -> &str {
        &self.get_inner().file
    }

    pub fn get_folder_uri(&self) -> &str {
        strip_filename_linux(self.get_uri())
    }

    /// Display name for this song (or stream)
    pub fn get_name(&self) -> &str {
        self.imp().name.get_or_init(|| {
            let inner = self.get_inner();
            let path = Path::new(&inner.file);
            inner
                .title
                .as_deref()
                .or(inner.name.as_deref())
                .or_else(|| path.file_stem().map(|stem| stem.to_str()).flatten())
                .unwrap_or_else(|| "")
                .to_string()
        })
    }

    pub fn get_last_modified(&self) -> Option<&str> {
        self.get_inner().last_mod.as_deref()
    }

    pub fn init_last_played(&self, ts: OffsetDateTime) {
        let _ = self.imp().last_played.set(ts);
    }

    pub fn get_last_played_desc(&self) -> Option<String> {
        // TODO: translations
        self.get_last_played()
            .map(|then| get_time_ago_desc(then.unix_timestamp()))
    }

    pub fn get_last_played(&self) -> Option<&OffsetDateTime> {
        self.imp().last_played.get()
    }

    pub fn get_duration(&self) -> u64 {
        if let Some(dur) = self.get_inner().duration.as_ref() {
            return dur.as_secs();
        }
        0
    }

    // pub async fn get_artists_lazy(&self) -> Box<dyn Fn() -> Arc<Vec<ArtistInfo>>> {
    //     let inner = self.get_inner_arc();
    //     let artists_arc = self.imp().artists.clone();
    //     Box::new(move || {
    //         artists_arc.init(|| {
    //             // Flatten artist tag only since it's usually the less standardised one.
    //             // ArtistSort and musicbrainz_artistid are expected to follow the Vorbis
    //             // standard since they're likely tagged by proper autotagging software.
    //             let mut res: Vec<ArtistInfo> = Vec::new();
    //             let sort_tags = &inner.artistsorts;
    //             let mbids = &inner.artist_mbids;
    //             for (i, name) in inner
    //                 .artists
    //                 .iter()
    //                 .map(|s| parse_mb_artist_tag(s))
    //                 .flatten()
    //                 .enumerate()
    //             {
    //                 res.push(ArtistInfo {
    //                     name: name.to_owned(),
    //                     sort_tag: sort_tags.get(i).map(ToOwned::to_owned),
    //                     mbid: mbids.get(i).map(ToOwned::to_owned),
    //                     genres: FxHashSet::default(),
    //                     example_uris: Vec::with_capacity(0),
    //                     is_composer: false,
    //                     is_albumartist: false,
    //                 });
    //             }
    //             res
    //         })
    //     })
    //     // if let Some(artists) = self.imp().artists.load(Ordering::Acquire) {
    //     //     artists
    //     // } else {
    //     //     // Temporarily take out then put back in again since Shared future handles ain't Copy.
    //     //     match self.imp().artists_init.take() {
    //     //         InitState::Pending(fut) => {
    //     //             // SYNCHRONOUS PLZ
    //     //             self.imp().artists_init.set(InitState::Pending(fut.clone()));
    //     //             // Safe to await now
    //     //             fut.await;
    //     //         }
    //     //         InitState::Uninitialized => {
    //     //             let fut = self.init_artists().shared();
    //     //             self.imp().artists_init.set(InitState::Pending(
    //     //                 fut.clone()
    //     //             ));  // tell other callers to await with us
    //     //             // Safe to await now
    //     //             fut.await;
    //     //         }
    //     //     }
    //     //     self.imp().artists.load(Ordering::Acquire).unwrap()
    //     // }
    // }

    /// Get artist names separated by commas. Will look weird if a song was from artists
    /// whose names contain commas themselves, like Earth, Wind & Fire.
    pub fn get_artist_string(&self) -> &str {
        self.imp()
            .artist_string
            .get_or_init(|| self.get_inner().artists.join(", "))
            .as_str()
    }

    /// Get an iterator over artist tags for comparison/sorting purposes.
    pub fn get_artists(&self) -> impl Iterator<Item = &String> {
        self.get_inner().artists.iter()
    }

    pub fn get_queue_id(&self) -> u32 {
        self.get_inner().place.map(|p| p.id.0).unwrap_or(0)
    }

    pub fn get_queue_pos(&self) -> u32 {
        self.imp().queue_pos.get()
    }

    pub fn set_queue_pos(&self, new_pos: u32) {
        self.imp().queue_pos.set(new_pos);
    }

    pub fn is_queued(&self) -> bool {
        self.get_inner().place.is_some()
    }

    /// Just returns a "reader class". No actual data cloning.
    pub fn get_album_info(&self) -> Option<AlbumInfo> {
        AlbumInfo::from_mpd_songs(vec![self.get_inner_arc()])
        // self.imp().album.init(|| {
        //     Arc::new(if let Some(title) = inner.album.as_deref() {
        //     // Parse albumartists
        //     // let mut albumartists: Vec<ArtistInfo> = Vec::new();
        //     // let sort_tags = &inner.albumartistsorts;
        //     // let mbids = &inner.albumartist_mbids;
        //     // // Flatten artist tag only since it's usually the less standardised one.
        //     // // ArtistSort and musicbrainz_artistid are expected to follow the Vorbis
        //     // // standard since they're likely tagged by proper autotagging software.
        //     // for name in inner.albumartists
        //     //     .iter()
        //     //     .map(|s| parse_mb_artist_tag(s))
        //     //     .flatten()
        //     //     .enumerate()
        //     // {
        //     //     albumartists.push(ArtistInfo {
        //     //         name: name.to_owned(),
        //     //         sort_tag: sort_tags.get(i).map(ToOwned::to_owned),
        //     //         mbid: mbids.get(i).map(ToOwned::to_owned),
        //     //         genres: FxHashSet::default(),
        //     //         example_uris: Vec::with_capacity(0),
        //     //         is_composer: false,
        //     //         is_albumartist: false,
        //     //     });
        //     // }
        //     Some(AlbumInfo {
        //         title,
        //         albumsort: inner.albumsort.clone(),
        //         example_uri: inner.file.clone(),
        //         albumartists: inner.albumartists.clone(),
        //         albumartistsorts: inner.albumartistsorts.clone(),
        //         genres: inner.genres.clone(),
        //         cover: None,
        //         release_date: inner.release_date.as_deref().or(inner.original_date.as_deref()).map(|s| s.to_owned()),

        //     })

        //     if let Some(album) = res.album.as_mut() {
        //         album.mbid = album_mbid;
        //         album.albumsort = albumsort; //.or_else(|| {res.artist_tag.clone()});
        //         album.albumartistsort = albumartistsort;
        //         album.release_date = res.release_date;
        //         album.date_tag = date_tag;
        //         if !genre_tags.is_empty() {
        //             album.add_genres_from_tags(&genre_tags);
        //         }
        //         // Assume the albumartist IDs are given in the same order as the albumartist tags
        //         if let Some(album_artist_str) =
        //             albumartist.as_ref().or_else(|| res.artist_tag.as_ref())
        //         {
        //             album.add_artists_from_string(album_artist_str);
        //         }
        //         album.albumartist = albumartist;
        //         for (idx, id) in album_artist_mbids.drain(..).enumerate() {
        //             if idx < album.artists.len() {
        //                 let _ = album.artists[idx].mbid.replace(id);
        //             }
        //         }
        //     }
        //     Arc::new(Some(AlbumInfo::new(&song.file))>
        // } else {
        //     None
        // })
        // })
        // let res = APP_THREAD_POOL.push_future(move || {
        //     let mut album_info: Option<AlbumInfo> =
        // }).expect("glib threadpool error").await.expect("glib threadpool error");
        // self.imp().artists.store(Arc::new(res), Ordering::Relaxed);
    }

    pub fn get_album_title(&self) -> Option<&str> {
        self.get_inner().album.as_deref()
    }

    /// Starts from 1
    pub fn get_track(&self) -> u32 {
        self.get_inner().track.unwrap_or(0)
    }

    /// Starts from 1
    pub fn get_disc(&self) -> u32 {
        self.get_inner().disc.unwrap_or(0)
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
        self.imp().quality_grade.get()
    }

    pub fn get_release_date(&self) -> Option<Date> {
        let inner = self.get_inner();
        inner
            .date
            .as_deref()
            .or(inner.original_date.as_deref())
            .map(parse_date)
            .flatten()
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.get_inner().mbid.as_deref()
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
        let inner = self.get_inner();
        if let Some(album) = inner.album.as_deref() {
            meta.set_album(Some(album));
            if !inner.albumartists.is_empty() {
                meta.set_album_artist(Some(&inner.albumartists));
            }
        }
        if inner.artists.is_empty() {
            meta.set_artist(Some(&inner.artists))
        }

        // Album art, if available
        if let Some(thumbnail_name) =
            sqlite::find_cover_by_uri(&inner.file, true).expect("Sqlite DB error")
        {
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

    pub fn get_comp_id(&self) -> &str {
        let inner = self.get_inner();
        inner.mbid.as_deref().unwrap_or(inner.file.as_ref())
    }

    /// Get something to serve as this song's album's internal ID for
    /// filtering/comparison purposes.
    /// If an album has a MusicBrainz ID, use it. If not, fall
    /// back to their folder URI.
    /// This is a shortcut to compare songs by their albums without actually
    /// parsing into Album objects themselves.
    pub fn album_comp_id(&self) -> Option<&str> {
        let inner = self.get_inner();
        if inner.album.is_some() {
            inner
                .album_mbid
                .as_deref()
                .or(Some(strip_filename_linux(&inner.file)))
        } else {
            None
        }
    }
}

impl Default for Song {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<SongInfo> for Song {
    fn from(song: SongInfo) -> Self {
        // Most of the parsing is now done by our rust-mpd fork.
        // This from() implementation now only takes care of Euphonica-specific stuff.

        // // Parse artists
        // let artists: Vec<ArtistInfo> = zip_longest(
        //     // Flatten artist tag only since it's usually the less standardised one.
        //     // ArtistSort and musicbrainz_artistid are expected to follow the Vorbis
        //     // standard since they're likely tagged by proper autotagging software.
        //     song.artists
        //         .into_iter()
        //         .map(|s| parse_mb_artist_tag(&s))
        //         .flatten()
        //         .map(|s| s.to_owned()),
        //     zip_longest(song.artistsorts, song.artist_mbids),
        // )
        // .map(|(name, (sort_tag, mbid))| ArtistInfo {
        //     name,
        //     sort_tag,
        //     mbid,
        //     genres: FxHashSet::default(),
        //     example_uris: Vec::with_capacity(0),
        //     is_composer: false,
        //     is_albumartist: false,
        // })
        // .collect();

        // Parse album information
        // let mut album_info: Option<AlbumInfo> = if let Some(album) = song.album {
        //     // Parse albumartists
        //     let albumartists: Vec<ArtistInfo> = zip_longest(
        //         // Flatten artist tag only since it's usually the less standardised one.
        //         // ArtistSort and musicbrainz_artistid are expected to follow the Vorbis
        //         // standard since they're likely tagged by proper autotagging software.
        //         song.albumartists
        //             .into_iter()
        //             .map(|s| parse_mb_artist_tag(&s))
        //             .flatten()
        //             .map(|s| s.to_owned()),
        //         zip_longest(song.albumartistsorts, song.albumartist_mbids),
        //     )
        //     .map(|(name, (sort_tag, mbid))| ArtistInfo {
        //         name,
        //         sort_tag,
        //         mbid,
        //         genres: FxHashSet::default(),
        //         example_uris: Vec::with_capacity(0),
        //         is_composer: false,
        //         is_albumartist: true,
        //     })
        //     .collect();
        //     if let Some(album) = res.album.as_mut() {
        //         album.mbid = album_mbid;
        //         album.albumsort = albumsort; //.or_else(|| {res.artist_tag.clone()});
        //         album.albumartistsort = albumartistsort;
        //         album.release_date = res.release_date;
        //         album.date_tag = date_tag;
        //         if !genre_tags.is_empty() {
        //             album.add_genres_from_tags(&genre_tags);
        //         }
        //         // Assume the albumartist IDs are given in the same order as the albumartist tags
        //         if let Some(album_artist_str) =
        //             albumartist.as_ref().or_else(|| res.artist_tag.as_ref())
        //         {
        //             album.add_artists_from_string(album_artist_str);
        //         }
        //         album.albumartist = albumartist;
        //         for (idx, id) in album_artist_mbids.drain(..).enumerate() {
        //             if idx < album.artists.len() {
        //                 let _ = album.artists[idx].mbid.replace(id);
        //             }
        //         }
        //     }
        //     Some(AlbumInfo::new(&song.file))
        // } else {
        //     None
        // };

        let res: Self = glib::Object::new();
        let imp = res.imp();
        if let Some(place) = &song.place {
            imp.queue_pos.set(place.pos);
        }
        imp.quality_grade.set(QualityGrade::of_mpd_song(&song));
        let _ = imp.info.store(Arc::new(song), Ordering::Release);

        res
    }
}
