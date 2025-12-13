use libsecret::*;
use std::collections::HashMap;

use crate::config::APPLICATION_ID;

pub fn get_mpd_password_schema() -> Schema {
    let mut attributes = HashMap::new();
    attributes.insert("type", SchemaAttributeType::String);

    Schema::new(APPLICATION_ID, SchemaFlags::NONE, attributes)
}

pub async fn get_mpd_password() -> Result<Option<String>, String> {
    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    libsecret::password_lookup_future(Some(&schema), attributes)
        .await
        .map(|op| op.map(|gs| gs.as_str().to_owned()))
        .map_err(|ge| format!("{ge:?}"))
}

pub async fn set_mpd_password(maybe_password: Option<&str>) -> Result<(), String> {
    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    if let Some(password) = maybe_password {
        libsecret::password_store_future(
            Some(&schema),
            attributes,
            None,
            "Euphonica MPD password",
            password,
        )
        .await
        .map_err(|ge| format!("{ge:?}"))
    } else {
        libsecret::password_clear_future(Some(&schema), attributes)
            .await
            .map_err(|ge| format!("{ge:?}"))
    }
}
