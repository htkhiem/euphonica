/// Config file generator, for use with the managed MPD instance.
/// Since it's only meant for the above case, there is no need to allow configuring things like state file,
/// sticker DB or bind_to_address. These things are always on & fully abstracted away to minimise fuss.
/// In the future we can probably expose this as some sort of "config generator" for user-managed MPD servers too?
/// When that happens the above will need to be implemented properly.
///
/// The format is kinda simple but nonstandard so it's not worth trying to shoehorn Serde here.
use std::fmt::Write;
use strum_macros::{Display, EnumString};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use crate::{config::VERSION, utils::{get_app_cache_path, get_standalone_playlists_path}};

#[derive(Debug, Clone, Copy, PartialEq, Default, Display, EnumString)]
#[non_exhaustive]
pub enum OutputType {
    #[strum(to_string = "httpd")]
    Httpd,
    #[strum(to_string = "alsa")]
    Alsa,
    #[strum(to_string = "pulse")]
    Pulse,
    #[strum(to_string = "pipewire")]
    #[default]
    PipeWire,
}

/// No default value, as what's available depends on the output.
/// ALSA, OSS and Pulse supports hardware mixer and MPD uses that as default.
/// Other outputs use None as default.
/// To leave this to default, simply do not specify in the config file (leave option as None).
#[derive(Debug, Clone, Copy, PartialEq, Display, EnumString)]
#[non_exhaustive]
pub enum MixerType {
    #[strum(to_string = "hardware")]
    Hardware,
    #[strum(to_string = "software")]
    Software,
    #[strum(to_string = "null")]
    Null,
    #[strum(to_string = "none")]
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Display, EnumString)]
#[non_exhaustive]
pub enum ReplayGainHandler {
    #[default]
    #[strum(to_string = "software")]
    Software,
    #[strum(to_string = "mixer")]
    Mixer,
    #[strum(to_string = "none")]
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
    /// Plugin name, e.g. `"httpd"`, `"alsa"`, `"pulse"`, `"pipewire"`.
    pub output_type: OutputType,
    /// Unique name of the output, as visible to the client.
    pub name: String,
    /// Fixed sample rate:bits:channels, e.g. `"44100:16:2"`.
    pub format: Option<String>,
    /// Whether the output is enabled when MPD starts.
    pub enabled: bool,
    /// Whether metadata tags are sent to this output.
    pub tags: bool,
    /// Mixer to use: `"hardware"`, `"software"`, `"null"`, or `"none"`.
    pub mixer_type: Option<MixerType>,
    /// ReplayGain handler
    pub replaygain_handler: ReplayGainHandler,
    /// Output-specific configuration items.
    pub additional_config: Vec<(String, String)>,
}

impl OutputConfig {
    pub fn write_buf(&self, out: &mut String) {
        writeln!(out, "audio_output {{").unwrap();
        writeln!(out, "    type \"{}\"", self.output_type).unwrap();
        writeln!(out, "    name \"{}\"", self.name).unwrap();
        if let Some(ref fmt) = self.format {
            writeln!(out, "    format \"{}\"", fmt).unwrap();
        }
        writeln!(
            out,
            "    enabled \"{}\"",
            if self.enabled { "yes" } else { "no" }
        )
        .unwrap();
        if self.tags {
            writeln!(out, "    tags \"yes\"").unwrap();
        }
        if let Some(ref mixer) = self.mixer_type {
            writeln!(out, "    mixer_type \"{}\"", mixer).unwrap();
        }
        writeln!(
            out,
            "    replay_gain_handler \"{}\"",
            self.replaygain_handler
        )
        .unwrap();
        for (k, v) in &self.additional_config {
            writeln!(out, "    {} \"{}\"", k, v).unwrap();
        }
        writeln!(out, "}}").unwrap();
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
                    "format" => output.format = Some(val.to_owned()),
                    "enabled" => output.enabled = val == "yes" || val == "true" || val == "1",
                    "tags" => output.tags = val == "yes" || val == "true" || val == "1",
                    "mixer_type" => {
                        output.mixer_type = Some(
                            MixerType::try_from(val)
                                .map_err(|_| format!("Unknown mixer_type: {}", val))?,
                        )
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
            bind_to_address: Some(socket_path.to_str().expect("OS does not support UTF-8 paths").to_owned()),
            log_level: LogLevel::default(),
            port: Some(6600),
            audio_outputs: vec![default_out],
            state_file: Some(state_file.to_str().expect("OS does not support UTF-8 paths").to_owned()),
            sticker_file: Some(sticker_file.to_str().expect("OS does not support UTF-8 paths").to_owned()),
            playlist_directory: Some(playlist_directory.to_str().expect("OS does not support UTF-8 paths").to_owned()),
            db_file: Some(db_file.to_str().expect("OS does not support UTF-8 paths").to_owned()),
        }
    }

    pub fn is_socket_connection(&self) -> bool {
        self.bind_to_address.as_ref().is_some_and(|addr| {
            addr.starts_with("~") || addr.starts_with("/") || addr.starts_with("@")
        })
    }

    /// Note: this will assume some connection defaults, so writing down a default MpdConfig then reading it back up
    /// will not produce the same default config.
    pub fn to_string(&self) -> String {
        let mut out = String::new();

        // Header
        writeln!(out, "# AUTOGENERATED MPD CONFIGURATION FILE - DO NOT EDIT").unwrap();
        writeln!(
            out,
            "# Generated at {} by Euphonica {}",
            OffsetDateTime::now_local()
                .unwrap_or_else(|_| OffsetDateTime::now_utc())
                .format(&Rfc3339)
                .expect("Timestamp format error"),
            VERSION
        )
        .unwrap();

        writeln!(out, "music_directory \"{}\"", self.music_directory).unwrap();
        writeln!(out, "bind_to_address \"{}\"", self.bind_to_address.as_deref().unwrap_or("localhost")).unwrap();
        writeln!(out, "port \"{}\"", self.port.as_ref().unwrap_or(&6600)).unwrap();
        if let Some(state_file) = self.state_file.as_deref() {
            writeln!(out, "state_file \"{}\"", state_file).unwrap();
        }
        if let Some(sticker_file) = self.sticker_file.as_deref() {
            writeln!(out, "sticker_file \"{}\"", sticker_file).unwrap();
        }
        if let Some(playlist_directory) = self.playlist_directory.as_deref() {
            writeln!(out, "playlist_directory \"{}\"", playlist_directory).unwrap();
        }
        if let Some(db_file) = self.db_file.as_deref() {
            writeln!(out, "db_file \"{}\"", db_file).unwrap();
        }


        for output in &self.audio_outputs {
            output.write_buf(&mut out);
        }

        out
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
                    "port" => {
                        config.port = val.parse::<u32>().ok()
                    }
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