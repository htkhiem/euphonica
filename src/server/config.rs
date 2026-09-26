use crate::{
    config::VERSION,
    utils::{get_app_cache_path, get_pipewire_devices, get_standalone_playlists_path},
};
#[cfg(target_os = "linux")]
use alsa::{self, device_name::HintIter};
use derivative::Derivative;
use regex::Regex;
use std::ffi::CString;
/// Config file generator, for use with the managed MPD instance.
/// Since it's only meant for the above case, there is no need to allow configuring things like state file,
/// sticker DB or bind_to_address. These things are always on & fully abstracted away to minimise fuss.
/// In the future we can probably expose this as some sort of "config generator" for user-managed MPD servers too?
/// When that happens the above will need to be implemented properly.
///
/// The format is kinda simple but nonstandard so it's not worth trying to shoehorn Serde here.
use std::fmt::{Display, Write};
use strum::{EnumMessage, VariantNames};
use strum_macros::{
    Display, EnumDiscriminants, EnumIter, EnumMessage, EnumString, FromRepr, VariantNames,
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

// Euphonica manages one hidden FIFO output plugin (not exposed to the user) to power the
// spectrum visualiser.
pub static INTERNAL_FIFO_NAME: &'static str = "__euphonica_fifo__";

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
        /// DSD-over-PCM
        bool,
    ),
}

impl AudioFormatConfig {
    pub const DEFAULT: Self = Self::Pcm(PcmSampleRate::P441, PcmBitDepth::I16, None);
    pub fn is_dsd(&self) -> bool {
        match self {
            Self::Pcm(_, _, _) => false,
            Self::Dsd(_, _, _) => true,
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
                let dop = value.ends_with("=dop");
                return Ok(Self::Dsd(mul, channels, dop));
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
            Self::Dsd(mul, ch, dop) => {
                write!(
                    f,
                    "dsd{}:{}{}",
                    mul.get_serializations()[0],
                    ch.map(|ch| ch.to_string()).unwrap_or(String::from("*")),
                    if *dop { "=dop" } else { "" }
                )
            }
            Self::Pcm(rate, bits, ch) => {
                write!(
                    f,
                    "{}:{}:{}",
                    rate.get_serializations()[0],
                    bits.get_serializations()[0],
                    ch.map(|ch| ch.to_string()).unwrap_or(String::from("*"))
                )
            }
        }
    }
}

// EnumDiscriminants is used here to get a parallel data-less enum, for use in downstream widgets
// to determine how to read values of specialised sub-widgets.
// The generated enum by default has "Discriminants" appended to its name.
#[derive(Debug, EnumDiscriminants)]
pub enum ConfigValueType {
    /// Free text. Will use AdwEntryRow. Will default to blank.
    Text,
    /// Use this to show a "Browse" row. Will default to blank.
    Path,
    /// Allow selection from a predefined list of (display, internal) values. Will default to first in list.
    Combo(Vec<(String, String)>),
    /// AdwSpinRow. Parameters are min, max, step size, page size, number of decimal digits to keep in output.
    Number(f64, f64, f64, f64, u8),
    /// AdwSwitchRow. Parameter allows specifying default value.
    Bool(bool),
    /// List of AudioFormat widgets.
    Formats,
}

#[derive(Debug)]
pub struct OutputConfigSpec {
    pub key: &'static str,
    pub title: String,
    pub subtitle: Option<String>,
    pub value_type: ConfigValueType,
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
    #[default]
    Httpd,
    #[cfg(target_os = "linux")]
    #[strum(serialize = "alsa", to_string = "ALSA")]
    Alsa,
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    #[strum(serialize = "pulse", to_string = "PulseAudio")]
    Pulse,
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    #[strum(serialize = "pipewire", to_string = "PipeWire")]
    PipeWire,
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    Oss,
    #[cfg(any(
        target_os = "linux",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    #[strum(serialize = "fifo", to_string = "FIFO")]
    Fifo,
}

impl OutputType {
    /// Return plugin-specific configuration, with valid values initialised with
    /// what the local system currently has.
    pub fn get_custom_config_spec(&self) -> Vec<OutputConfigSpec> {
        match &self {
            #[cfg(target_os = "linux")]
            &Self::Alsa => {
                let mut display_internal_pairs = vec![("Auto".into(), "default".into())];
                // For now only allow selection of playback devices (ALSA calls them "pcm", doesn't mean they only play PCM).
                let i = HintIter::new(None, &*CString::new("pcm").unwrap()).unwrap();
                for a in i.into_iter() {
                    if let (Some(display), Some(internal), Some(dir)) =
                        (a.desc, a.name, a.direction)
                    {
                        if dir == alsa::Direction::Playback {
                            println!("  Display name {}, Internal name {}", &display, &internal);
                            // By default display names are on two lines, both of which are necessary to tell one device from another.
                            display_internal_pairs.push((display, internal));
                        }
                    }
                }
                vec![
                    OutputConfigSpec {
                        key: "device",
                        title: "Override playback device".into(),
                        subtitle: None,
                        value_type: ConfigValueType::Combo(display_internal_pairs),
                    },
                    OutputConfigSpec {
                        key: "auto_resample",
                            title: "Auto resample".into(),
                            subtitle: Some(
                                "If set to no, then libasound will not attempt to resample, \
                            handing the responsibility over to MPD. It is recommended to let MPD \
                            resample (with libsamplerate), because ALSA is quite poor at doing so."
                                    .into(),
                            ),
                            value_type: ConfigValueType::Bool(false),
                        },
                        OutputConfigSpec {
                            key: "auto_channels",
                            title: "Auto channels".into(),
                            subtitle: Some(
                                "If set to no, then libasound will not attempt to convert \
                            between different channel numbers."
                                    .into(),
                            ),
                            value_type: ConfigValueType::Bool(true),
                        },
                        OutputConfigSpec {
                            key: "auto_format",
                            title: "Auto format".into(),
                            subtitle: Some(
                                "If set to no, then libasound will not attempt to convert \
                            between different sample formats (16 bit, 24 bit, floating point, …)."
                                    .into(),
                            ),
                            value_type: ConfigValueType::Bool(true),
                        },
                        OutputConfigSpec {
                            key: "dop",
                            title: "Use DSD-over-PCM (DoP)".into(),
                            subtitle: Some(
                                "This wraps DSD samples in fake 24 bit PCM, and is \
                            understood by some DSD capable products, but may be harmful to \
                            other hardware. Therefore, the default is no and you can enable \
                            the option at your own risk."
                                    .into(),
                            ),
                            value_type: ConfigValueType::Bool(false),
                        },
                        OutputConfigSpec {
                            key: "stop_dsd_silence",
                            title: "Stop DSD silence".into(),
                            subtitle: Some(
                                "If enabled, silence is played before manually stopping \
                                playback (“stop” or “pause”) in DSD mode (native \
                                DSD or DoP). This is a workaround for some DACs \
                                which emit noise when stopping DSD playback."
                                    .into(),
                            ),
                            value_type: ConfigValueType::Bool(false),
                        },
                        OutputConfigSpec {
                            key: "allowed_formats",
                            title: "Allowed formats".into(),
                            subtitle: Some(
                                "Additionally specify a list of audio formats understood \
                                by the device here. ALSA will try to pick the one closest \
                                to the material being played."
                                    .into(),
                            ),
                            value_type: ConfigValueType::Formats,
                        }
                ]
            }
            #[cfg(any(
                target_os = "linux",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            OutputType::Fifo => vec![
            OutputConfigSpec {
                    key: "path",
                    title: "FIFO file path".into(),
                    subtitle: None,
                    value_type: ConfigValueType::Path,
                }
            ],
            OutputType::Httpd => vec![
                OutputConfigSpec {
                    key: "bind_to_address",
                    title: "Bind to address".into(),
                    subtitle: None,
                    value_type: ConfigValueType::Text,
                },
                OutputConfigSpec {
                    key: "port",
                    title: "Port".into(),
                    subtitle: None,
                    value_type: ConfigValueType::Text,
                },
                OutputConfigSpec {
                    key: "dscp_class",
                    title: "DSCP class".into(),
                    subtitle: Some("Differentiated Services Code Point class for outgoing traffic. CS3 is recommended.".into()),
                    // Put CS3 as default (top). Only expose CS levels.
                    value_type: ConfigValueType::Combo(vec![
                        ("Broadcast video (CS3)".into(), "CS3".into()),
                        ("Standard (CS0)".into(), "CS0".into()),
                        ("Low-priority (CS1)".into(), "CS1".into()),
                        ("OAM (CS2)".into(), "CS2".into()),
                        ("Real-time interactive (CS4)".into(), "CS4".into()),
                        ("Signalling (CS5)".into(), "CS5".into()),
                        ("Network control (CS6)".into(), "CS6".into()),
                    ]),
                },
                OutputConfigSpec {
                    key: "max_clients",
                    title: "Maximum concurrent clients".into(),
                    subtitle: Some("When set to 0 no limit will apply.".into()),
                    // Put CS3 as default (top). Only expose CS levels.
                    value_type: ConfigValueType::Number(0.0, 128.0, 1.0, 5.0, 0),
                },
                OutputConfigSpec {
                    key: "genre",
                    title: "Stream genre".into(),
                    subtitle: Some("Will be reflected in the icy-genre header of the stream.".into()),
                    value_type: ConfigValueType::Text,
                },
                OutputConfigSpec {
                    key: "website",
                    title: "Stream website".into(),
                    subtitle: Some("Will be reflected in the icy-website header of the stream.".into()),
                    value_type: ConfigValueType::Text,
                },
            ],
            #[cfg(any(
                target_os = "linux",  // not recommended tho
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            OutputType::Oss => {
                vec![
                    OutputConfigSpec {
                        key: "device",
                        title: "Override device path".into(),
                        subtitle: None,
                        value_type: ConfigValueType::Text
                    },
                    OutputConfigSpec {
                        key: "dop",
                        title: "Use DSD-over-PCM (DoP)".into(),
                        subtitle: Some(
                            "This wraps DSD samples in fake 24 bit PCM, and is \
                        understood by some DSD capable products, but may be harmful to \
                        other hardware. Therefore, the default is no and you can enable \
                        the option at your own risk."
                                .into(),
                        ),
                        value_type: ConfigValueType::Bool(false),
                    }
                ]
            }
            #[cfg(any(
                target_os = "linux",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            OutputType::PipeWire => {
                // FIXME: BLOCKING LOGIC
                let display_and_node_names = get_pipewire_devices(false);
                vec![
                    OutputConfigSpec {
                        key: "target",
                        title: "Override device".into(),
                        subtitle: Some("If not specified, let the PipeWire manager select a target.".into()),
                        value_type: ConfigValueType::Combo(display_and_node_names)
                    },
                    OutputConfigSpec {
                        key: "remote",
                        title: "Override remote name".into(),
                        subtitle: None,
                        value_type: ConfigValueType::Text
                    },
                    OutputConfigSpec {
                        key: "dsd",
                        title: "Enable DSD playback".into(),
                        subtitle: Some(
                            "Requires PipeWire 0.38 and up.".into(),
                        ),
                        value_type: ConfigValueType::Bool(false),
                    },
                ]
            }
            #[cfg(any(
                target_os = "linux",
                target_os = "freebsd",
                target_os = "openbsd",
                target_os = "netbsd",
                target_os = "dragonfly"
            ))]
            OutputType::Pulse => {
                vec![
                    OutputConfigSpec {
                        key: "server",
                        title: "Override server hostname".into(),
                        subtitle: None,
                        value_type: ConfigValueType::Text
                    },
                    // Too lazy to implement auto sink names fetching here.
                    // Most people use PipeWire these days anyway.
                    OutputConfigSpec {
                        key: "sink",
                        title: "Override sink".into(),
                        subtitle: None,
                        value_type: ConfigValueType::Text
                    },
                    // Too lazy to implement auto sink names fetching here.
                    // Most people use PipeWire these days anyway.
                    OutputConfigSpec {
                        key: "media_role",
                        title: "Media role".into(),
                        subtitle: Some("Specify what media role MPD should report to PulseAudio.".into()),
                        value_type: ConfigValueType::Combo(vec![
                            ("video".into(), "video".into()),
                            ("music".into(), "music".into()),
                            ("game".into(), "game".into()),
                            ("event".into(), "event".into()),
                            ("phone".into(), "phone".into()),
                            ("animation".into(), "animation".into()),
                            ("production".into(), "production".into()),
                            ("a11y".into(), "a11y".into()),
                        ])
                    },
                    OutputConfigSpec {
                        key: "scale_volume",
                        title: "Scale volume".into(),
                        subtitle: Some("Specifies a linear scaling coefficient to apply when adjusting \
                        volume through MPD. For example, chosing 0.7 means that setting the volume to \
                        100 in MPD will set the PulseAudio volume to 70%.".into()),
                        value_type: ConfigValueType::Number(0.5, 5.0, 0.05, 0.1, 2)
                    }
                ]
            }
        }
    }
}

/// ALSA, OSS and Pulse supports hardware mixer and MPD uses that as default.
/// Other outputs use None as default.
/// To leave this to default, simply do not specify in the config file (leave option as None).
#[derive(
    Debug, Clone, Copy, PartialEq, Display, EnumString, VariantNames, Default, EnumMessage, FromRepr,
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
    Debug, Clone, Copy, PartialEq, Default, Display, EnumString, VariantNames, EnumMessage, FromRepr,
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
#[derive(Clone, Debug, Derivative, PartialEq)]
#[derivative(Default)]
pub struct OutputConfig {
    /// MPD output plugin name.
    pub output_type: OutputType,
    /// Unique name of the output, as visible to the client.
    pub name: String,
    /// Fixed sample rate:bits:channels, e.g. `"44100:16:2"`.
    pub format: Option<AudioFormatConfig>,
    /// Whether the output is enabled when MPD starts.
    #[derivative(Default(value = "true"))]
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

impl OutputConfig {
    pub fn internal_fifo() -> Self {
        let mut fifo_path = get_app_cache_path();
        fifo_path.push("euphonica_internal.fifo");
        Self {
            output_type: OutputType::Fifo,
            name: INTERNAL_FIFO_NAME.to_string(),
            format: Some(AudioFormatConfig::Pcm(
                PcmSampleRate::P441,
                PcmBitDepth::I16,
                Some(2),
            )),
            enabled: true,
            tags: false,
            always_on: false,
            always_off: false,
            mixer_type: MixerType::None,
            replaygain_handler: ReplayGainHandler::None,
            additional_config: vec![(
                "path".to_string(),
                fifo_path
                    .to_str()
                    .expect("OS does not support Unicode")
                    .to_string(),
            )],
        }
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
