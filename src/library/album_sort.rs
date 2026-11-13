use gtk::{Ordering, gio, gio::prelude::SettingsExt};

use crate::{
    common::Album,
    utils::{g_cmp_options, g_cmp_str_options},
};

pub const ALBUM_SORT_SETTINGS: [(&str, &str); 3] = [
    ("sort-by", "sort-direction"),
    ("sort-by-2", "sort-direction-2"),
    ("sort-by-3", "sort-direction-3"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlbumSortOrder {
    AlbumArtist,
    AlbumTitle,
    ReleaseDate,
    Rating,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDirection {
    Ascending,
    Descending,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AlbumSortKey {
    pub order: AlbumSortOrder,
    pub direction: SortDirection,
}

impl AlbumSortOrder {
    /// Map a value of the `io.github.htkhiem.Euphonica.sortby` enum in the gschema.
    pub fn from_sortby(value: i32) -> Option<Self> {
        match value {
            3 => Some(Self::AlbumTitle),
            4 => Some(Self::AlbumArtist),
            5 => Some(Self::ReleaseDate),
            8 => Some(Self::Rating),
            _ => None,
        }
    }

    fn compare(
        self,
        a: &Album,
        b: &Album,
        ascending: bool,
        nulls_first: bool,
        case_sensitive: bool,
    ) -> Ordering {
        match self {
            Self::AlbumArtist => g_cmp_str_options(
                a.get_sortable_artist_tag(),
                b.get_sortable_artist_tag(),
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

/// A tier whose `sort-by` value doesn't apply to albums is `None`.
pub fn album_sort_keys(state: &gio::Settings) -> [Option<AlbumSortKey>; 3] {
    ALBUM_SORT_SETTINGS.map(|(sort_by, sort_direction)| {
        let order = AlbumSortOrder::from_sortby(state.enum_(sort_by))?;
        let direction = if state.enum_(sort_direction) > 0 {
            SortDirection::Ascending
        } else {
            SortDirection::Descending
        };
        Some(AlbumSortKey { order, direction })
    })
}

pub fn compare_albums(
    a: &Album,
    b: &Album,
    keys: &[Option<AlbumSortKey>; 3],
    nulls_first: bool,
    case_sensitive: bool,
) -> Ordering {
    keys.iter()
        .flatten()
        .map(|key| {
            let ascending = key.direction == SortDirection::Ascending;
            key.order
                .compare(a, b, ascending, nulls_first, case_sensitive)
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}
