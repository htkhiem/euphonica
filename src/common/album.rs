use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use itertools::Itertools;
use mpd::Query;
use mpd::Term;
use std::cell::{OnceCell, RefCell};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use time::Date;

use crate::common::parse_date;
use crate::common::tags;
use crate::utils::mpd_args_to_string;
use crate::utils::strip_filename_linux;

use super::{QualityGrade, Stickers};

/// From v1.0 the AlbumInfo class is just a "reader class", in that it simply keeps a strong ref to
/// one or more mpd::song::Song objects from which it reads its data.
#[derive(Debug, Clone, PartialEq)]
pub struct AlbumInfo {
    pub first_song: Arc<mpd::song::Song>, // keep a ref straight to the first one to reduce indirection
    pub songs: Vec<Arc<mpd::song::Song>>,
}

impl AlbumInfo {
    /// Build a thin Album wrapper around mpd::song::Song structs.
    /// Note that the FIRST song in the list MUST have an album tag.
    /// Most info will be read off the first song. Some will be from all songs.
    pub fn from_mpd_songs(songs: Vec<Arc<mpd::song::Song>>) -> Option<Self> {
        assert!(!songs.is_empty());
        if songs[0].album.is_some() {
            Some(Self {
                first_song: songs[0].clone(),
                songs,
            })
        } else {
            None
        }
    }

    pub fn get_folder_uri(&self) -> &str {
        strip_filename_linux(&self.first_song.file)
    }

    pub fn get_title(&self) -> &str {
        self.first_song.album.as_deref().unwrap_or("")
    }

    /// Get something to serve as this album's internal ID for
    /// filtering/comparison purposes.
    /// If an album has a MusicBrainz ID, use it. If not, fall
    /// back to their folder URI.
    pub fn get_comp_id(&self) -> &str {
        self.first_song
            .mbid
            .as_deref()
            .unwrap_or_else(|| self.get_folder_uri())
    }

    pub fn get_date(&self) -> Option<&str> {
        self.first_song
            .date
            .as_deref()
            .or(self.first_song.original_date.as_deref())
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.first_song.album_mbid.as_deref()
    }

    /// Get MPD filter expression to find all songs by this album.
    /// Main use is as this album's identifier within MPD's stickers DB.
    /// This avoids the issue of all same-named albums receiving the same stickers.
    /// Fallback is AlbumArtist + Album + Date, as is the case with myMPD.
    /// See https://github.com/jcorporation/mpd-stickers/discussions/10.
    pub fn get_filter_expression(&self) -> String {
        let mut q = Query::new();
        let song = &self.first_song;
        if let Some(mbid) = song.mbid.as_deref() {
            q.and(Term::Tag(tags::ALBUM_MBID.into()), mbid.to_owned());
        } else {
            q.and(
                Term::Tag(tags::ALBUMARTIST.into()),
                // For now only key by the first
                song.albumartists.get(0).map(|s| s.as_str()).unwrap_or(""),
            );
            q.and(
                Term::Tag(tags::ALBUM.into()),
                song.album.as_deref().unwrap_or(""),
            );
            if let Some(date) = song.date.as_deref() {
                q.and(Term::Tag(tags::DATE.into()), date);
            } else if let Some(date) = song.original_date.as_deref() {
                q.and(Term::Tag(tags::ORIGINAL_DATE.into()), date);
            }
        }
        mpd_args_to_string(&q)
    }

    /// Only comes from the first song.
    pub fn albumartists(&self) -> impl Iterator<Item = &str> {
        self.first_song.albumartists.iter().map(|s| s.as_str())
    }

    /// Only comes from the first song.
    pub fn albumartistsorts(&self) -> impl Iterator<Item = &str> {
        self.first_song.albumartistsorts.iter().map(|s| s.as_str())
    }

    /// Should be cached
    pub fn get_unique_genres(&self) -> impl Iterator<Item = &str> {
        self.songs
            .iter()
            .map(|song| song.genres.iter().map(|s| s.as_str()))
            .flatten()
            .unique()
    }

    pub fn example_uri(&self) -> &str {
        &self.first_song.file
    }
}

mod imp {
    use super::*;
    use glib::{ParamSpec, ParamSpecChar, ParamSpecObject, ParamSpecString};
    use once_arc::OnceArc;
    use once_cell::sync::Lazy;

    /// The GObject Song wrapper.
    /// By nesting info inside another struct, we enforce tag editing to be
    /// atomic. Tag editing is performed by first cloning the whole SongInfo
    /// struct to a mutable variable, modify it, then create a new Song wrapper
    /// from the modified SongInfo struct (no copy required this time).
    /// This design also avoids a RefCell.
    /// Album contains a SongInfo of a random song in that album, which in turn
    /// contains a nested AlbumInfo.
    /// In other words, an Album actually also contains a random song's information.
    /// This is to facilitate fallback between folder cover and embedded track cover.
    #[derive(Default, Debug)]
    pub struct Album {
        pub info: OnceArc<AlbumInfo>,
        pub albumartists_string: OnceCell<Option<String>>,
        pub albumartistsorts_string: OnceCell<Option<String>>,
        pub date: OnceCell<Option<Date>>,
        pub genres: OnceCell<Vec<String>>,
        pub stickers: RefCell<Stickers>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Album {
        const NAME: &'static str = "EuphonicaAlbum";
        type Type = super::Album;

        fn new() -> Self {
            Self {
                info: OnceArc::new(),
                albumartists_string: OnceCell::new(),
                albumartistsorts_string: OnceCell::new(),
                date: OnceCell::new(),
                genres: OnceCell::new(),
                stickers: RefCell::new(Stickers::default()),
            }
        }
    }

    impl ObjectImpl for Album {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").read_only().build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecString::builder("sortable-title")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("artist").read_only().build(),
                    ParamSpecChar::builder("rating").build(),
                    ParamSpecObject::builder::<glib::BoxedAnyObject>("release-date")
                        .read_only()
                        .build(),
                    ParamSpecString::builder("quality-grade")
                        .read_only()
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "uri" => obj.get_folder_uri().to_value(),
                "title" => obj.get_title().to_value(),
                "sortable-title" => obj.get_sortable_title().to_value(),
                "artist" => obj.get_albumartists_str().to_value(),
                "rating" => obj.get_rating().unwrap_or(-1).to_value(),
                "release-date" => glib::BoxedAnyObject::new(obj.get_release_date()).to_value(),
                "quality-grade" => obj.get_quality_grade().to_icon_name().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &ParamSpec) {
            let obj = self.obj();
            match pspec.name() {
                "rating" => {
                    if let (Ok(r), mut st) = (value.get::<i8>(), self.stickers.borrow_mut()) {
                        let new_rating: Option<i8> = if r < 0 { Some(r) } else { None };
                        if st.rating != new_rating {
                            st.rating = new_rating;
                            obj.notify("rating");
                        }
                    }
                }
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct Album(ObjectSubclass<imp::Album>);
}

impl Album {
    // ALL of the getters below require that the info field be initialised!
    pub fn get_inner(&self) -> &AlbumInfo {
        self.imp().info.get(Ordering::Relaxed).unwrap()
    }

    pub fn get_inner_arc(&self) -> Arc<AlbumInfo> {
        self.imp().info.load(Ordering::Relaxed).unwrap()
    }

    pub fn get_folder_uri(&self) -> &str {
        &self.get_inner().get_folder_uri()
    }

    pub fn get_example_uri(&self) -> &str {
        &self.get_inner().example_uri()
    }

    pub fn get_title(&self) -> &str {
        &self.get_inner().get_title()
    }

    pub fn get_sortable_title(&self) -> &str {
        let inner = self.get_inner();
        inner
            .first_song
            .albumsort
            .as_deref()
            .unwrap_or_else(|| inner.get_title())
    }

    pub fn get_albumartists(&self) -> impl Iterator<Item = &str> {
        self.get_inner().albumartists()
    }

    pub fn get_albumartistsorts(&self) -> impl Iterator<Item = &str> {
        self.get_inner().albumartistsorts()
    }

    /// Cached, hence no longer impl Iterator.
    pub fn get_genres(&self) -> Vec<&str> {
        self.imp()
            .genres
            .get_or_init(|| {
                self.get_inner()
                    .get_unique_genres()
                    .map(|s| s.to_owned())
                    .collect()
            })
            .iter()
            .map(|s| s.as_str())
            .collect()
    }

    /// Concatenated version, cached for sorting and filtering
    pub fn get_albumartists_str(&self) -> Option<&str> {
        self.imp()
            .albumartists_string
            .get_or_init(|| {
                let aa: Vec<&str> = self.get_inner().albumartists().collect();
                if aa.is_empty() {
                    None
                } else {
                    Some(aa.join(", "))
                }
            })
            .as_deref()
    }

    /// Get the original ALBUMARTISTSORT tags.
    pub fn get_sortable_artist_tag(&self) -> Option<&str> {
        self.imp()
            .albumartistsorts_string
            .get_or_init(|| {
                let aa: Vec<&str> = self.get_inner().albumartistsorts().collect();
                if aa.is_empty() {
                    None
                } else {
                    Some(aa.join(", "))
                }
            })
            .as_deref()
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.get_inner().get_mbid()
    }

    /// Cached due to nontrivial parsing
    pub fn get_release_date(&self) -> Option<Date> {
        *self
            .imp()
            .date
            .get_or_init(|| self.get_inner().get_date().map(parse_date).flatten())
    }

    pub fn get_quality_grade(&self) -> QualityGrade {
        QualityGrade::of_mpd_song(self.get_inner().first_song.as_ref())
    }

    pub fn get_rating(&self) -> Option<i8> {
        self.imp().stickers.borrow().rating
    }

    pub fn set_rating(&self, new: Option<i8>) {
        let old = self.get_rating();
        if new != old {
            self.imp().stickers.borrow_mut().rating = new;
            self.notify("rating");
        }
    }

    pub fn get_stickers(&self) -> &RefCell<Stickers> {
        &self.imp().stickers
    }

    /// Call this after mutating stickers to alert UI widgets
    pub fn notify_stickers_changed(&self) {
        self.notify("rating");
    }

    pub fn set_stickers(&self, stickers: Stickers) {
        let _ = &self.imp().stickers.replace(stickers);
    }

    /// Checks if the given genre substring is in any of this album's genre strings.
    /// Genre-less albums always return false.
    pub fn has_genre(&self, genre_substr: &str) -> bool {
        self.get_genres().contains(&genre_substr)
    }
}

impl Default for Album {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<AlbumInfo> for Album {
    fn from(info: AlbumInfo) -> Self {
        let res = glib::Object::builder::<Self>().build();
        let _ = res.imp().info.store(Arc::new(info), Ordering::Release);
        res
    }
}
