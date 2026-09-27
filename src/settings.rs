use gtk::{gio, glib, prelude::*};
use once_cell::sync::Lazy;
use serde_json::{Map, Value};

use crate::config::APPLICATION_ID;

const SETTINGS_PATH: &str = "/io/github/htkhiem/Euphonica/";
const DCONF_PROBE_TIMEOUT_MS: i32 = 3000;

static USE_JSON_BACKEND: Lazy<bool> = Lazy::new(|| {
    if let Some(reason) = fallback_reason(&gio::SettingsBackend::default()) {
        eprintln!("dconf unavailable ({reason}); using JSON settings");
        true
    } else {
        false
    }
});
static JSON_BACKEND: Lazy<JsonSettingsBackend> = Lazy::new(glib::Object::new);

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

pub fn init() {
    let _ = settings_manager();
}

pub fn settings_manager() -> gio::Settings {
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    if *USE_JSON_BACKEND {
        gio::Settings::with_backend(app_id, &*JSON_BACKEND)
    } else {
        gio::Settings::new(app_id)
    }
}

glib::wrapper! {
    pub struct JsonSettingsBackend(ObjectSubclass<imp::JsonSettingsBackend>)
        @extends gio::SettingsBackend;
}

// GSettingsBackend synchronizes its watchers. This subclass synchronizes its
// state and writes, releasing both locks before emitting notifications.
unsafe impl Send for JsonSettingsBackend {}
unsafe impl Sync for JsonSettingsBackend {}

fn variant_to_json(value: &glib::Variant) -> Option<Value> {
    match value.type_().as_str() {
        "b" => Some(value.get::<bool>()?.into()),
        "i" => Some(value.get::<i32>()?.into()),
        "u" => Some(value.get::<u32>()?.into()),
        "d" => serde_json::Number::from_f64(value.get()?).map(Value::Number),
        "s" => Some(value.str()?.into()),
        "as" => serde_json::to_value(value.get::<Vec<String>>()?).ok(),
        _ => None,
    }
}

fn json_to_variant(value: &Value, expected_type: &glib::VariantTy) -> Option<glib::Variant> {
    match expected_type.as_str() {
        "b" => Some(
            serde_json::from_value::<bool>(value.clone())
                .ok()?
                .to_variant(),
        ),
        "i" => Some(
            serde_json::from_value::<i32>(value.clone())
                .ok()?
                .to_variant(),
        ),
        "u" => Some(
            serde_json::from_value::<u32>(value.clone())
                .ok()?
                .to_variant(),
        ),
        "d" => Some(
            serde_json::from_value::<f64>(value.clone())
                .ok()?
                .to_variant(),
        ),
        "s" => {
            let value = value.as_str()?;
            if value.contains('\0') {
                return None;
            }
            Some(value.to_variant())
        }
        "as" => {
            let values: Vec<String> = serde_json::from_value(value.clone()).ok()?;
            if values.iter().any(|value| value.contains('\0')) {
                return None;
            }
            Some(values.to_variant())
        }
        _ => None,
    }
}

fn set_value(values: &mut Map<String, Value>, key: &str, value: Option<Value>) -> bool {
    if let Some((child, key)) = key.split_once('/') {
        if value.is_none() && !values.contains_key(child) {
            return true;
        }
        let child_value = values
            .entry(child)
            .or_insert_with(|| Value::Object(Map::new()));
        let Some(children) = child_value.as_object_mut() else {
            return false;
        };
        if !set_value(children, key, value) {
            return false;
        }
        if children.is_empty() {
            values.remove(child);
        }
    } else if let Some(value) = value {
        values.insert(key.to_owned(), value);
    } else {
        values.remove(key);
    }
    true
}

mod imp {
    use super::*;
    use glib::{subclass::prelude::*, translate::*};
    use std::{
        ffi::{CStr, c_char},
        fs,
        io::ErrorKind,
        path::PathBuf,
        ptr,
        sync::Mutex,
    };

    pub struct JsonSettingsBackend {
        path: PathBuf,
        values: Mutex<Option<Map<String, Value>>>,
        write_lock: Mutex<()>,
    }

    impl Default for JsonSettingsBackend {
        fn default() -> Self {
            let path = glib::user_config_dir()
                .join("euphonica")
                .join("settings.json");
            let values = match fs::read(&path) {
                Ok(contents) => match serde_json::from_slice(&contents) {
                    Ok(values) => Some(values),
                    Err(error) => {
                        eprintln!("Cannot read settings {}: {error}", path.display());
                        None
                    }
                },
                Err(error) if error.kind() == ErrorKind::NotFound => Some(Map::new()),
                Err(error) => {
                    eprintln!("Cannot read settings {}: {error}", path.display());
                    None
                }
            };
            Self {
                path,
                values: Mutex::new(values),
                write_lock: Mutex::new(()),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for JsonSettingsBackend {
        const NAME: &'static str = "EuphonicaJsonSettingsBackend";
        type Type = super::JsonSettingsBackend;
        type ParentType = gio::SettingsBackend;
    }

    impl ObjectImpl for JsonSettingsBackend {}

    // gio-rs exposes the backend class but does not provide subclass bindings.
    unsafe impl IsSubclassable<JsonSettingsBackend> for gio::SettingsBackend {
        fn class_init(class: &mut glib::Class<Self>) {
            Self::parent_class_init::<JsonSettingsBackend>(class);
            let class = class.as_mut();
            class.read = Some(read);
            class.get_writable = Some(get_writable);
            class.write = Some(write);
            class.write_tree = Some(write_tree);
            class.reset = Some(reset);
        }
    }

    impl JsonSettingsBackend {
        fn read_value(&self, key: &str, expected_type: &glib::VariantTy) -> Option<glib::Variant> {
            let key = key.strip_prefix(SETTINGS_PATH)?;
            let values = self.values.lock().unwrap();
            let mut parts = key.split('/');
            let mut value = values.as_ref()?.get(parts.next()?)?;
            for part in parts {
                value = value.as_object()?.get(part)?;
            }
            json_to_variant(value, expected_type)
        }

        fn write_values(&self, changes: &[(String, Option<glib::Variant>)]) -> bool {
            let _write_guard = self.write_lock.lock().unwrap();
            let mut updated = {
                let values = self.values.lock().unwrap();
                let Some(current) = values.as_ref() else {
                    return false;
                };
                current.clone()
            };
            for (key, value) in changes {
                let Some(key) = key.strip_prefix(SETTINGS_PATH) else {
                    return false;
                };
                let value = match value {
                    Some(value) => {
                        let Some(value) = variant_to_json(value) else {
                            return false;
                        };
                        Some(value)
                    }
                    None => None,
                };
                if !set_value(&mut updated, key, value) {
                    return false;
                }
            }
            if let Err(error) = self.save(&updated) {
                eprintln!("Cannot write settings {}: {error}", self.path.display());
                return false;
            }
            *self.values.lock().unwrap() = Some(updated);
            true
        }

        fn save(&self, values: &Map<String, Value>) -> Result<(), Box<dyn std::error::Error>> {
            let contents = serde_json::to_vec_pretty(values)?;
            fs::create_dir_all(self.path.parent().unwrap())?;
            glib::file_set_contents_full(
                &self.path,
                &contents,
                glib::FileSetContentsFlags::CONSISTENT | glib::FileSetContentsFlags::DURABLE,
                0o600,
            )?;
            Ok(())
        }
    }

    unsafe extern "C" fn read(
        backend: *mut gio::ffi::GSettingsBackend,
        key: *const c_char,
        expected_type: *const glib::ffi::GVariantType,
        default_value: glib::ffi::gboolean,
    ) -> *mut glib::ffi::GVariant {
        if default_value != glib::ffi::GFALSE {
            return ptr::null_mut();
        }
        let instance =
            unsafe { &*backend.cast::<<JsonSettingsBackend as ObjectSubclass>::Instance>() };
        let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
        let expected_type = unsafe { glib::VariantTy::from_ptr(expected_type) };
        instance.imp().read_value(key, expected_type).to_glib_full()
    }

    unsafe extern "C" fn get_writable(
        backend: *mut gio::ffi::GSettingsBackend,
        key: *const c_char,
    ) -> glib::ffi::gboolean {
        let instance =
            unsafe { &*backend.cast::<<JsonSettingsBackend as ObjectSubclass>::Instance>() };
        let key = unsafe { CStr::from_ptr(key) }.to_str().unwrap();
        (key.starts_with(SETTINGS_PATH) && instance.imp().values.lock().unwrap().is_some())
            .into_glib()
    }

    unsafe extern "C" fn write(
        backend: *mut gio::ffi::GSettingsBackend,
        key: *const c_char,
        value: *mut glib::ffi::GVariant,
        origin_tag: glib::ffi::gpointer,
    ) -> glib::ffi::gboolean {
        let instance =
            unsafe { &*backend.cast::<<JsonSettingsBackend as ObjectSubclass>::Instance>() };
        let name = unsafe { CStr::from_ptr(key) }.to_str().unwrap().to_owned();
        let value = unsafe { from_glib_none(value) };
        if !instance.imp().write_values(&[(name, Some(value))]) {
            return glib::ffi::GFALSE;
        }
        unsafe { gio::ffi::g_settings_backend_changed(backend, key, origin_tag) };
        glib::ffi::GTRUE
    }

    unsafe extern "C" fn reset(
        backend: *mut gio::ffi::GSettingsBackend,
        key: *const c_char,
        origin_tag: glib::ffi::gpointer,
    ) {
        let instance =
            unsafe { &*backend.cast::<<JsonSettingsBackend as ObjectSubclass>::Instance>() };
        let name = unsafe { CStr::from_ptr(key) }.to_str().unwrap().to_owned();
        if instance.imp().write_values(&[(name, None)]) {
            unsafe { gio::ffi::g_settings_backend_changed(backend, key, origin_tag) };
        }
    }

    unsafe extern "C" fn write_tree(
        backend: *mut gio::ffi::GSettingsBackend,
        tree: *mut glib::ffi::GTree,
        origin_tag: glib::ffi::gpointer,
    ) -> glib::ffi::gboolean {
        unsafe extern "C" fn collect(
            key: glib::ffi::gpointer,
            value: glib::ffi::gpointer,
            data: glib::ffi::gpointer,
        ) -> glib::ffi::gboolean {
            let changes = unsafe { &mut *data.cast::<Vec<(String, Option<glib::Variant>)>>() };
            let key = unsafe { CStr::from_ptr(key.cast()) }
                .to_str()
                .unwrap()
                .to_owned();
            let value = if value.is_null() {
                None
            } else {
                Some(unsafe { from_glib_none(value.cast::<glib::ffi::GVariant>()) })
            };
            changes.push((key, value));
            glib::ffi::GFALSE
        }

        let instance =
            unsafe { &*backend.cast::<<JsonSettingsBackend as ObjectSubclass>::Instance>() };
        let mut changes: Vec<(String, Option<glib::Variant>)> = Vec::new();
        unsafe {
            glib::ffi::g_tree_foreach(tree, Some(collect), (&mut changes as *mut Vec<_>).cast());
        }
        if changes.is_empty() {
            return glib::ffi::GTRUE;
        }
        if !instance.imp().write_values(&changes) {
            return glib::ffi::GFALSE;
        }
        unsafe { gio::ffi::g_settings_backend_changed_tree(backend, tree, origin_tag) };
        glib::ffi::GTRUE
    }
}
