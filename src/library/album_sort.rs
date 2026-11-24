use gtk::{Ordering, gio, gio::prelude::SettingsExt};
use strum_macros::{Display, EnumMessage, EnumString, VariantArray};

use crate::{
    common::Album,
    utils::{g_cmp_options, g_cmp_str_options, g_cmp_str_vecs},
};

pub const ALBUM_SORT_SETTINGS: [(&str, &str); 3] = [
    ("sort-by", "sort-direction"),
    ("sort-by-2", "sort-direction-2"),
    ("sort-by-3", "sort-direction-3"),
];

/// Variants are in the order of the sort-by dropdown items.
/// `serialize` is the nick in the gschema's `io.github.htkhiem.Euphonica.sortby` enum,
/// and `to_string` is the dropdown label.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Display, EnumMessage, EnumString, VariantArray)]
pub enum AlbumSortCriterion {
    #[strum(serialize = "album-title", to_string = "Album Title")]
    AlbumTitle,
    #[strum(serialize = "album-artist", to_string = "AlbumArtist")]
    AlbumArtist,
    #[strum(serialize = "release-date", to_string = "Release Date")]
    ReleaseDate,
    #[strum(serialize = "rating", to_string = "Rating")]
    Rating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlbumSortTier {
    pub criterion: AlbumSortCriterion,
    pub direction: SortDirection,
}

impl AlbumSortCriterion {
    fn compare(
        self,
        a: &Album,
        b: &Album,
        ascending: bool,
        nulls_first: bool,
        case_sensitive: bool,
    ) -> Ordering {
        match self {
            Self::AlbumArtist => g_cmp_str_vecs(
                a.get_albumartists().collect(),
                b.get_albumartists().collect(),
                nulls_first,
                ascending,
                case_sensitive,
            ),
            Self::AlbumTitle => g_cmp_str_options(
                Some(a.get_sortable_title()),
                Some(b.get_sortable_title()),
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
            Self::Rating => g_cmp_options(
                a.get_rating().as_ref(),
                b.get_rating().as_ref(),
                nulls_first,
                ascending,
            ),
        }
    }
}

pub fn album_sort_tiers(state: &gio::Settings) -> [Option<AlbumSortTier>; 3] {
    ALBUM_SORT_SETTINGS.map(|(sort_by, sort_direction)| {
        let criterion = state.string(sort_by).parse().ok()?;
        let direction = if state.enum_(sort_direction) > 0 {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };
        Some(AlbumSortTier {
            criterion,
            direction,
        })
    })
}

pub fn compare_albums(
    a: &Album,
    b: &Album,
    tiers: &[Option<AlbumSortTier>; 3],
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
