use crate::config::APPLICATION_ID;
use aho_corasick::AhoCorasick;
use gio::prelude::*;
use gtk::gio;
use gtk::Ordering;
use image::{imageops::FilterType, ImageReader, DynamicImage, RgbImage};
use mpd::status::AudioFormat;
use once_cell::sync::Lazy;
use std::sync::OnceLock;
use std::fmt::Write;
use std::{io::Cursor, sync::RwLock};
use tokio::runtime::Runtime;

/// Spawn a Tokio runtime on a new thread. This is needed by the zbus dependency.
pub fn tokio_runtime() -> &'static Runtime {
    static RUNTIME: OnceLock<Runtime> = OnceLock::new();
    RUNTIME.get_or_init(|| {
        Runtime::new().expect("Setting up tokio runtime needs to succeed.")
    })
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
        format!("{} days {:02}:{:02}:{:02}", days, hours, minutes, seconds)
    } else if hours > 0 {
        format!("{:02}:{:02}:{:02}", hours, minutes, seconds)
    } else {
        format!("{:02}:{:02}", minutes, seconds)
    }
}

pub fn format_bitrate(bitrate_kbps: u32) -> String {
    if bitrate_kbps < 5000 {
        format!("{}kbps", bitrate_kbps)
    } else {
        let bitrate_mbps = bitrate_kbps as f64 / 1000.0;
        let mut buffer = String::new();
        let result = write!(&mut buffer, "{:.2}Mbps", bitrate_mbps);

        match result {
            Ok(_) => buffer,
            Err(e) => {
                format!("{:?}", e)
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
    if let Some(dyn_img) = image::load_from_memory(&bytes).ok() {
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
    let hires_size = settings
        .uint("hires-image-size")
        .min(w.max(h));
    let thumbnail_short_edge = settings.uint("thumbnail-image-size");
    // For thumbnails, scale such that the short edge is equal to thumbnail_size.
    let thumbnail_sizes = if w > h {
        ((w as f32 * (thumbnail_short_edge as f32 / h as f32)).ceil() as u32, thumbnail_short_edge)
    } else {
        (thumbnail_short_edge, (h as f32 * (thumbnail_short_edge as f32 / w as f32)).ceil() as u32)
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
    fn clear(&self);
    fn populate(&self);
}
