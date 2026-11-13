use gtk::Ordering;

use crate::{
    common::Album,
    utils::{g_cmp_options, g_cmp_str_options},
};

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

    /// Sort by `self` in `direction`, then by two ascending tiebreakers.
    pub fn default_keys(self, direction: SortDirection) -> [AlbumSortKey; 3] {
        use AlbumSortOrder::*;
        let (second, third) = match self {
            AlbumArtist => (ReleaseDate, AlbumTitle),
            AlbumTitle => (AlbumArtist, ReleaseDate),
            ReleaseDate => (AlbumArtist, AlbumTitle),
            Rating => (AlbumArtist, ReleaseDate),
        };
        [
            AlbumSortKey {
                order: self,
                direction,
            },
            AlbumSortKey {
                order: second,
                direction: SortDirection::Ascending,
            },
            AlbumSortKey {
                order: third,
                direction: SortDirection::Ascending,
            },
        ]
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

pub fn compare_albums(
    a: &Album,
    b: &Album,
    keys: &[AlbumSortKey; 3],
    nulls_first: bool,
    case_sensitive: bool,
) -> Ordering {
    keys.iter()
        .map(|key| {
            let ascending = key.direction == SortDirection::Ascending;
            key.order
                .compare(a, b, ascending, nulls_first, case_sensitive)
        })
        .find(|ordering| *ordering != Ordering::Equal)
        .unwrap_or(Ordering::Equal)
}
