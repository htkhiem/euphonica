pub const ALBUM: &str = "album";
pub const ARTIST: &str = "artist";
pub const ALBUMSORT: &str = "albumsort";
pub const ALBUMARTIST: &str = "albumartist";
pub const ARTISTSORT: &str = "artistsort";
pub const ALBUMARTISTSORT: &str = "albumartistsort";
pub const GENRE: &str = "genre";
pub const FORMAT: &str = "format";
pub const ORIGINAL_DATE: &str = "originaldate";
pub const DATE: &str = "date";
pub const TRACK: &str = "track";
pub const DISC: &str = "disc";
pub const COMPOSER: &str = "composer";
pub const COMPOSERSORT: &str = "composersort";
pub const LABEL: &str = "label";
pub const SONG_MBID: &str = "musicbrainz_trackid";
pub const ALBUM_MBID: &str = "musicbrainz_albumid";
pub const ARTIST_MBID: &str = "musicbrainz_artistid";
pub const ALBUMARTIST_MBID: &str = "musicbrainz_albumartistid";
pub const RELEASEGROUP_MBID: &str = "musicbrainz_releasegroupid";
pub const RELEASETRACK_MBID: &str = "musicbrainz_releasetrackid";
pub const WORK_MBID: &str = "musicbrainz_workid";
pub const ADDED: &str = "added";
pub const LAST_MODIFIED: &str = "last-modified";

/// Returns a human-friendly title for a lowercased MPD tag name.
/// String return for later translatability.
pub fn prettify_tag_name(name: &str) -> String {
    match name {
        ALBUM => "Album".into(),
        ARTIST => "Artist".into(),
        ARTISTSORT => "Artist sort".into(),
        ALBUMARTIST => "Album artist".into(),
        ALBUMARTISTSORT => "Album artist sort".into(),
        COMPOSER => "Composer".into(),
        COMPOSERSORT => "Composer sort".into(),
        DATE => "Date".into(),
        ORIGINAL_DATE => "Original date".into(),
        TRACK => "Track number".into(),
        DISC => "Disc number".into(),
        GENRE => "Genre".into(),
        LABEL => "Label".into(),
        SONG_MBID => "MusicBrainz Track IDs".into(),
        ALBUM_MBID => "MusicBrainz Album IDs".into(),
        ARTIST_MBID => "MusicBrainz Artist IDs".into(),
        ALBUMARTIST_MBID => "MusicBrainz Album Artist IDs".into(),
        RELEASEGROUP_MBID => "MusicBrainz Release Group IDs".into(),
        RELEASETRACK_MBID => "MusicBrainz Release Track IDs".into(),
        WORK_MBID => "MusicBrainz Work IDs".into(),
        // Unknown tags, including any future/other MusicBrainz keys
        _ => prettify_tag_name_generic(name),
    }
}

/// Prettify an arbitrary tag name, e.g. `my_custom_tag` -> `My Custom Tag`.
pub fn prettify_tag_name_generic(name: &str) -> String {
    name.split('_')
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_string().to_uppercase() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
