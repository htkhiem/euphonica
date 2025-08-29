use crate::utils::{ARTIST_DELIM_AUTOMATON, ARTIST_DELIM_EXCEPTION_AUTOMATON};
use aho_corasick::Match;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use std::cell::OnceCell;

/// Artist struct, for use with both Artist and AlbumArtist tags.
#[derive(Debug, Clone, PartialEq)]
pub struct ArtistInfo {
    // TODO: Might want to refactor to Into<Cow<'a, str>>
    pub name: String,
    pub mbid: Option<String>,
    pub is_composer: bool,
}

impl ArtistInfo {
    pub fn new(name: &str, is_composer: bool) -> Self {
        Self {
            name: name.to_owned(),
            mbid: None,
            is_composer,
        }
    }
}

impl Default for ArtistInfo {
    fn default() -> Self {
        ArtistInfo {
            name: "Untitled Artist".to_owned(),
            mbid: None,
            is_composer: false,
        }
    }
}

/// Utility function to create a list of ArtistInfo objects from a MusicBrainz Artist tag.
/// Can be used with AlbumArtist tag too, but NOT with with ArtistSort or AlbumArtistSort tags.
/// Internally, we rely on two passes of the Aho-Corasick algorithm, with the first used to
/// pick up "exceptions" and the second to locate delimiters.
/// The "exceptions" pass is necessary due to some artist names having delimiter-like
/// substrings. Examples: the ampersand (&) is a popular delimiter, but then Simon & Garfunkel
/// exists; likewise, the forward slash (/) is sometimes also used, but what about AC/DC?
pub fn parse_mb_artist_tag<'a>(input: &'a str) -> Vec<&'a str> {
    let mut buffer: String = input.to_owned();
    // println!("Original buffer len: {}", buffer.len());
    if let (Some(exc_ac), Some(delim_ac)) = (
        &*ARTIST_DELIM_EXCEPTION_AUTOMATON.read().unwrap(),
        &*ARTIST_DELIM_AUTOMATON.read().unwrap(),
    ) {
        // Step 1: extract exceptions out first
        let mut found_artists: Vec<&str> = Vec::new();
        for mat in exc_ac.find_iter(input) {
            // Remove from buffer. Should now cause a reallocation since we are not
            // using any extra storage.
            let start = mat.start();
            let end = mat.end();
            found_artists.push(&input[start..end]);
            let len = end - start;
            buffer.replace_range(start..end, &" ".repeat(len));
            // println!("Buffer is now: {buffer}");
        }

        // Step 2: split the remaining buffer. Here we again make use of the
        // Aho-Corasick algorithm to find all delimiters.
        let matched_delims = delim_ac.find_iter(&buffer).collect::<Vec<Match>>();
        if matched_delims.is_empty() {
            // In case no delimiter is found but there are artists detected by exception rules
            // in the above pass, return those exceptions.
            if !found_artists.is_empty() {
                return found_artists;
            }
            // Else return the whole string
            return vec![input];
            // Incorrect outputs are due to unspecified delimiters.
        } else {
            // Take note to check for "blankness" using the buffer, but return slices
            // of input, since buffer will go out of scope after this function concludes.
            let first_range = 0..matched_delims[0].start();
            if buffer[first_range.clone()].trim().len() > 0 {
                found_artists.push(input[first_range].trim());
            }
            for i in 1..(matched_delims.len()) {
                let between_range = matched_delims[i - 1].end()..matched_delims[i].start();
                // println!("Between: `{between_range:?}`");
                if buffer[between_range.clone()].trim().len() > 0 {
                    found_artists.push(input[between_range].trim());
                }
            }
            let last_range = matched_delims.last().unwrap().end().min(buffer.len())..;
            if buffer[last_range.clone()].trim().len() > 0 {
                found_artists.push(input[last_range].trim());
            }
            return found_artists;
        }
    } else {
        vec![input]
    }
}

pub fn artists_to_string(artists: &[ArtistInfo]) -> Option<String> {
    if artists.is_empty() {
        None
    } else if artists.len() > 1 {
        // For now assume that only the first artist in the list can be a composer
        let mut res: String = "".to_owned();
        for (i, artist) in artists.iter().enumerate() {
            if i > 0 {
                let sep = if artists[i - 1].is_composer {
                    "; "
                } else {
                    ", "
                };
                res.push_str(sep);
            }
            res.push_str(artist.name.as_ref());
        }
        Some(res)
    } else {
        Some(artists[0].name.clone())
    }
}

mod imp {
    use super::*;
    use glib::{ParamSpec, ParamSpecString};
    use once_cell::sync::Lazy;

    #[derive(Default, Debug)]
    pub struct Artist {
        pub info: OnceCell<ArtistInfo>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Artist {
        const NAME: &'static str = "EuphonicaArtist";
        type Type = super::Artist;

        fn new() -> Self {
            Self {
                info: OnceCell::new(),
            }
        }
    }

    impl ObjectImpl for Artist {
        fn properties() -> &'static [ParamSpec] {
            static PROPERTIES: Lazy<Vec<ParamSpec>> =
                Lazy::new(|| vec![ParamSpecString::builder("name").read_only().build()]);
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &ParamSpec) -> glib::Value {
            let obj = self.obj();
            match pspec.name() {
                "name" => obj.get_name().to_value(),
                _ => unimplemented!(),
            }
        }
    }
}

glib::wrapper! {
    pub struct Artist(ObjectSubclass<imp::Artist>);
}

impl Artist {
    pub fn get_info(&self) -> &ArtistInfo {
        self.imp().info.get().unwrap()
    }

    pub fn get_name(&self) -> &str {
        &self.get_info().name
    }

    pub fn get_mbid(&self) -> Option<&str> {
        self.get_info().mbid.as_deref()
    }

    pub fn is_composer(&self) -> bool {
        self.get_info().is_composer
    }
}

impl Default for Artist {
    fn default() -> Self {
        glib::Object::new()
    }
}

impl From<ArtistInfo> for Artist {
    fn from(info: ArtistInfo) -> Self {
        let res = glib::Object::builder::<Self>().build();
        let _ = res.imp().info.set(info);
        res
    }
}
