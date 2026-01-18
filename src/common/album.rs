use gtk::gdk::Texture;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::{OnceCell, RefCell};
use time::Date;

use crate::utils::strip_filename_linux;

use super::{ArtistInfo, QualityGrade, SongInfo, Stickers, artists_to_string, parse_mb_artist_tag};

// This is a model class for queue view displays.
// It does not contain any actual song in terms of data.

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumInfo {
    // TODO: Might want to refactor to Into<Cow<'a, str>>
    pub title: String,
    pub albumsort: Option<String>,
    // Example track-level URI, acquired from the first song found with this album's tag.
    // This facilitates falling back to embedded covers in case there's no cover file in the folder.
    pub example_uri: String,
    // The above but with filename removed, for looking up folder-level covers.
    pub folder_uri: String,
    pub artists: Vec<ArtistInfo>, // parse from AlbumArtist tag please, not Artist.
    pub albumartist: Option<String>,
    pub albumartistsort: Option<String>,
    pub cover: Option<Texture>,
    pub release_date: Option<Date>,
    pub quality_grade: QualityGrade,
    pub mbid: Option<String>,
}

impl AlbumInfo {
    pub fn new(
        example_uri: &str,
        title: &str,
        albumsort: Option<&str>,
        artist_tag: Option<&str>,
        albumartistsort: Option<&str>,
        artists: Vec<ArtistInfo>,
        quality_grade: QualityGrade,
    ) -> Self {
        Self {
            example_uri: example_uri.to_owned(),
            folder_uri: strip_filename_linux(example_uri).to_owned(),
            albumsort: albumsort.map(|s| s.to_owned()),
            artists,
            albumartist: artist_tag.map(str::to_owned),
            albumartistsort: albumartistsort.map(str::to_owned),
            title: title.to_owned(),
            cover: None,
            release_date: None,
            quality_grade,
            mbid: None,
        }
    }

    /// Get something to serve as this album's internal ID for
    /// filtering/comparison purposes.
    /// If an album has a MusicBrainz ID, use it. If not, fall
    /// back to their folder URI.
    pub fn get_comp_id(&self) -> &str {
        self.mbid.as_deref().unwrap_or(self.folder_uri.as_ref())
    }

    /// Add artists from more artist tags, separated from existing ones by simple commas.
    pub fn add_artists_from_string(&mut self, tag: &str) {
        if let Some(existing_tag) = &mut self.albumartist {
            existing_tag.push_str(", ");
            existing_tag.push_str(tag);
        } else {
            self.albumartist = Some(tag.to_owned());
        }

        let mut new_artists: Vec<ArtistInfo> = parse_mb_artist_tag(tag)
            .iter()
            .map(|s| ArtistInfo::new(s, None, false))
            .collect();

        self.artists.append(&mut new_artists);
    }

    pub fn get_artist_str(&self) -> Option<String> {
        artists_to_string(&self.artists)
    }

    pub fn get_artist_tag(&self) -> Option<&str> {
        self.albumartist.as_deref()
    }
}

impl Default for AlbumInfo {
    fn default() -> Self {
        AlbumInfo {
            title: "".to_owned(),
            albumsort: None,
            example_uri: "".to_owned(),
            folder_uri: "".to_owned(),
            artists: Vec::with_capacity(0),
            albumartist: None,
            albumartistsort: None,
            cover: None,
            release_date: None,
            quality_grade: QualityGrade::Unknown,
            mbid: None,
        }
    }
}

impl From<SongInfo> for AlbumInfo {
    fn from(song_info: SongInfo) -> Self {
        song_info.into_album_info().unwrap()
    }
}

mod imp {
    use super::*;
    use glib::{ParamSpec, ParamSpecChar, ParamSpecObject, ParamSpecString};
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
        pub info: OnceCell<AlbumInfo>,
        pub stickers: RefCell<Stickers>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Album {
        const NAME: &'static str = "EuphonicaAlbum";
        type Type = super::Album;

        fn new() -> Self {
            Self {
                info: OnceCell::new(),
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
                "artist" => obj.get_artist_str().to_value(),
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
    pub fn get_info(&self) -> &AlbumInfo {
        self.imp().info.get().unwrap()
    }

    pub fn get_folder_uri(&self) -> &str {
        &self.get_info().folder_uri
    }

    pub fn get_example_uri(&self) -> &str {
        &self.get_info().example_uri
    }

    pub fn get_title(&self) -> &str {
        &self.get_info().title
    }

    pub fn get_sortable_title(&self) -> &str {
        let info = self.get_info();
        info.albumsort.as_deref().unwrap_or(info.title.as_str())
    }

    pub fn get_artists(&self) -> &[ArtistInfo] {
        &self.get_info().artists
    }

    /// Get albumartist names separated by commas. If the first artist listed is a composer,
    /// the next separator will be a semicolon instead. The quality of this output depends
    /// on whether all delimiters are specified by the user.
    pub fn get_artist_str(&self) -> Option<String> {
        artists_to_string(&self.get_info().artists)
    }

    /// Get the original albumartist tag before any parsing.
    pub fn get_artist_tag(&self) -> Option<&str> {
        self.get_info().albumartist.as_deref()
    }

    /// Get the original ALBUMARTISTSORT tag.
    pub fn get_sortable_artist_tag(&self) -> Option<&str> {
        let info = self.get_info();
        if let Some(albumartistsort) = info.albumartistsort.as_deref() {
            Some(albumartistsort)
        } else if let Some(albumartist) = info.albumartist.as_deref() {
            Some(albumartist)
        } else {
            None
        }
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.get_info().mbid.as_deref()
    }

    pub fn get_release_date(&self) -> Option<Date> {
        self.get_info().release_date
    }

    pub fn get_quality_grade(&self) -> QualityGrade {
        self.get_info().quality_grade
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
}

impl Default for Album {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<AlbumInfo> for Album {
    fn from(info: AlbumInfo) -> Self {
        let res = glib::Object::builder::<Self>().build();
        let _ = res.imp().info.set(info);
        res
    }
}
