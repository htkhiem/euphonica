use std::time::SystemTime;

use gtk::prelude::*;
extern crate bson;

use musicbrainz_rs::{
    client::MusicBrainzClient,
    entity::{artist::*, release::*},
    error::Error as MBError,
    prelude::*,
};

use crate::{
    common::{AlbumInfo, ArtistInfo},
    config::APPLICATION_USER_AGENT,
    meta_providers::models::ImageMeta,
    utils::{meta_provider_settings, settings_manager},
};

use super::{
    super::{MetadataProvider, models, prelude::*},
    PROVIDER_KEY,
};

/// v0.99: updated to musicbrainz_rs v0.12 which has cover art URL handling & retrying built-in.
/// Since we're already handling retries here, we never return MetadataError::Reqwest, which
/// means MetadataChain will never retry this provider.
pub struct MusicBrainzWrapper {
    last_request_time: SystemTime,
    client: MusicBrainzClient,
}

fn pack_error<T>(existing: T, mbe: MBError) -> MetadataError<T> {
    match mbe {
        // Included here, but shouldn't be reachable
        MBError::ReqwestError(e) => MetadataError::Reqwest(existing, e),
        MBError::NotFound(_) => MetadataError::NotFound(existing),
        MBError::MaxRetriesExceeded => MetadataError::RateLimit(existing),
        _ => MetadataError::Parse(existing),
    }
}

impl MetadataProvider for MusicBrainzWrapper {
    fn new() -> Self {
        let settings = settings_manager().child("metaprovider");
        let mut client = MusicBrainzClient::default();
        client.max_retries = settings.uint("n-tries").max(1);
        let _ = client.set_user_agent(APPLICATION_USER_AGENT);
        Self {
            last_request_time: SystemTime::now(),
            client,
        }
    }

    /// Schedule getting album metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> MetadataResult<Option<models::AlbumMeta>> {
        sleep_between_requests(self.last_request_time);
        if !meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            return Ok(existing);
        }

        let mut new_result: models::AlbumMeta;

        if let Some(mbid) = key.mbid.as_ref() {
            println!("[MusicBrainz] Fetching release by MBID: {}", &mbid);
            self.last_request_time = SystemTime::now();
            match Release::fetch()
                .id(mbid)
                .with_artist_credits()
                .execute_with_client(&self.client)
            {
                Ok(release) => {
                    new_result = release.into();
                }
                Err(e) => {
                    println!("[MusicBrainz] Could not fetch album metadata: {:?}", &e);
                    return Err(pack_error(existing, e));
                }
            }
        }
        // Else there must be an artist tag before we can search reliably
        else if let (title, Some(artist)) = (&key.title, key.get_artist_tag()) {
            // Ensure linkages match those on MusicBrainz.
            // TODO: use multiple ORed artist clauses instead.
            println!("[MusicBrainz] Searching release with title = {title} and artist = {artist}");
            self.last_request_time = SystemTime::now();
            match Release::search(
                ReleaseSearchQuery::query_builder()
                    .release(title)
                    .and()
                    .artist(artist)
                    .build(),
            )
            .with_artist_credits()
            .execute_with_client(&self.client)
            {
                Ok(found) => {
                    if let Some(first) = found.entities.into_iter().nth(0) {
                        new_result = first.into();
                    } else {
                        println!("[MusicBrainz] No release found for artist & album title");
                        return Err(MetadataError::NotFound(existing));
                    }
                }
                Err(e) => {
                    println!("[MusicBrainz] Could not fetch album metadata: {:?}", &e);
                    return Err(pack_error(existing, e));
                }
            }
        } else {
            println!("[MusicBrainz] Either MBID or BOTH album name & artist must be provided");
            return Err(MetadataError::InsufficientKey(existing));
        }

        if let Some(old) = existing {
            new_result = old.merge(new_result);
        }

        if meta_provider_settings(PROVIDER_KEY).boolean("download-album-art") {
            // Newer musicbrainz_rs versions are also aware of the cover art link situation,
            // but we'd just like to get a direct link for our existing fetch machinery.
            new_result.image.push(ImageMeta {
                // in reality, we don't really know the quality. However, its likely a highres version.
                size: models::ImageSize::Mega,
                url: format!(
                    "{}/release/{}/front",
                    &self.client.coverart_archive_url,
                    new_result.mbid.clone().unwrap()
                ),
            });
        }
        return Ok(Some(new_result));
    }

    /// Schedule getting artist metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    /// Since images have varying aspect ratios, we will use a simple entropy-based cropping
    /// algorithm.
    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        existing: std::option::Option<models::ArtistMeta>,
    ) -> MetadataResult<Option<models::ArtistMeta>> {
        sleep_between_requests(self.last_request_time);
        if meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            if let Some(mbid) = key.mbid.as_ref() {
                println!("[MusicBrainz] Fetching artist by MBID: {mbid}");
                self.last_request_time = SystemTime::now();
                match Artist::fetch()
                    .id(mbid)
                    .with_url_relations()
                    .execute_with_client(&self.client)
                {
                    Ok(artist) => {
                        let new: models::ArtistMeta = artist.into();
                        println!("{:?}", &new);
                        // If there is existing data, merge new data to it
                        return Ok(Some(if let Some(old) = existing {
                            old.merge(new)
                        } else {
                            new
                        }));
                    }
                    Err(e) => {
                        println!("[MusicBrainz] Could not fetch artist metadata: {:?}", &e);
                        return Err(pack_error(existing, e));
                    }
                }
            }
            // If MBID is not available we'll need to search solely by artist name.
            // TODO: add some more clues, such as a song or album name.
            else {
                let name = &key.name;
                println!("[MusicBrainz] Fetching artist with name = {}", &name);
                self.last_request_time = SystemTime::now();
                match Artist::search(ArtistSearchQuery::query_builder().artist(name).build())
                    .with_url_relations()
                    .execute_with_client(&self.client)
                {
                    Ok(found) => {
                        if let Some(first) = found.entities.into_iter().nth(0) {
                            let new: models::ArtistMeta = first.into();
                            println!("{:?}", &new);
                            // If there is existing data, merge new data to it
                            return Ok(Some(if let Some(old) = existing {
                                old.merge(new)
                            } else {
                                new
                            }));
                        } else {
                            println!("[MusicBrainz] No artist metadata found for {name}");
                            return Err(MetadataError::NotFound(existing));
                        }
                    }
                    Err(e) => {
                        println!("[MusicBrainz] Could not fetch artist metadata: {:?}", &e);
                        return Err(pack_error(existing, e));
                    }
                }
            }
        } else {
            Ok(existing)
        }
    }

    /// MusicBrainz does not provide lyrics.
    fn get_lyrics(
        &mut self,
        _key: &crate::common::SongInfo,
    ) -> MetadataResult<Option<models::Lyrics>> {
        Ok(None)
    }
}
