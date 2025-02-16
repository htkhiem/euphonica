use super::{lastfm::LastfmWrapper, models, musicbrainz::MusicBrainzWrapper, MetadataProvider};

/// A meta-MetadataProvider that works by daisy-chaining actual MetadataProviders.
/// Think composite pattern.
/// The key document might be updated as it passes through providers, for example
/// to add a MusicBrainz ID. This should help downstream providers locate metadata
/// more accurately.
pub struct MetadataChain {
    pub providers: Vec<Box<dyn MetadataProvider>>,
}

impl MetadataProvider for MetadataChain {
    /// The priority argument exists only for compatibility and is always ignored.
    fn new(_prio: u32) -> Self
    where
        Self: Sized,
    {
        Self {
            providers: Vec::new(),
        }
    }

    fn key(&self) -> &'static str {
        "chain"
    }

    /// Will always return 0, since MetadataChain is not meant to be nested in another chain.
    fn priority(&self) -> u32 {
        0
    }

    /// Does nothing, since MetadataChain is not meant to be nested in another chain.
    fn set_priority(&self, _prio: u32) {}

    fn get_album_meta(
        self: &Self,
        key: bson::Document,
        mut existing: Option<models::AlbumMeta>,
    ) -> Option<models::AlbumMeta> {
        let mut current_key: bson::Document = key.clone();
        for provider in self.providers.iter() {
            existing = provider.get_album_meta(current_key.clone(), existing);
            // Update key document with new fields
            if let Some(meta) = &existing {
                if let Some(id) = &meta.mbid {
                    if !current_key.contains_key("mbid") {
                        current_key.insert("mbid", id.to_owned());
                    }
                }
            }
        }
        existing
    }

    fn get_artist_meta(
        self: &Self,
        key: bson::Document,
        mut existing: Option<models::ArtistMeta>,
    ) -> Option<models::ArtistMeta> {
        let mut current_key: bson::Document = key.clone();
        for provider in self.providers.iter() {
            existing = provider.get_artist_meta(current_key.clone(), existing);
            // Update key document with new fields
            if let Some(meta) = &existing {
                if let Some(id) = &meta.mbid {
                    if !current_key.contains_key("mbid") {
                        current_key.insert("mbid", id.to_owned());
                    }
                }
            }
        }
        existing
    }
}

/// Convenience method to construct a metadata provider instance by key with the given priority.
/// When implementing a new provider, you must manually add it to this function too.
pub fn get_provider_with_priority(key: &str, prio: u32) -> Box<dyn MetadataProvider> {
    match key {
        "musicbrainz" => Box::new(MusicBrainzWrapper::new(prio)),
        "lastfm" => Box::new(LastfmWrapper::new(prio)),
        _ => unimplemented!(),
    }
}
