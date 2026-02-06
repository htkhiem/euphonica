use gtk::prelude::*;
extern crate bson;
use reqwest::{
    blocking::{Client},
    header,
    StatusCode,
};

use musicbrainz_rs::{
    entity::{artist::*, release::*},
    prelude::*,
};

use crate::{common::{AlbumInfo, ArtistInfo}, config::APPLICATION_USER_AGENT, meta_providers::musicbrainz::models::{CoverArtImage, CoverArtResponse}, utils::meta_provider_settings};

use super::{
    super::{models, prelude::*, MetadataProvider},
    PROVIDER_KEY,
};

pub struct MusicBrainzWrapper {
    client: Client
}

impl MusicBrainzWrapper {
    // custom impl since the musicbrainz_rs one has weird https errors...
    fn fetch_album_cover(&self, id: String) -> Option<CoverArtResponse> {
        let mut base_api: String = "https://coverartarchive.org/release/".to_owned();
        base_api.push_str(&id);

        let resp = self
            .client
            .get(base_api)
            .header(header::USER_AGENT, APPLICATION_USER_AGENT)
            .header(header::ACCEPT, "application/json")
            .send();

        match resp {
            Ok(resp) => {
                match resp.status() {
                    StatusCode::BAD_REQUEST => {
                        println!("[MusicBrainz] Couldn't parse input as mbid");
                    }
                    StatusCode::NOT_FOUND => {
                        println!("[MusicBrainz] Couldn't find a release with this mbid");
                    }
                    StatusCode::SERVICE_UNAVAILABLE => {
                        // Why did this API use 503 instead of 429? Who knows/cares
                        // Anyways this SHOULD be dead code as of writing this, since no rate limits actually exist
                        println!("[MusicBrainz] Exceeded rate limit for cover art");
                    }
                    StatusCode::OK => {
                        // 307 -> 200
                        match resp.json::<CoverArtResponse>() {
                            Ok(parsed) => {
                                return Some(parsed);
                            }
                            Err(err) => {
                                println!("[MusicBrainz] couldn't deserialize cover art meta: {err}");
                            }
                        }
                    }
                    s => {
                        println!("[MusicBrainz] Cover art api returned an unknown status: {s}")
                    }
                }
            }
            Err(e) => {
                println!("[MusicBrainz] Couldn't fetch from cover art api: {e:?}");
            }
        }

        return None;
    }
}

impl MetadataProvider for MusicBrainzWrapper {
    fn new() -> Self {
        Self {
            client: Client::new(),
        }
    }

    /// Schedule getting album metadata from MusicBrainz.
    /// A signal will be emitted to notify the caller when the result arrives.
    fn get_album_meta(
        &self,
        key: &mut AlbumInfo,
        existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta> {
        if ! meta_provider_settings(PROVIDER_KEY).boolean("enabled") {
            return existing;
        }

        let mut new_result: models::AlbumMeta;

        if let Some(mbid) = key.mbid.as_ref() {
            println!("[MusicBrainz] Fetching release by MBID: {}", &mbid);
            let res = Release::fetch()
                .id(mbid)
                .with_artist_credits()
                .execute();
            if let Ok(release) = res {
                new_result = release.into();
            } else {
                println!(
                    "[MusicBrainz] Could not fetch album metadata: {:?}",
                    res.err()
                );
                return existing
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
                    .and()
                    .artist(artist)
                    .build(),
            )
            .with_artist_credits()
            .execute();

            if let Ok(found) = res {
                if let Some(first) = found.entities.into_iter().nth(0) {
                    new_result = first.into();
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

        if let Some(old) = existing {
            new_result = old.merge(new_result);
        }

        if ! meta_provider_settings(PROVIDER_KEY).boolean("download-album-art") {
            return Some(new_result);
        }

        println!("[MusicBrainz] Fetching cover art");

        let cover_resp = self.fetch_album_cover(new_result.mbid.clone().unwrap());
        if let Some(data) = cover_resp {
            if data.images.is_empty() {
                println!("[MusicBrainz] Empty cover art response");
                return Some(new_result);
            }

            let mut new_images: Vec<models::ImageMeta> = data.images.into_iter().map(
                CoverArtImage::into,
            ).collect();

            println!("[MusicBrainz] Got images: {:?}", new_images);

            new_result.image.append(&mut new_images);

            println!("[MusicBrainz] Got images: {:?}", new_result.image);
        } else {
            println!("[MusicBrainz] Could not fetch album cover");
        }

        return Some(new_result);
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
                println!("[MusicBrainz] Fetching artist by MBID: {mbid}");
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
    fn get_lyrics(
        &self,
        _key: &crate::common::SongInfo
    ) -> Option<models::Lyrics> {
        None
    }
}
