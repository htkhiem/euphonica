use regex::Regex;
/// Config file generator, for use with the managed MPD instance.
/// Since it's only meant for the above case, there is no need to allow configuring things like state file,
/// sticker DB or bind_to_address. These things are always on & fully abstracted away to minimise fuss.
/// In the future we can probably expose this as some sort of "config generator" for user-managed MPD servers too?
/// When that happens the above will need to be implemented properly.
///
/// The format is kinda simple but nonstandard so it's not worth trying to shoehorn Serde here.
use std::fmt::{Display, Write};
use strum::{EnumMessage, VariantNames};
use strum_macros::{Display, EnumIter, EnumMessage, EnumString, FromRepr, VariantNames};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{
    config::VERSION,
    utils::{get_app_cache_path, get_standalone_playlists_path},
};

// We use the to_string one for UI display and the serialize one for writing into config.
// This allows us to use VariantNames to programmatically populate the gtk::StringLists,
// and use EnumString to deserialize config values
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
    Display,
    EnumString,
    EnumIter,
    EnumMessage,
    FromRepr,
    VariantNames,
)]
pub enum PcmSampleRate {
    #[default]
    #[strum(to_string = "*", serialize = "*")]
    Any,
    // You're sane, thank you
    #[strum(serialize = "44100", to_string = "44.1kHz")]
    P441,
    // Most systems use this to balance both audio and video
    #[strum(serialize = "48000", to_string = "48kHz")]
    P480,
    #[strum(serialize = "88200", to_string = "88.2kHz")]
    P882,
    #[strum(serialize = "96000", to_string = "96kHz")]
    P960,
    #[strum(serialize = "176400", to_string = "176.4kHz")]
    P1764,
    // You overpaid for your digital copies
    #[strum(serialize = "192000", to_string = "192kHz")]
    P1920,
    #[strum(serialize = "352800", to_string = "352.8kHz")]
    P3528,
    #[strum(serialize = "384000", to_string = "384kHz")]
    P3840,
    #[strum(serialize = "705600", to_string = "705.6kHz")]
    P7056,
    // Just because your DAC can doesn't mean you should
    #[strum(serialize = "768000", to_string = "768kHz")]
    P7680,
    // Do these even exist
    #[strum(serialize = "1411200", to_string = "1.4112MHz")]
    P14112,
    #[strum(serialize = "1536000", to_string = "1.536MHz")]
    P15360,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
    Display,
    EnumString,
    EnumIter,
    EnumMessage,
    FromRepr,
    VariantNames,
)]
pub enum PcmBitDepth {
    #[default]
    #[strum(to_string = "*", serialize = "*")]
    Any,
    #[strum(serialize = "8", to_string = "8bit")]
    I8,
    #[strum(serialize = "16", to_string = "16bit")]
    I16,
    #[strum(serialize = "24", to_string = "24bit")]
    I24,
    #[strum(serialize = "32", to_string = "32bit")]
    I32,
    #[strum(serialize = "f", to_string = "32bit (float)")]
    F32,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
    Display,
    EnumString,
    EnumIter,
    EnumMessage,
    FromRepr,
    VariantNames,
)]
pub enum DsdMultiplier {
    #[default]
    #[strum(to_string = "*", serialize = "*")]
    Any,
    // None of these are sane, but you do you
    #[strum(serialize = "64", to_string = "64 (2.8MHz)")]
    D64,
    #[strum(serialize = "128", to_string = "128 (5.6MHz)")]
    D128,
    #[strum(serialize = "256", to_string = "256 (11.2MHz)")]
    D256,
    #[strum(serialize = "512", to_string = "512 (22.6MHz)")]
    D512,
    // Outside of a few British snakeoil DACs with DSD upsampling (and horrible SINAD) I haven't seen DSD1024+ in the wild.
    #[strum(serialize = "1024", to_string = "1024 (45.2MHz)")]
    D1024,
    #[strum(serialize = "1536", to_string = "1536 (67.7MHz)")]
    D1535,
    #[strum(serialize = "2048", to_string = "2048 (90.3MHz)")]
    D2048,
}

/// rust-mpd already has a format parser, but we'll redefine our own format config and parsing to be
/// friendlier to the UI controls, to allow for wildcards, and to have our own error reporting.
/// TODO: move into our rust-mpd fork.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AudioFormatConfig {
    /// Good ol' Pulse Code Modulation
    Pcm(
        /// Sample rate
        PcmSampleRate,
        /// Bit depth and is-float
        PcmBitDepth,
        /// Number of channels
        Option<u8>,
    ),
    /// "but but it pushes noise into the ultrasonic range so it sounds better1!11!!!1" Direct Stream Digital
    Dsd(
        /// Red Book sample multiplier (64, 128, 256, etc)
        DsdMultiplier,
        /// Number of channels
        Option<u8>,
    ),
}

impl AudioFormatConfig {
    pub const DEFAULT: Self = Self::Pcm(PcmSampleRate::P441, PcmBitDepth::I16, None);
    pub fn is_dsd(&self) -> bool {
        match self {
            Self::Pcm(_, _, _) => false,
            Self::Dsd(_, _) => true,
        }
    }
}

impl Default for AudioFormatConfig {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl TryFrom<&str> for AudioFormatConfig {
    type Error = String;
    /// Attempt to parse an MPD format string. For DSD this only supports preset-type strings.
    /// Who in their right mind would even attempt to use custom DSD multipliers?
    /// https://mpd.readthedocs.io/en/stable/user.html#global-audio-format
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        // First, see if it starts with DSD.
        // Note: too lazy to add Perl classes so the regex patterns may look weird.
        if value.starts_with("dsd") {
            // Is DSD
            let re = Regex::new(r"^dsd([[:digit:]]+):([0-9*]+)$").unwrap();
            if let Some(caps) = re.captures(value) {
                // Won't hit this without having found every group so we won't panic here.
                let (_, [mul, channels]) = caps.extract();
                let mul: DsdMultiplier = mul.try_into().map_err(|_| {
                    format!(
                        "DSD multiplier {} not in {:?}",
                        mul,
                        DsdMultiplier::VARIANTS
                    )
                })?;
                let channels = if channels == "*" {
                    None
                } else {
                    Some(
                        channels
                            .parse::<u8>()
                            .map_err(|_| format!("Invalid channel count: {}", mul))?,
                    )
                };
                if channels.is_some_and(|channels| channels < 1 || channels > 128) {
                    return Err(format!(
                        "Channel count must be between 1 and 128 (got {})",
                        channels.unwrap()
                    ));
                }
                return Ok(Self::Dsd(mul, channels));
            } else {
                return Err(format!("Invalid DSD preset spec: {}", value));
            }
        } else if value.contains("dsd") {
            // Custom DSD strings are unsupported.
            return Err(format!(
                "Custom DSD format strings are unsupported: {}",
                value
            ));
        } else {
            // Is PCM
            let re = Regex::new(r"^([0-9*]+):([0-9*f]+):([0-9*]+)$").unwrap();
            if let Some(caps) = re.captures(value) {
                let (_, [rate, bits, channels]) = caps.extract();

                let rate = rate
                    .try_into()
                    .map_err(|_| format!("Invalid PCM sample rate: {}", rate))?;
                let bits = bits.try_into().map_err(|_| {
                    format!(
                        "Invalid PCM bit depth: {} (must be 8, 16, 24, 32 or f)",
                        bits
                    )
                })?;
                let channels = if channels == "*" {
                    None
                } else {
                    Some(
                        channels
                            .parse::<u8>()
                            .map_err(|_| format!("Invalid channel count: {}", channels))?,
                    )
                };
                if channels.is_some_and(|channels| channels < 1 || channels > 128) {
                    return Err(format!(
                        "Channel count must be between 1 and 128 (got {})",
                        channels.unwrap()
                    ));
                }
                return Ok(Self::Pcm(rate, bits, channels));
            } else {
                return Err(format!("Invalid PCM preset spec: {}", value));
            }
        }
    }
}

impl Display for AudioFormatConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Dsd(mul, ch) => {
                write!(
                    f,
                    "dsd{}:{}",
                    mul,
                    ch.map(|ch| ch.to_string()).unwrap_or(String::from("*"))
                )
            }
            Self::Pcm(rate, bits, ch) => {
                write!(
                    f,
                    "{}:{}:{}",
                    rate,
                    bits,
                    ch.map(|ch| ch.to_string()).unwrap_or(String::from("*"))
                )
            }
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Default,
    Display,
    EnumString,
    EnumIter,
    EnumMessage,
    FromRepr,
    VariantNames,
)]
#[non_exhaustive]
pub enum OutputType {
    #[strum(serialize = "httpd", to_string = "HTTPD")]
    Httpd,
    #[strum(serialize = "alsa", to_string = "ALSA")]
    Alsa,
    #[strum(serialize = "pulse", to_string = "PulseAudio")]
    Pulse,
    #[strum(serialize = "oss", to_string = "OSS")]
    Oss,
    #[strum(serialize = "pipewire", to_string = "PipeWire")]
    #[default]
    PipeWire,
}

/// ALSA, OSS and Pulse supports hardware mixer and MPD uses that as default.
/// Other outputs use None as default.
/// To leave this to default, simply do not specify in the config file (leave option as None).
#[derive(
    Debug, Clone, Copy, PartialEq, Display, EnumString, VariantNames, Default, EnumMessage, FromRepr
)]
#[non_exhaustive]
pub enum MixerType {
    #[strum(serialize = "", to_string = "Plugin default")]
    #[default]
    Default,
    #[strum(serialize = "hardware", to_string = "Hardware")]
    Hardware,
    #[strum(serialize = "software", to_string = "Software")]
    Software,
    #[strum(serialize = "null", to_string = "Null (bypass)")]
    Null,
    #[strum(serialize = "none", to_string = "None (disable)")]
    None,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Default, Display, EnumString, VariantNames, EnumMessage, FromRepr
)]
#[non_exhaustive]
pub enum ReplayGainHandler {
    #[default]
    #[strum(serialize = "software", to_string = "Software (pre-mixer)")]
    Software,
    #[strum(serialize = "mixer", to_string = "Use mixer")]
    Mixer,
    #[strum(serialize = "none", to_string = "None")]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
#[non_exhaustive]
pub enum LogLevel {
    Error,
    #[default]
    Warning,
    Notice,
    Info,
    Verbose,
}

fn parse_key_value<'a>(line: &'a str) -> Option<(&'a str, &'a str)> {
    let mut parts = line.splitn(2, |c: char| c.is_whitespace());
    let key = parts.next()?.trim();
    let rest = parts.next()?.trim();

    // Remove quotes around values
    let val = rest.trim_matches('"');
    Some((key, val))
}

/// A single `audio_output` block.
///
/// Not to be confused with the runtime `mpd::output::Output` struct, which
/// only carries runtime-editable attributes. Most of the configuration keys
/// here are not editable at runtime.
///
/// The common keys (`format`, `enabled`, `tags`, `always_on`, `always_off`,
/// `mixer_type`, `replay_gain_handler`, `filters`) are direct fields.
/// Other key-val pairs are stored verbatim in `OutputConfig::additional_config`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OutputConfig {
    /// MPD output plugin name.
    pub output_type: OutputType,
    /// Unique name of the output, as visible to the client.
    pub name: String,
    /// Fixed sample rate:bits:channels, e.g. `"44100:16:2"`.
    pub format: Option<AudioFormatConfig>,
    /// Whether the output is enabled when MPD starts.
    pub enabled: bool,
    /// Whether metadata tags are sent to this output.
    pub tags: bool,
    /// Try to keep output device "open" by parking them in a "closed" state. Not all output types support this capability.
    pub always_on: bool,
    /// Never use this output for playback even if enabled.
    /// Can be used with the null output (see docs, too lazy to write everything here.)
    pub always_off: bool,
    /// Mixer to use: `"hardware"`, `"software"`, `"null"`, or `"none"`.
    pub mixer_type: MixerType,
    /// ReplayGain handler
    pub replaygain_handler: ReplayGainHandler,
    /// Output-specific configuration items.
    pub additional_config: Vec<(String, String)>,
}

impl Display for OutputConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "audio_output {{")?;
        writeln!(
            f,
            "    type \"{}\"",
            self.output_type.get_serializations()[0]
        )?;
        writeln!(f, "    name \"{}\"", self.name)?;
        if let Some(ref fmt) = self.format {
            writeln!(f, "    format \"{}\"", fmt)?;
        }
        writeln!(
            f,
            "    enabled \"{}\"",
            if self.enabled { "yes" } else { "no" }
        )?;
        writeln!(
            f,
            "    always_on \"{}\"",
            if self.always_on { "yes" } else { "no" }
        )?;
        writeln!(
            f,
            "    always_off \"{}\"",
            if self.always_off { "yes" } else { "no" }
        )?;
        if self.tags {
            writeln!(f, "    tags \"yes\"")?;
        }
        if !matches!(self.mixer_type, MixerType::Default) {
            writeln!(
                f,
                "    mixer_type \"{}\"",
                self.mixer_type.get_serializations()[0]
            )?;
        }
        writeln!(
            f,
            "    replay_gain_handler \"{}\"",
            self.replaygain_handler.get_serializations()[0]
        )?;
        for (k, v) in &self.additional_config {
            writeln!(f, "    {} \"{}\"", k, v)?;
        }
        writeln!(f, "}}")
    }
}

impl TryFrom<&[&str]> for OutputConfig {
    type Error = String;
    fn try_from(lines: &[&str]) -> Result<Self, Self::Error> {
        let mut output = OutputConfig::default();

        for line in lines {
            if let Some((key, val)) = parse_key_value(line) {
                match key {
                    "type" => {
                        output.output_type = OutputType::try_from(val)
                            .map_err(|_| format!("Unknown audio_output type: {}", val))?;
                    }
                    "name" => output.name = val.to_owned(),
                    "format" => output.format = Some(AudioFormatConfig::try_from(val)?),
                    "enabled" => output.enabled = val == "yes" || val == "true" || val == "1",
                    "always_on" => output.always_on = val == "yes" || val == "true" || val == "1",
                    "always_off" => output.always_off = val == "yes" || val == "true" || val == "1",
                    "tags" => output.tags = val == "yes" || val == "true" || val == "1",
                    "mixer_type" => {
                        output.mixer_type = MixerType::try_from(val)
                            .map_err(|_| format!("Unknown mixer_type: {}", val))?;
                    }
                    "replay_gain_handler" => {
                        output.replaygain_handler = ReplayGainHandler::try_from(val)
                            .map_err(|_| format!("Unknown replay_gain_handler: {}", val))?;
                    }
                    other => output
                        .additional_config
                        .push((other.to_string(), val.to_string())),
                }
            }
        }

        Ok(output)
    }
}

#[derive(Default, Debug, Clone)]
pub struct MpdConfig {
    pub music_directory: String, // just one right now
    /// Optional server-side debug logging
    pub log_level: LogLevel,
    pub bind_to_address: Option<String>,
    pub port: Option<u32>,
    pub audio_outputs: Vec<OutputConfig>,
    pub state_file: Option<String>,
    pub sticker_file: Option<String>,
    pub playlist_directory: Option<String>,
    pub db_file: Option<String>,
}

impl MpdConfig {
    pub fn new_minimal() -> Self {
        eprintln!("Generating a default MPD config file...");
        // For now the managed option always uses a socket file for the following reasons:
        // - It does not make sense to let the user pick between socket and TCP here. This server instance
        //   is only used by Euphonica and is turned on and off alongside it, so we only need a loal connection.
        // - Supporting TCP means either letting the user set a bind address and port (no longer user-friendly, and
        //   if they wanted/knew how to do these already, why not just use the "external MPD" option>?), or handling
        //   port collisions by ourselves (takes time to scan/retry).
        // The only benefit supporting TCP here may bring is future Windows compatibility, but Unix sockets are
        // technically supported by Windows too; it's just MPD seemingly refusing to support it there.
        let base_path = get_app_cache_path();
        let mut socket_path = base_path.clone();
        socket_path.push("mpd.socket");
        let mut state_file = base_path.clone();
        state_file.push("mpd.state");
        let mut sticker_file = base_path.clone();
        sticker_file.push("mpd_stickers.db");
        let playlist_directory = get_standalone_playlists_path();
        let mut db_file = base_path.clone();
        db_file.push("mpd.db");
        let mut default_out = OutputConfig::default();
        default_out.name = String::from("PipeWire");
        default_out.enabled = true;

        MpdConfig {
            // No default music directory; in Flatpak the user needs to explicitly select a path for us else we won't
            music_directory: String::from(""),
            bind_to_address: Some(
                socket_path
                    .to_str()
                    .expect("OS does not support UTF-8 paths")
                    .to_owned(),
            ),
            log_level: LogLevel::default(),
            port: Some(6600),
            audio_outputs: vec![default_out],
            state_file: Some(
                state_file
                    .to_str()
                    .expect("OS does not support UTF-8 paths")
                    .to_owned(),
            ),
            sticker_file: Some(
                sticker_file
                    .to_str()
                    .expect("OS does not support UTF-8 paths")
                    .to_owned(),
            ),
            playlist_directory: Some(
                playlist_directory
                    .to_str()
                    .expect("OS does not support UTF-8 paths")
                    .to_owned(),
            ),
            db_file: Some(
                db_file
                    .to_str()
                    .expect("OS does not support UTF-8 paths")
                    .to_owned(),
            ),
        }
    }

    pub fn is_socket_connection(&self) -> bool {
        self.bind_to_address.as_ref().is_some_and(|addr| {
            addr.starts_with("~") || addr.starts_with("/") || addr.starts_with("@")
        })
    }
}

impl Display for MpdConfig {
    /// Note: this will assume some connection defaults, so writing down a default MpdConfig then reading it back up
    /// will not produce the same default config.
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Header
        writeln!(out, "# AUTOGENERATED MPD CONFIGURATION FILE - DO NOT EDIT")?;
        writeln!(
            out,
            "# Generated at {} by Euphonica {}",
            OffsetDateTime::now_local()
                .unwrap_or_else(|_| OffsetDateTime::now_utc())
                .format(&Rfc3339)
                .expect("Timestamp format error"),
            VERSION
        )?;

        writeln!(out, "music_directory \"{}\"", self.music_directory)?;
        writeln!(
            out,
            "bind_to_address \"{}\"",
            self.bind_to_address.as_deref().unwrap_or("localhost")
        )?;
        writeln!(out, "port \"{}\"", self.port.as_ref().unwrap_or(&6600))?;
        if let Some(state_file) = self.state_file.as_deref() {
            writeln!(out, "state_file \"{}\"", state_file)?;
        }
        if let Some(sticker_file) = self.sticker_file.as_deref() {
            writeln!(out, "sticker_file \"{}\"", sticker_file)?;
        }
        if let Some(playlist_directory) = self.playlist_directory.as_deref() {
            writeln!(out, "playlist_directory \"{}\"", playlist_directory)?;
        }
        if let Some(db_file) = self.db_file.as_deref() {
            writeln!(out, "db_file \"{}\"", db_file)?;
        }

        for output in &self.audio_outputs {
            write!(out, "{}", output)?;
        }
        Ok(())
    }
}

impl TryFrom<&str> for MpdConfig {
    type Error = String;
    /// A VERY limited parser, since it's only supposed to parse what WE generated.
    /// It will try its best to gloss over invalid configuration values, falling back
    /// to sensible defaults.
    /// It may still fail against more severe corruptions.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut config = MpdConfig::default();

        let mut in_audio_output = false;
        let mut in_ignored_block = false;
        let mut buf = Vec::new();

        for (raw_line_num, raw_line) in value.lines().enumerate() {
            let line_num = raw_line_num + 1;
            // Strip comments and trim whitespace
            let line = raw_line.split('#').next().unwrap_or("").trim();

            if line.is_empty() {
                continue;
            }

            // Detect nested block error
            if line.ends_with('{') && (in_audio_output || in_ignored_block) {
                return Err(format!(
                    "Syntax error on line {}: nested blocks are not supported",
                    line_num
                ));
            }

            // Closing braces have to be on their own line
            if line == "}" {
                if in_audio_output {
                    in_audio_output = false;
                    config.audio_outputs.push(OutputConfig::try_from(&buf[..])?);
                    buf.clear();
                } else if in_ignored_block {
                    in_ignored_block = false;
                } else {
                    return Err(format!(
                        "Syntax error on line {}: unmatched closing brace '}}'",
                        line_num
                    ));
                }
            } else if in_audio_output {
                // Collect lines until end of block
                buf.push(line);
            } else if in_ignored_block {
                // Ignore contents inside any other block
            } else if line.ends_with('{') {
                // Handle block openings
                if line.starts_with("audio_output") {
                    in_audio_output = true;
                    buf.clear();
                } else {
                    in_ignored_block = true;
                }
            } else if let Some((key, val)) = parse_key_value(line) {
                // Parse top-level key "value" assignments
                match key {
                    "music_directory" => config.music_directory = val.to_owned(),
                    "bind_to_address" => config.bind_to_address = Some(val.to_owned()),
                    "port" => config.port = val.parse::<u32>().ok(),
                    "state_file" => config.state_file = Some(val.to_owned()),
                    "sticker_file" => config.sticker_file = Some(val.to_owned()),
                    "playlist_directory" => config.playlist_directory = Some(val.to_owned()),
                    "db_file" => config.db_file = Some(val.to_owned()),
                    _ => {} // Discard all other top-level keys
                }
            }
        }

        if in_audio_output || in_ignored_block {
            return Err("Syntax error: unclosed block at end of file".to_string());
        }

        Ok(config)
    }
}
