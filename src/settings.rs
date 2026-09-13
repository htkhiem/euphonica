use gtk::{gio, glib, prelude::*};
use once_cell::sync::Lazy;
use rustc_hash::FxHashMap;
use std::{cell::OnceCell, sync::RwLock};

use crate::config::APPLICATION_ID;

static VALUES: Lazy<RwLock<FxHashMap<String, glib::Variant>>> = Lazy::new(Default::default);
const DCONF_PROBE_TIMEOUT_MS: i32 = 3000;

#[derive(Debug)]
struct SettingsState {
    root: gio::Settings,
    _watched: Vec<gio::Settings>,
}

thread_local! {
    static SETTINGS: OnceCell<SettingsState> = const { OnceCell::new() };
}

fn fallback_reason(backend: &gio::SettingsBackend) -> Option<String> {
    match backend.type_().name() {
        "DConfSettingsBackend" => {
            let result =
                gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).and_then(|bus| {
                    bus.call_sync(
                        Some("org.freedesktop.DBus"),
                        "/org/freedesktop/DBus",
                        "org.freedesktop.DBus",
                        "StartServiceByName",
                        Some(&("ca.desrt.dconf", 0u32).to_variant()),
                        None,
                        gio::DBusCallFlags::NONE,
                        DCONF_PROBE_TIMEOUT_MS,
                        gio::Cancellable::NONE,
                    )
                });
            result.err().map(|error| error.to_string())
        }
        "GMemorySettingsBackend" | "GNullSettingsBackend"
            if std::env::var_os("GSETTINGS_BACKEND").is_none() =>
        {
            Some("dconf settings backend is unavailable".to_owned())
        }
        _ => None,
    }
}

fn watch_settings(settings: gio::Settings, path: String, watched: &mut Vec<gio::Settings>) {
    let changed_path = path.clone();
    settings.connect_changed(None, move |settings, key| {
        let value = settings.value(key);
        VALUES
            .write()
            .unwrap()
            .insert(format!("{changed_path}{key}"), value);
    });

    for key in settings.settings_schema().unwrap().list_keys() {
        let value = settings.value(&key);
        VALUES
            .write()
            .unwrap()
            .insert(format!("{path}{key}"), value);
    }
    for child in settings.list_children() {
        watch_settings(settings.child(&child), format!("{path}{child}/"), watched);
    }
    watched.push(settings);
}

pub fn init() {
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    let backend = gio::SettingsBackend::default();
    let root = if let Some(reason) = fallback_reason(&backend) {
        let path = glib::user_config_dir()
            .join("euphonica")
            .join("settings.ini");
        eprintln!(
            "dconf unavailable ({reason}); using settings file {}",
            path.display()
        );
        let backend = gio::keyfile_settings_backend_new(
            path.to_str().expect("Settings path must be UTF-8"),
            "/",
            None,
        );
        gio::Settings::with_backend(app_id, &backend)
    } else {
        gio::Settings::with_backend(app_id, &backend)
    };

    let mut watched = Vec::new();
    watch_settings(root.clone(), String::new(), &mut watched);
    SETTINGS.with(|settings| {
        settings
            .set(SettingsState {
                root,
                _watched: watched,
            })
            .expect("Settings already initialized");
    });
}

pub fn settings_manager() -> gio::Settings {
    SETTINGS.with(|settings| {
        settings
            .get()
            .expect("GSettings must be accessed on the main thread")
            .root
            .clone()
    })
}

// Workers read shared values; the keyfile backend and its change handlers stay
// on the main thread because the backend is not thread safe.
pub struct SettingsReader {
    path: String,
}

pub fn settings_reader() -> SettingsReader {
    SettingsReader {
        path: String::new(),
    }
}

impl SettingsReader {
    pub fn child(&self, name: &str) -> Self {
        Self {
            path: format!("{}{name}/", self.path),
        }
    }

    pub fn value(&self, key: &str) -> glib::Variant {
        VALUES
            .read()
            .unwrap()
            .get(&format!("{}{key}", self.path))
            .expect("Unknown settings key")
            .clone()
    }

    pub fn boolean(&self, key: &str) -> bool {
        self.value(key).get().unwrap()
    }

    pub fn uint(&self, key: &str) -> u32 {
        self.value(key).get().unwrap()
    }

    pub fn double(&self, key: &str) -> f64 {
        self.value(key).get().unwrap()
    }

    pub fn string(&self, key: &str) -> glib::GString {
        self.value(key).str().unwrap().into()
    }
}
