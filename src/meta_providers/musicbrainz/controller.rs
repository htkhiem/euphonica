use gtk::prelude::*;
extern crate bson;

use std::sync::RwLock;

use musicbrainz_rs::{
    entity::{artist::*, release::*},
    prelude::*,
};

use crate::{common::{AlbumInfo, ArtistInfo}, utils::meta_provider_settings};

use super::{
    super::{models, prelude::*, MetadataProvider},
    PROVIDER_KEY,
};

pub struct MusicBrainzWrapper {
    priority: RwLock<u32>,
}

impl MetadataProvider for MusicBrainzWrapper {
    fn new(prio: u32) -> Self {
        Self {
            priority: RwLock::new(prio),
        }
    }

    fn key(&self) -> &'static str {
        PROVIDER_KEY
    }

    fn priority(&self) -> u32 {
        *self.priority.read().expect("Poisoned RwLock")
    }

    fn set_priority(&self, prio: u32) {
        let mut this_prio = self.priority.write().expect("Poisoned RwLock");
        *this_prio = prio;
    }

    /// Schedule getting album metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    fn get_album_meta(
        &self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            if let Some(mbid) = key.mbid.as_ref() {
                println!("[MusicBrainz] Fetching release by MBID: {}", &mbid);
                let res = Release::fetch()
                    .id(mbid)
                    .with_artist_credits()
                    .execute();
                if let Ok(release) = res {
                    let new: models::AlbumMeta = release.into();
                    println!("{:?}", &new);
                    // If there is existing data, merge new data to it
                    if let Some(old) = existing {
                        return Some(old.merge(new));
                    }
                    return Some(new);
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch album metadata: {:?}",
                        res.err()
                    );
                    return existing;
                }
            }
            // Else there must be an artist tag before we can search reliably
            else if let (title, Some(artist)) = (&key.title, key.get_artist_tag()) {
                // Ensure linkages match those on MusicBrainz.
                // TODO: use multiple ORed artist clauses instead.
                println!(
                    "[MusicBrainz] Searching release with title = {title} and artist = {artist}"
                );
                let res = Release::search(
                    ReleaseSearchQuery::query_builder()
                        .release(title)
                        .artist(artist)
                        .build(),
                )
                .with_artist_credits()
                .execute();

                if let Ok(found) = res {
                    if let Some(first) = found.entities.into_iter().nth(0) {
                        let new: models::AlbumMeta = first.into();
                        // If there is existing data, merge new data to it
                        if let Some(old) = existing {
                            return Some(old.merge(new));
                        }
                        return Some(new);
                    } else {
                        println!("[MusicBrainz] No release found for artist & album title");
                        return existing;
                    }
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch album metadata: {:?}",
                        res.err()
                    );
                    return existing;
                }
            } else {
                println!("[MusicBrainz] Either MBID or BOTH album name & artist must be provided");
                return existing;
            }
        } else {
            return existing;
        }
    }

    /// Schedule getting artist metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    /// Since images have varying aspect ratios, we will use a simple entropy-based cropping
    /// algorithm.
    fn get_artist_meta(
        &self,
        key: &mut ArtistInfo,
        existing: std::option::Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta> {
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            if let Some(mbid) = key.mbid.as_ref() {
                println!("[MusicBrainz] Fetching artist by MBID: {}", mbid);
                let res = Artist::fetch()
                    .id(mbid)
                    .with_url_relations()
                    .execute();
                if let Ok(artist) = res {
                    let new: models::ArtistMeta = artist.into();
                    println!("{:?}", &new);
                    // If there is existing data, merge new data to it
                    if let Some(old) = existing {
                        return Some(old.merge(new));
                    }
                    return Some(new);
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch artist metadata: {:?}",
                        res.err()
                    );
                    return existing;
                }
            }
            // If MBID is not available we'll need to search solely by artist name.
            // TODO: add some more clues, such as a song or album name.
            else {
                let name = &key.name;
                println!("[MusicBrainz] Fetching artist with name = {}", &name);
                let res = Artist::search(
                    ArtistSearchQuery::query_builder()
                        .artist(name)
                        .build(),
                )
                .with_url_relations()
                .execute();

                if let Ok(found) = res {
                    if let Some(first) = found.entities.into_iter().nth(0) {
                        let new: models::ArtistMeta = first.into();
                        println!("{:?}", &new);
                        // If there is existing data, merge new data to it
                        if let Some(old) = existing {
                            return Some(old.merge(new));
                        }
                        return Some(new);
                    } else {
                        println!("[MusicBrainz] No artist metadata found for {name}");
                        return existing;
                    }
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch artist metadata: {:?}",
                        res.err()
                    );
                    return existing;
                }
            }
        } else {
            return existing;
        }
    }

    /// MusicBrainz does not provide lyrics.
    fn get_lyrics(
        &self,
        key: &crate::common::SongInfo
    ) -> Option<models::Lyrics> {
        None
    }
}
