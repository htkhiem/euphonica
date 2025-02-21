use gtk::gdk::Texture;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::OnceCell;
use time::Date;

use super::{artists_to_string, parse_mb_artist_tag, ArtistInfo, QualityGrade, SongInfo};

// This is a model class for queue view displays.
// It does not contain any actual song in terms of data.

#[derive(Debug, Clone, PartialEq)]
pub struct AlbumInfo {
    // TODO: Might want to refactor to Into<Cow<'a, str>>
    pub title: String,
    // Folder-based URI, acquired from the first song found with this album's tag.
    pub uri: String,
    pub artists: Vec<ArtistInfo>, // parse from AlbumArtist tag please, not Artist.
    pub artist_tag: Option<String>,
    pub cover: Option<Texture>,
    pub release_date: Option<Date>,
    pub quality_grade: QualityGrade,
    pub mbid: Option<String>,
}

impl AlbumInfo {
    pub fn new(
        uri: &str,
        title: &str,
        artist_tag: Option<&str>,
        artists: Vec<ArtistInfo>,
        quality_grade: QualityGrade,
    ) -> Self {
        Self {
            uri: uri.to_owned(),
            artists,
            artist_tag: artist_tag.map(str::to_owned),
            title: title.to_owned(),
            cover: None,
            release_date: None,
            quality_grade,
            mbid: None,
        }
    }

    pub fn set_artists_from_string(&mut self, tag: &str) {
        self.artist_tag = Some(tag.to_owned());
        self.artists = parse_mb_artist_tag(tag)
            .iter()
            .map(|s| ArtistInfo::new(s, false))
            .collect();
    }

    pub fn get_artist_str(&self) -> Option<String> {
        artists_to_string(&self.artists)
    }

    pub fn get_artist_tag(&self) -> Option<&str> {
        self.artist_tag.as_deref()
    }
}

impl Default for AlbumInfo {
    fn default() -> Self {
        AlbumInfo {
            title: "Untitled Album".to_owned(),
            uri: "".to_owned(),
            artists: Vec::with_capacity(0),
            artist_tag: None,
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
    use glib::{
        ParamSpec,
        ParamSpecObject,
        // ParamSpecUInt,
        // ParamSpecUInt64,
        // ParamSpecBoolean,
        ParamSpecString,
    };
    use once_cell::sync::Lazy;

    /// The GObject Song wrapper.
    /// By nesting info inside another struct, we enforce tag editing to be
    /// atomic. Tag editing is performed by first cloning the whole SongInfo
    /// struct to a mutable variable, modify it, then create a new Song wrapper
    /// from the modified SongInfo struct (no copy required this time).
    /// This design also avoids a RefCell.
    #[derive(Default, Debug)]
    pub struct Album {
        pub info: OnceCell<AlbumInfo>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Album {
        const NAME: &'static str = "EuphonicaAlbum";
        type Type = super::Album;

        fn new() -> Self {
            Self {
                info: OnceCell::new(),
            }
        }
    }

    impl ObjectImpl for Album {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> = Lazy::new(|| {
                vec![
                    ParamSpecString::builder("uri").read_only().build(),
                    ParamSpecString::builder("title").read_only().build(),
                    ParamSpecString::builder("artist").read_only().build(),
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
                "uri" => obj.get_uri().to_value(),
                "title" => obj.get_title().to_value(),
                "artist" => obj.get_artist_str().to_value(),
                "release-date" => glib::BoxedAnyObject::new(obj.get_release_date()).to_value(),
                "quality-grade" => obj.get_quality_grade().to_icon_name().to_value(),
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
        &self.imp().info.get().unwrap()
    }

    pub fn get_uri(&self) -> &str {
        &self.get_info().uri
    }

    pub fn get_title(&self) -> &str {
        &self.get_info().title
    }

    pub fn get_artists(&self) -> &[ArtistInfo] {
        &self.get_info().artists
    }

    /// Get albumartist names separated by commas. If the first artist listed is a composer,
    /// the next separator will be a semicolon insead. The quality of this output depends
    /// on whether all delimiters are specified by the user.
    pub fn get_artist_str(&self) -> Option<String> {
        artists_to_string(&self.get_info().artists)
    }

    /// Get the original albumartist tag before any parsing.
    pub fn get_artist_tag(&self) -> Option<&str> {
        self.get_info().artist_tag.as_deref()
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.get_info().mbid.as_deref()
    }

    pub fn get_release_date(&self) -> Option<Date> {
        self.get_info().release_date.clone()
    }

    pub fn get_quality_grade(&self) -> QualityGrade {
        self.get_info().quality_grade.clone()
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
