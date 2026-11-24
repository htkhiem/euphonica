use gtk::{Ordering, gio, gio::prelude::SettingsExt};
use strum_macros::{Display, EnumMessage, EnumString, VariantArray};

use crate::{
    common::Song,
    library::{SortCriterion, SortDirection, SortTier},
    utils::{g_cmp_options, g_cmp_str_options, g_cmp_str_vecs},
};

pub const SONG_SORT_SETTINGS: [(&str, &str); 3] = [
    ("sort-by", "sort-direction"),
    ("sort-by-2", "sort-direction-2"),
    ("sort-by-3", "sort-direction-3"),
];

/// Variants are in the order of the sort-by dropdown items.
/// `serialize` is the nick in the gschema's `io.github.htkhiem.Euphonica.sortby` enum,
/// and `to_string` is the dropdown label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, EnumMessage, EnumString, VariantArray)]
pub enum SongSortCriterion {
    #[strum(serialize = "song-title", to_string = "Song Title")]
    Title,
    #[strum(serialize = "artist", to_string = "Artist")]
    Artist,
    #[strum(serialize = "album-title", to_string = "Album Title")]
    AlbumTitle,
    #[strum(serialize = "album-artist", to_string = "AlbumArtist")]
    AlbumArtist,
    #[strum(serialize = "release-date", to_string = "Release Date")]
    ReleaseDate,
    #[strum(serialize = "last-modified", to_string = "Last Modified")]
    LastModified,
    #[strum(serialize = "disc-track", to_string = "Disc & Track")]
    DiscTrack,
}

impl SortCriterion for SongSortCriterion {}

impl SongSortCriterion {
    fn compare(
        self,
        a: &Song,
        b: &Song,
        ascending: bool,
        nulls_first: bool,
        case_sensitive: bool,
    ) -> Ordering {
        match self {
            Self::Title => g_cmp_str_options(
                Some(a.get_name()),
                Some(b.get_name()),
                nulls_first,
                ascending,
                case_sensitive,
            ),
            Self::Artist => g_cmp_str_vecs(
                a.get_artists().map(|s| s.as_str()).collect(),
                b.get_artists().map(|s| s.as_str()).collect(),
                nulls_first,
                ascending,
                case_sensitive,
            ),
            Self::AlbumTitle => g_cmp_str_options(
                a.get_album_title(),
                b.get_album_title(),
                nulls_first,
                ascending,
                case_sensitive,
            ),
            Self::AlbumArtist => g_cmp_str_vecs(
                a.get_albumartists().map(|s| s.as_str()).collect(),
                b.get_albumartists().map(|s| s.as_str()).collect(),
                nulls_first,
                ascending,
                case_sensitive,
            ),
            Self::ReleaseDate => g_cmp_options(
                a.get_release_date().as_ref(),
                b.get_release_date().as_ref(),
                nulls_first,
                ascending,
            ),
            Self::LastModified => g_cmp_str_options(
                a.get_last_modified(),
                b.get_last_modified(),
                nulls_first,
                ascending,
                true, // MPD returns fixed casing, but by setting this to true we avoid having to_lowercase()
            ),
            Self::DiscTrack => {
                // Combo option: disc first then track. No point sorting by one without the other.
                let disc_res = g_cmp_options(
                    Some(&a.get_disc()),
                    Some(&b.get_disc()),
                    nulls_first,
                    ascending,
                );
                if disc_res != Ordering::Equal {
                    disc_res
                } else {
                    g_cmp_options(
                        Some(&a.get_track()),
                        Some(&b.get_track()),
                        nulls_first,
                        ascending,
                    )
                }
            }
        }
    }
}

pub fn song_sort_tiers(state: &gio::Settings) -> [Option<SortTier<SongSortCriterion>>; 3] {
    SONG_SORT_SETTINGS.map(|(sort_by, sort_direction)| {
        let criterion = state.string(sort_by).parse().ok()?;
        let direction = if state.enum_(sort_direction) > 0 {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };
        Some(SortTier {
            criterion,
            direction,
        })
    })
}

pub fn compare_songs(
    a: &Song,
    b: &Song,
    tiers: &[Option<SortTier<SongSortCriterion>>; 3],
    nulls_first: bool,
    case_sensitive: bool,
) -> Ordering {
    tiers
        .iter()
        .flatten()
        .map(|tier| {
            let ascending = tier.direction == SortDirection::Ascending;
            tier.criterion
                .compare(a, b, ascending, nulls_first, case_sensitive)
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}
