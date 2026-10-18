use asyncified::Asyncified;
use gtk::{gio::prelude::*, glib};

use crate::{
    common::{AlbumInfo, ArtistInfo, SongInfo},
    meta_providers::the_audio_db::TheAudioDbWrapper,
    utils::settings_manager,
    window::EuphonicaWindow,
};

use super::{
    MetadataProvider, lastfm::LastfmWrapper, lrclib::LrcLibWrapper, models,
    musicbrainz::MusicBrainzWrapper,
};

/// Utility struct aiding in daisy-chaining actual MetadataProviders.
/// For convenience, it's also where we implement our retrying logic. Each provider
/// in the chain will be retried separately.
/// In this struct, each provider is stored in an Asyncified container to effectively
/// "queue" async calls from the rest of the app.
/// Compared to the old MetadataChain, this struct is async and facilitates error
/// reporting to the UI.
/// Some providers, like musicbrainz, has its own retry logic. In that case, the
/// error they return should never be retryable to prevent this struct from further
/// looping. Note that each provider still needs to implement its own cooldown.
/// The key document might be updated as it passes through providers, for example
/// to add a MusicBrainz ID. This should help downstream providers locate metadata
/// more accurately.
pub struct MetadataChain {
    providers: Vec<(&'static str, Asyncified<Box<dyn MetadataProvider>>)>,
    max_tries: u32,
}

impl MetadataChain {
    /// The priority argument exists only for compatibility and is always ignored.
    pub async fn new() -> Self
    where
        Self: Sized,
    {
        let mut providers = Vec::new();
        let settings = settings_manager().child("metaprovider");
        for key in effective_provider_order() {
            let (name, provider) = get_provider(&key);
            providers.push((
                name,
                Asyncified::builder()
                    .channel_size(usize::MAX)
                    .build_ok(move || provider)
                    .await,
            ));
        }

        Self {
            providers,
            max_tries: settings.uint("n-tries").max(1),
        }
    }

    pub async fn get_album_meta(
        &self,
        key: AlbumInfo,
        mut existing: Option<models::AlbumMeta>,
        window: Option<&EuphonicaWindow>, // if given, will send error toasts
    ) -> Option<models::AlbumMeta> {
        // Clone once here then pass back n forth.
        for (name, container) in self.providers.iter() {
            let mut cloned_key = key.clone();
            let mut tries_left = self.max_tries;
            'retry: loop {
                let (modified_key, res) = container
                    .call(move |provider| {
                        let res = provider.get_album_meta(&mut cloned_key, existing);
                        (cloned_key, res)
                    })
                    .await;
                cloned_key = modified_key;
                match res {
                    Ok(res) => {
                        existing = res;
                        break 'retry;
                    }
                    Err(e) => {
                        if e.can_retry() && tries_left > 0 {
                            tries_left -= 1;
                            println!(
                                "[chain] Retrying current provider (retries left: {})...",
                                tries_left
                            );
                            existing = e.metadata();
                        } else {
                            if let Some(window) = window {
                                window
                                    .send_simple_toast(&format!("{}: {}", &name, &e.message()), 3);
                            }
                            existing = e.metadata();
                            break 'retry;
                        }
                    }
                }
            }
        }
        existing
    }

    pub async fn get_artist_meta(
        &self,
        key: ArtistInfo,
        mut existing: Option<models::ArtistMeta>,
        window: Option<&EuphonicaWindow>,
    ) -> Option<models::ArtistMeta> {
        for (name, container) in self.providers.iter() {
            let mut tries_left = self.max_tries;
            let mut cloned_key = key.clone();
            'retry: loop {
                let (modified_key, res) = container
                    .call(move |provider| {
                        let res = provider.get_artist_meta(&mut cloned_key, existing);
                        (cloned_key, res)
                    })
                    .await;
                cloned_key = modified_key;
                match res {
                    Ok(res) => {
                        existing = res;
                        break 'retry;
                    }
                    Err(e) => {
                        if e.can_retry() && tries_left > 0 {
                            tries_left -= 1;
                            println!(
                                "[chain] Retrying current provider (retries left: {})...",
                                tries_left
                            );
                            existing = e.metadata();
                        } else {
                            if let Some(window) = window {
                                window
                                    .send_simple_toast(&format!("{}: {}", &name, &e.message()), 3);
                            }
                            existing = e.metadata();
                            break 'retry;
                        }
                    }
                }
            }
        }
        existing
    }

    pub async fn get_lyrics(
        &self,
        key: SongInfo,
        window: Option<&EuphonicaWindow>,
    ) -> Option<models::Lyrics> {
        for (name, container) in self.providers.iter() {
            let tries_left = self.max_tries;
            let cloned_key = key.clone();
            match container
                .call(move |provider| {
                    let mut tries_left = tries_left;
                    loop {
                        match provider.get_lyrics(&cloned_key) {
                            Ok(res) => {
                                return Ok(res);
                            }
                            Err(e) => {
                                if e.can_retry() && tries_left > 0 {
                                    tries_left -= 1;
                                    println!(
                                        "[chain] Retrying current provider (retries left: {})...",
                                        tries_left
                                    );
                                } else {
                                    return Err(e);
                                }
                            }
                        };
                    }
                })
                .await
            {
                Ok(res) => {
                    if let Some(lyrics) = res {
                        return Some(lyrics);
                    }
                }
                Err(e) => {
                    if let Some(window) = window {
                        window.send_simple_toast(&format!("{}: {}", &name, &e.message()), 3);
                    }
                }
            }
        }
        None
    }
}

/// All known provider keys, matching the arms of `get_provider` below.
/// Upon implementing a new provider, append its key here.
pub const ALL_PROVIDER_KEYS: [&str; 4] = ["lastfm", "lrclib", "musicbrainz", "theaudiodb"];

/// Convenience method to construct a metadata provider instance by key with the given priority.
/// When implementing a new provider, you must manually add it to this function too.
/// Returns a display-friendly name (for error toasts) and the provider struct itself
pub fn get_provider(key: &str) -> (&'static str, Box<dyn MetadataProvider>) {
    match key {
        "musicbrainz" => ("MusicBrainz", Box::new(MusicBrainzWrapper::new())),
        "lastfm" => ("Last.fm", Box::new(LastfmWrapper::new())),
        "lrclib" => ("LRCLIB", Box::new(LrcLibWrapper::new())),
        "theaudiodb" => ("TheAudioDB", Box::new(TheAudioDbWrapper::new())),
        _ => unimplemented!(),
    }
}

/// The effective provider order for this run: the user's stored `order`, with
/// any known providers missing from it automatically appended at the end
/// in the same order they appear in `ALL_PROVIDER_KEYS`. This allows new
/// providers added in app updates to show up in existing installs' priority list.
pub fn effective_provider_order() -> Vec<String> {
    let settings = settings_manager().child("metaprovider");
    let mut res: Vec<String> = settings
        .strv("order")
        .into_iter()
        .map(|gs| gs.to_string())
        .collect();
    let mut new_providers: Vec<String> = ALL_PROVIDER_KEYS
        .iter()
        .filter(|k| !res.iter().any(|existing| existing.as_str() == **k))
        .map(|name| name.to_string())
        .collect();
    res.append(&mut new_providers);
    res
}
