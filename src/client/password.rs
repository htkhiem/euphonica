use gtk::{
    gio::{self, Cancellable},
    glib::Error as GError
};
use libsecret::*;
use std::collections::HashMap;

use crate::config::APPLICATION_ID;

pub fn get_mpd_password_schema() -> Schema {
    let mut attributes = HashMap::new();
    attributes.insert("type", SchemaAttributeType::String);

    Schema::new(APPLICATION_ID, SchemaFlags::NONE, attributes)
}

pub fn get_mpd_password() -> Result<Option<String>, GError> {
    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    match libsecret::password_lookup_sync(Some(&schema), attributes, Cancellable::NONE).map(|gs| gs.map(|gs| gs.to_string())) {
        Ok(res) => Ok(res),
        Err(ge) => Err(dbg!(ge))
    }
}

pub async fn get_mpd_password_async() -> Result<Option<String>, GError> {
    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    match libsecret::password_lookup_future(Some(&schema), attributes).await {
        Ok(pw) => Ok(pw.map(|gs| gs.as_str().to_owned())),
        Err(ge) => {
            if ge.message().eq("The name is not activatable") {
                Ok(None)
            } else {
                Err(dbg!(ge))
            }
        }
    }
}

pub async fn set_mpd_password(maybe_password: Option<&str>) -> Result<(), GError> {
    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    if let Some(password) = maybe_password {
        match libsecret::password_store_future(
            Some(&schema),
            attributes,
            None,
            "Euphonica MPD password",
            password,
        )
        .await {
            Ok(()) => Ok(()),
            Err(ge) => {
                Err(dbg!(ge))
            }
        }
    } else {
        match libsecret::password_clear_future(Some(&schema), attributes)
            .await {
                Ok(()) => Ok(()),
                Err(ge) => Err(dbg!(ge))
            }
    }
}
