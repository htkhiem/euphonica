use std::time::SystemTime;

use gtk::prelude::*;
extern crate bson;

use musicbrainz_rs::{
    entity::{artist::*, release::*},
    prelude::*,
};

use crate::{
    common::{AlbumInfo, ArtistInfo},
    utils::meta_provider_settings,
};

use super::{
    super::{MetadataProvider, models, prelude::*},
    PROVIDER_KEY,
};

pub struct MusicBrainzWrapper {
    last_request_time: SystemTime
}

impl MetadataProvider for MusicBrainzWrapper {
    fn new() -> Self {
        Self {
            last_request_time: SystemTime::now()
        }
    }

    /// Schedule getting album metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta> {
        sleep_between_requests(self.last_request_time);
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            if let Some(mbid) = key.mbid.as_ref() {
                println!("[MusicBrainz] Fetching release by MBID: {}", &mbid);
                let res = Release::fetch().id(mbid).with_artist_credits().execute();
                self.last_request_time = SystemTime::now();
                if let Ok(release) = res {
                    let new: models::AlbumMeta = release.into();
                    println!("{:?}", &new);
                    // If there is existing data, merge new data to it
                    if let Some(old) = existing {
                        return Some(old.merge(new));
                    }
                    Some(new)
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch album metadata: {:?}",
                        res.err()
                    );
                    existing
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
                self.last_request_time = SystemTime::now();
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
            existing
        }
    }

    /// Schedule getting artist metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    /// Since images have varying aspect ratios, we will use a simple entropy-based cropping
    /// algorithm.
    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        existing: std::option::Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta> {
        sleep_between_requests(self.last_request_time);
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            if let Some(mbid) = key.mbid.as_ref() {
                println!("[MusicBrainz] Fetching artist by MBID: {mbid}");
                let res = Artist::fetch().id(mbid).with_url_relations().execute();
                self.last_request_time = SystemTime::now();
                if let Ok(artist) = res {
                    let new: models::ArtistMeta = artist.into();
                    println!("{:?}", &new);
                    // If there is existing data, merge new data to it
                    if let Some(old) = existing {
                        return Some(old.merge(new));
                    }
                    Some(new)
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch artist metadata: {:?}",
                        res.err()
                    );
                    existing
                }
            }
            // If MBID is not available we'll need to search solely by artist name.
            // TODO: add some more clues, such as a song or album name.
            else {
                let name = &key.name;
                println!("[MusicBrainz] Fetching artist with name = {}", &name);
                let res = Artist::search(ArtistSearchQuery::query_builder().artist(name).build())
                    .with_url_relations()
                    .execute();
                self.last_request_time = SystemTime::now();
                if let Ok(found) = res {
                    if let Some(first) = found.entities.into_iter().nth(0) {
                        let new: models::ArtistMeta = first.into();
                        println!("{:?}", &new);
                        // If there is existing data, merge new data to it
                        if let Some(old) = existing {
                            return Some(old.merge(new));
                        }
                        Some(new)
                    } else {
                        println!("[MusicBrainz] No artist metadata found for {name}");
                        existing
                    }
                } else {
                    println!(
                        "[MusicBrainz] Could not fetch artist metadata: {:?}",
                        res.err()
                    );
                    existing
                }
            }
        } else {
            existing
        }
    }

    /// MusicBrainz does not provide lyrics.
    fn get_lyrics(&mut self, _key: &crate::common::SongInfo) -> Option<models::Lyrics> {
        None
    }
}
