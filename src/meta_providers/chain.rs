use gio::prelude::SettingsExt;

use crate::{
    common::{AlbumInfo, ArtistInfo, SongInfo},
    utils::settings_manager,
};

use super::{
    lastfm::LastfmWrapper, lrclib::LrcLibWrapper, models, musicbrainz::MusicBrainzWrapper,
    MetadataProvider,
};

/// A meta-MetadataProvider that works by daisy-chaining actual MetadataProviders.
/// Think composite pattern. For convenience, it's also where we implement our
/// retrying logic. Each provider in the chain will be retried separately.
/// Since retrying is already handled by this provider, it will always return false
/// retry flags. Note that each provider still needs to implement its own cooldown.
/// The key document might be updated as it passes through providers, for example
/// to add a MusicBrainz ID. This should help downstream providers locate metadata
/// more accurately.
pub struct MetadataChain {
    pub providers: Vec<Box<dyn MetadataProvider>>,
}

impl MetadataProvider for MetadataChain {
    /// The priority argument exists only for compatibility and is always ignored.
    fn new() -> Self
    where
        Self: Sized,
    {
        Self {
            providers: Vec::new(),
        }
    }

    fn get_album_meta(
        &mut self,
        key: &mut AlbumInfo,
        mut existing: Option<models::AlbumMeta>,
    ) -> (Option<models::AlbumMeta>, bool) {
        let settings = settings_manager().child("metaprovider");
        let max_tries: u32 = settings.uint("n-tries").max(1);
        for provider in self.providers.iter_mut() {
            let mut tries_left = max_tries;
            'retry: loop {
                let should_retry: bool;
                (existing, should_retry) = provider.get_album_meta(key, existing);
                if should_retry && tries_left > 0 {
                    tries_left -= 1;
                    println!("[chain] Current provider reported a retryable error.");
                } else {
                    break 'retry;
                }
            }

            // Update key document with new fields
            if let Some(meta) = &existing {
                if let (Some(id), None) = (&meta.mbid, &key.mbid) {
                    key.mbid = Some(id.to_owned());
                }
            }
        }
        (existing, false)
    }

    fn get_artist_meta(
        &mut self,
        key: &mut ArtistInfo,
        mut existing: Option<models::ArtistMeta>,
    ) -> (Option<models::ArtistMeta>, bool) {
        let settings = settings_manager().child("metaprovider");
        let max_tries: u32 = settings.uint("n-tries").max(1);
        for provider in self.providers.iter_mut() {
            let mut tries_left = max_tries;
            'retry: loop {
                let should_retry: bool;
                (existing, should_retry) = provider.get_artist_meta(key, existing);
                if should_retry && tries_left > 0 {
                    tries_left -= 1;
                    println!("[chain] Current provider reported a retryable error.");
                } else {
                    break 'retry;
                }
            }

            // Update key document with new fields
            if let Some(meta) = &existing {
                if let (Some(id), None) = (&meta.mbid, &key.mbid) {
                    key.mbid = Some(id.to_owned());
                }
            }
        }
        (existing, false)
    }

    fn get_lyrics(&mut self, key: &SongInfo) -> (Option<models::Lyrics>, bool) {
        let settings = settings_manager().child("metaprovider");
        let max_tries: u32 = settings.uint("n-tries").max(1);
        for provider in self.providers.iter_mut() {
            let mut tries_left = max_tries;
            'retry: loop {
                let (lyrics, should_retry) = provider.get_lyrics(key);
                if let Some(lyrics) = lyrics {
                    return (Some(lyrics), false);
                } else if should_retry && tries_left > 0 {
                    tries_left -= 1;
                    println!("[chain] Current provider reported a retryable error.");
                } else {
                    break 'retry;
                }
            }
        }
        (None, false)
    }
}

/// Convenience method to construct a metadata provider instance by key with the given priority.
/// When implementing a new provider, you must manually add it to this function too.
pub fn get_provider(key: &str) -> Box<dyn MetadataProvider> {
    match key {
        "musicbrainz" => Box::new(MusicBrainzWrapper::new()),
        "lastfm" => Box::new(LastfmWrapper::new()),
        "lrclib" => Box::new(LrcLibWrapper::new()),
        _ => unimplemented!(),
    }
}
