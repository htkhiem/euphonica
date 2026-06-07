use gtk::{
    gio::{self, Cancellable},
    glib::{self, variant::ToVariant},
};
use libsecret::*;
use std::collections::HashMap;

use crate::config::APPLICATION_ID;

pub fn secret_service_available() -> bool {
    let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, Cancellable::NONE) else {
        return false;
    };
    let reply_type = glib::VariantTy::new("(b)").unwrap();
    let Ok(reply) = bus.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&("org.freedesktop.secrets",).to_variant()),
        Some(reply_type),
        gio::DBusCallFlags::NO_AUTO_START,
        -1,
        Cancellable::NONE,
    ) else {
        return false;
    };

    reply
        .get::<(bool,)>()
        .is_some_and(|(available,)| available)
}

pub async fn secret_service_available_async() -> bool {
    let Ok(bus) = gio::bus_get_future(gio::BusType::Session).await else {
        return false;
    };
    let reply_type = glib::VariantTy::new("(b)").unwrap();
    let Ok(reply) = bus
        .call_future(
            Some("org.freedesktop.DBus"),
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "NameHasOwner",
            Some(&("org.freedesktop.secrets",).to_variant()),
            Some(reply_type),
            gio::DBusCallFlags::NO_AUTO_START,
            -1,
        )
        .await
    else {
        return false;
    };

    reply
        .get::<(bool,)>()
        .is_some_and(|(available,)| available)
}

pub fn get_mpd_password_schema() -> Schema {
    let mut attributes = HashMap::new();
    attributes.insert("type", SchemaAttributeType::String);

    Schema::new(APPLICATION_ID, SchemaFlags::NONE, attributes)
}

pub fn get_mpd_password() -> Result<Option<String>, String> {
    if !secret_service_available() {
        return Err(String::from("Default credential store is not available"));
    }

    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    match libsecret::password_lookup_sync(Some(&schema), attributes, Cancellable::NONE) {
        Ok(pw) => Ok(pw.map(|gs| gs.as_str().to_owned())),
        Err(ge) => {
            if ge.message().eq("The name is not activatable") {
                Ok(None)
            } else {
                Err(format!("{ge:?}"))
            }
        }
    }
}

pub async fn get_mpd_password_async() -> Result<Option<String>, String> {
    if !secret_service_available_async().await {
        return Err(String::from("Default credential store is not available"));
    }

    let schema = get_mpd_password_schema();
    let mut attributes = HashMap::new();
    attributes.insert("type", "mpd");

    match libsecret::password_lookup_future(Some(&schema), attributes).await {
        Ok(pw) => Ok(pw.map(|gs| gs.as_str().to_owned())),
        Err(ge) => {
            if ge.message().eq("The name is not activatable") {
                Ok(None)
            } else {
                Err(format!("{ge:?}"))
            }
        }
    }
}

pub async fn set_mpd_password(maybe_password: Option<&str>) -> Result<(), String> {
    if !secret_service_available_async().await {
        return Err(String::from("Default credential store is not available"));
    }

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
