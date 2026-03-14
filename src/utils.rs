use crate::cache::sqlite;
use crate::config::APPLICATION_ID;
use aho_corasick::AhoCorasick;
use gio::prelude::*;
use gtk::gdk;
use gtk::gio;
use gtk::Ordering;
use image::{imageops::FilterType, DynamicImage, RgbImage};
use mpd::status::AudioFormat;
use once_cell::sync::Lazy;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::cell::RefCell;
use std::fmt::Write;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::RwLock;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use time::error::IndeterminateOffset;
use time::format_description::{parse_owned, OwnedFormatItem};
use time::OffsetDateTime;
use time::UtcOffset;
use tokio::runtime::Runtime;
use uuid::Uuid;

static APP_CACHE_PATH: Lazy<PathBuf> = Lazy::new(|| {
    let mut res = glib::user_cache_dir();
    res.push("euphonica");
    res
});

pub fn get_app_cache_path() -> PathBuf {
    APP_CACHE_PATH.clone()
}

pub fn get_image_cache_path() -> PathBuf {
    let mut res = get_app_cache_path();
    res.push("images");
    res
}

pub fn get_doc_cache_path() -> PathBuf {
    let mut res = get_app_cache_path();
    res.push("metadata.sqlite");
    res
}

/// Spawn a Tokio runtime on a new thread. This is needed by the zbus dependency.
pub fn tokio_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| Runtime::new().expect("Setting up tokio runtime needs to succeed."))
}

/// Get GSettings for the entire application.
pub fn settings_manager() -> gio::Settings {
    // Trim the .Devel suffix if exists
    let app_id = APPLICATION_ID.trim_end_matches(".Devel");
    gio::Settings::new(app_id)
}

/// Shortcut to a metadata provider's settings.
pub fn meta_provider_settings(key: &str) -> gio::Settings {
    // Trim the .Devel suffix if exists
    settings_manager().child("metaprovider").child(key)
}

pub fn format_secs_as_duration(seconds: f64) -> String {
    let total_seconds = seconds.round() as i64;
    let days = total_seconds / 86400;
    let hours = (total_seconds % 86400) / 3600;
    let minutes = (total_seconds % 3600) / 60;
    let seconds = total_seconds % 60;

    if days > 0 {
        format!("{days} days {hours:02}:{minutes:02}:{seconds:02}")
    } else if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

pub fn format_bitrate(bitrate_kbps: u32) -> String {
    if bitrate_kbps < 5000 {
        format!("{bitrate_kbps}kbps")
    } else {
        let bitrate_mbps = bitrate_kbps as f64 / 1000.0;
        let mut buffer = String::new();
        let result = write!(&mut buffer, "{bitrate_mbps:.2}Mbps");

        match result {
            Ok(_) => buffer,
            Err(e) => {
                format!("{e:?}")
            }
        }
    }
}

// For convenience
pub fn prettify_audio_format(format: &AudioFormat) -> String {
    // Here we need to re-infer whether this format is DSD or PCM
    // Only detect DSD64 at minimum, anything lower is too esoteric
    if format.bits == 1 && format.rate >= 352800 {
        // Is probably DSD
        let sample_rate = format.rate * 8;
        return format!(
            "{} ({:.4}MHz) {}ch",
            sample_rate / 44100,
            (sample_rate as f64) / 1e6,
            format.chans
        );
    }
    format!(
        "{}bit {:.1}kHz {}ch",
        format.bits,
        (format.rate as f64) / 1e3,
        format.chans
    )
}

pub fn g_cmp_options<T: Ord>(
    s1: Option<&T>,
    s2: Option<&T>,
    nulls_first: bool,
    asc: bool,
) -> Ordering {
    if s1.is_none() && s2.is_none() {
        return Ordering::Equal;
    } else if s1.is_none() {
        if nulls_first {
            return Ordering::Smaller;
        }
        return Ordering::Larger;
    } else if s2.is_none() {
        if nulls_first {
            return Ordering::Larger;
        }
        return Ordering::Smaller;
    }
    if asc {
        return Ordering::from(s1.unwrap().cmp(s2.unwrap()));
    }
    Ordering::from(s2.unwrap().cmp(s1.unwrap()))
}

pub fn g_cmp_str_options(
    s1: Option<&str>,
    s2: Option<&str>,
    nulls_first: bool,
    asc: bool,
    case_sensitive: bool,
) -> Ordering {
    if s1.is_none() && s2.is_none() {
        return Ordering::Equal;
    } else if s1.is_none() {
        if nulls_first {
            return Ordering::Smaller;
        }
        return Ordering::Larger;
    } else if s2.is_none() {
        if nulls_first {
            return Ordering::Larger;
        }
        return Ordering::Smaller;
    }
    if asc {
        if case_sensitive {
            return Ordering::from(s1.unwrap().cmp(s2.unwrap()));
        }
        return Ordering::from(s1.unwrap().to_lowercase().cmp(&s2.unwrap().to_lowercase()));
    }
    if case_sensitive {
        return Ordering::from(s2.unwrap().cmp(s1.unwrap()));
    }
    Ordering::from(s2.unwrap().to_lowercase().cmp(&s1.unwrap().to_lowercase()))
}

pub fn g_search_substr(text: Option<&str>, term: &str, case_sensitive: bool) -> bool {
    if text.is_none() && term.is_empty() {
        return true;
    } else if text.is_some() && !term.is_empty() {
        if case_sensitive {
            return text.unwrap().contains(term);
        }
        return text.unwrap().to_lowercase().contains(&term.to_lowercase());
    }
    false
}

pub fn strip_filename_linux(path: &str) -> &str {
    // MPD insists on having a trailing slash so here we go
    if let Some(last_slash) = path.rfind('/') {
        return &path[..last_slash + 1];
    }
    // For tracks located at the root, just return empty string
    ""
}

pub fn read_image_from_bytes(bytes: Vec<u8>) -> Option<DynamicImage> {
    if let Ok(dyn_img) = image::load_from_memory(&bytes) {
        Some(dyn_img)
    } else {
        println!("read_image_from_bytes: Unable to infer image format from content");
        None
    }
}

/// Automatically resize & based on user settings, then convert to RGB8.
/// All providers should use this function on their child threads to resize applicable images
/// before returning the images to the main thread.
/// Two images will be returned: a high-resolution version and a thumbnail version.
/// Their major axis's resolution is determined by the keys hires-image-size and
/// thumbnail-image-size in the gschema respectively.
pub fn resize_convert_image(dyn_img: DynamicImage) -> (RgbImage, RgbImage) {
    let settings = settings_manager().child("library");
    // Avoid resizing to larger than the original image.
    let w = dyn_img.width();
    let h = dyn_img.height();
    let hires_size = settings.uint("hires-image-size").min(w.max(h));
    let thumbnail_short_edge = settings.uint("thumbnail-image-size");
    // For thumbnails, scale such that the short edge is equal to thumbnail_size.
    let thumbnail_sizes = if w > h {
        (
            (w as f32 * (thumbnail_short_edge as f32 / h as f32)).ceil() as u32,
            thumbnail_short_edge,
        )
    } else {
        (
            thumbnail_short_edge,
            (h as f32 * (thumbnail_short_edge as f32 / w as f32)).ceil() as u32,
        )
    };
    (
        dyn_img
            .resize(hires_size, hires_size, FilterType::Triangle)
            .into_rgb8(),
        dyn_img
            .thumbnail(thumbnail_sizes.0, thumbnail_sizes.1)
            .into_rgb8(),
    )
}

/// returns the image name that this is saved as
pub fn save_and_register_single_image(
    img: &RgbImage,
    key: &str,
    prefix: Option<&'static str>,
    is_thumb: bool,
) -> String {
    let mut path = get_image_cache_path();
    let name = Uuid::new_v4().simple().to_string() + ".png";
    path.push(&name);

    img.save(&path)
        .unwrap_or_else(|_| panic!("Couldn't save downloaded image to {:?}", &path));

    sqlite::register_image_key(key, prefix, Some(&name), is_thumb).expect("Sqlite error");

    name
}

pub struct RegisteredImage {
    /// image name (eg. uayhsjdkjasuijad.png)
    pub name: String,
    /// this field is only present if it is returned by a method that created the image
    pub img: RefCell<Option<RgbImage>>,
}

impl RegisteredImage {
    pub fn take_texture(&self) -> Result<gdk::Texture, glib::Error> {
        if let Some(rgb_image) = self.img.take() {
            let builder = gdk::MemoryTextureBuilder::new();
            builder.set_width(rgb_image.width() as i32);
            builder.set_height(rgb_image.height() as i32);
            builder.set_format(gdk::MemoryFormat::R8g8b8);
            builder.set_stride((rgb_image.width() * 3) as usize);
            builder.set_bytes(Some(&glib::Bytes::from_owned(rgb_image.into_raw())));
            Ok(builder.build())
        } else {
            let mut res = get_image_cache_path();
            res.push(&self.name);

            gdk::Texture::from_filename(res).inspect_err(|e| {
                dbg!(e);
            })
        }
    }
}

impl TryInto<gdk::Texture> for RegisteredImage {
    type Error = glib::Error;
    fn try_into(self) -> Result<gdk::Texture, Self::Error> {
        self.take_texture()
    }
}

pub struct RegisteredImageBundle {
    pub hires: RegisteredImage,
    pub thumb: RegisteredImage,
}

impl RegisteredImageBundle {
    pub fn take_texture(&self, thumb: bool) -> Result<gdk::Texture, glib::Error> {
        if thumb {
            self.thumb.take_texture()
        } else {
            self.hires.take_texture()
        }
    }
}

/// this is really a util wrap around resizing the dyn_img & registering. For fine grain control, you can call those individually
pub fn save_and_register_image(
    dyn_img: DynamicImage,
    key: &str,
    prefix: Option<&'static str>,
) -> RegisteredImageBundle {
    let (hires_img, thumb_img) = resize_convert_image(dyn_img);
    let hires_k = save_and_register_single_image(&hires_img, key, prefix, false);
    let thumb_k = save_and_register_single_image(&thumb_img, key, prefix, true);

    RegisteredImageBundle {
        hires: RegisteredImage {
            name: hires_k,
            img: RefCell::new(Some(hires_img)),
        },
        thumb: RegisteredImage {
            name: thumb_k,
            img: RefCell::new(Some(thumb_img)),
        },
    }
}

pub fn register_image_as_failure(key: &str, prefix: Option<&'static str>) {
    sqlite::register_image_key(key, prefix, None, false).expect("Sqlite error");
    sqlite::register_image_key(key, prefix, None, true).expect("Sqlite error");
}

pub fn current_unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

pub static LOCAL_TZ_OFFSET: OnceLock<Result<UtcOffset, IndeterminateOffset>> = OnceLock::new();

/// Safely retrieve the initialized local UTC offset, computing it if necessary.
/// Returns the UtcOffset or the error encountered during system lookup.
fn get_local_tz_offset() -> Result<UtcOffset, IndeterminateOffset> {
    *LOCAL_TZ_OFFSET.get_or_init(UtcOffset::current_local_offset)
}

/// Static storage for the determined locale format string.
static LOCALE_FORMAT: OnceLock<OwnedFormatItem> = OnceLock::new();

/// Determine the time format string based on the system's locale environment variables.
/// A normal user wouldn't be juggling locales while using their computer so doing this
/// once at startup suffices. For now assume Linux.
///
/// Note: This uses a simple heuristic to switch between common US and European formats.
/// A fully robust solution would require a dedicated i18n crate.
fn get_locale_format() -> &'static OwnedFormatItem {
    LOCALE_FORMAT.get_or_init(|| {
        // Check LC_TIME first, then LANG, then fall back to the default ISO-style format.
        let locale_str = std::env::var("LC_TIME")
            .or_else(|_| std::env::var("LANG"))
            .unwrap_or_default()
            .to_lowercase();

        if locale_str.contains("us") || locale_str.contains("ca") {
            // Example for North America: MM/DD/YYYY HH:MM:SS
            parse_owned::<2>(
                "[month padding:none]/[day padding:none]/[year] [hour]:[minute]:[second]",
            )
            .unwrap()
        } else if locale_str.contains("gb")
            || locale_str.contains("de")
            || locale_str.contains("fr")
            || locale_str.contains("au")
        {
            // Example for Europe/UK/Australia: DD/MM/YYYY HH:MM:SS
            parse_owned::<2>(
                "[day padding:none]/[month padding:none]/[year] [hour]:[minute]:[second]",
            )
            .unwrap()
        } else {
            // Default
            parse_owned::<2>("[year]-[month]-[day] [hour]:[minute]:[second]").unwrap()
        }
    })
}

pub fn format_datetime_local_tz(utc_dt: OffsetDateTime) -> String {
    let local_dt = get_local_tz_offset().map_or(utc_dt, |offset| utc_dt.to_offset(offset));

    local_dt.format(get_locale_format()).unwrap()
}

// Build Aho-Corasick automatons only once. In case no delimiter or exception is
// specified, no automaton will be returned. Caller code should take that as a signal
// to skip parsing and use the tags as-is.
// Changes in delimiters and exceptions require restarting.
// TODO: Might want to research memoisation so we can rebuild these automatons upon
// changing settings.
pub fn build_aho_corasick_automaton(phrases: &[&str]) -> Option<AhoCorasick> {
    if phrases.is_empty() {
        None
    } else {
        // println!("[AhoCorasick] Configured to detect the following: {:?}", phrases);
        Some(AhoCorasick::new(phrases).unwrap())
    }
}
fn build_artist_delim_automaton() -> Option<AhoCorasick> {
    let setting = settings_manager()
        .child("library")
        .value("artist-tag-delims");
    let delims: Vec<&str> = setting.array_iter_str().unwrap().collect();
    build_aho_corasick_automaton(&delims)
}
fn build_artist_delim_exceptions_automaton() -> Option<AhoCorasick> {
    let setting = settings_manager()
        .child("library")
        .value("artist-tag-delim-exceptions");
    let excepts: Vec<&str> = setting.array_iter_str().unwrap().collect();
    build_aho_corasick_automaton(&excepts)
}

pub static ARTIST_DELIM_AUTOMATON: Lazy<RwLock<Option<AhoCorasick>>> = Lazy::new(|| {
    // println!("Initialising Aho-Corasick automaton for artist tag delimiters...");
    let opt_automaton = build_artist_delim_automaton();
    RwLock::new(opt_automaton)
});

pub fn rebuild_artist_delim_automaton() {
    if let Ok(mut automaton) = ARTIST_DELIM_AUTOMATON.write() {
        // println!("Rebuilding Aho-Corasick automaton for artist tag delimiters...");
        let new = build_artist_delim_automaton();
        *automaton = new;
    }
}

pub static ARTIST_DELIM_EXCEPTION_AUTOMATON: Lazy<RwLock<Option<AhoCorasick>>> = Lazy::new(|| {
    // println!("Initialising Aho-Corasick automaton for artist tag delimiter exceptions...");
    let opt_automaton = build_artist_delim_exceptions_automaton();
    RwLock::new(opt_automaton)
});

pub fn rebuild_artist_delim_exception_automaton() {
    if let Ok(mut automaton) = ARTIST_DELIM_EXCEPTION_AUTOMATON.write() {
        // println!("Rebuilding Aho-Corasick automaton for artist tag delimiters...");
        let new = build_artist_delim_exceptions_automaton();
        *automaton = new;
    }
}

/// There are two guard layers against full fetches.
/// - This LazyInit trait. All heavy views must implement it. A view's populate() will then be called
/// by the sidebar upon navigating to that view. If that view is already initialised, populate() must
/// be a noop(). TODO: enforce noop at sidebar level instead.
/// - Additional checks at the controller level, to prevent new windows (after surfacing from background)
/// from mistakenly reinitialising already-fetched models.
pub trait LazyInit {
    fn populate(&self);
}

/// Exports any type that implements Serialize to a JSON file.
///
/// # Arguments
/// * `data` - A reference to the struct to serialize.
/// * `file_path` - The path where the JSON file will be saved. Assume we have
/// write access to this path already.
pub fn export_to_json<T: Serialize>(
    data: &T,
    file_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(file_path)?;
    // Use BufWriter for better performance (reduces system calls).
    let writer = BufWriter::new(file);
    serde_json::to_writer_pretty(writer, data)?;

    Ok(())
}

/// Import a type that implements Deserialize from a JSON file.
///
/// # Arguments
/// * `file_path` - The path to the JSON file.
///
/// # Returns
/// A Result containing the deserialized struct or an error.
/// We use `DeserializeOwned` here because we are creating the data from the file,
/// so it must own its memory (not borrow from the input string).
pub fn import_from_json<T: DeserializeOwned>(
    file_path: &str,
) -> Result<T, Box<dyn std::error::Error>> {
    let file = File::open(file_path)?;
    let reader = BufReader::new(file);
    let deserialized_data = serde_json::from_reader(reader)?;

    Ok(deserialized_data)
}

/// Describe how long ago was a timestamp compared to now.
///
/// # Arguments
/// * `past_ts` - A Unix timestamp in the past. If a timestamp in the future
/// is given, will treat as right now.
/// TODO: translations
pub fn get_time_ago_desc(past_ts: i64) -> String {
    let now = OffsetDateTime::now_utc();
    let diff = (now.unix_timestamp() - past_ts) as f64;
    let diff_days = diff / 86400.0;
    if diff_days <= 0.0 {
        String::from("now")
    } else if diff_days >= 365.0 {
        let years = (diff_days / 365.0).floor() as u32;
        if years == 1 {
            String::from("last year")
        } else {
            format!("{years} years ago")
        }
    } else if diff_days >= 30.0 {
        // Just let a month be 30 days long on average :)
        let months = (diff_days / 30.0).floor() as u32;
        if months == 1 {
            String::from("last month")
        } else {
            format!("{months} months ago")
        }
    } else if diff_days >= 2.0 {
        format!("{diff_days:.0} days ago")
    } else if diff_days >= 1.0 {
        String::from("yesterday")
    } else if diff >= 3600.0 {
        let hours = (diff / 3600.0).floor() as u32;
        format!("{hours}h ago")
    } else if diff >= 60.0 {
        let mins = (diff / 60.0).floor() as u32;
        format!("{mins}m ago")
    } else {
        "just now".to_string()
    }
}
