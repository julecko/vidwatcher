//! Configuration file loading and defaults.
//!
//! Config is TOML. It is looked for, in order, at:
//!
//!   1. the path given with `--config`
//!   2. `$VIDWATCHER_CONFIG`
//!   3. `$XDG_CONFIG_HOME/vidwatcher/config.toml` (usually `~/.config/...`)
//!   4. `/etc/vidwatcher/config.toml`
//!
//! The first file that exists is used. If none exist, built-in defaults apply.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;

pub const ENV_CONFIG: &str = "VIDWATCHER_CONFIG";
pub const SYSTEM_CONFIG: &str = "/etc/vidwatcher/config.toml";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Directories to watch. Relative paths are resolved against the daemon's CWD.
    #[serde(default)]
    pub watch: Vec<PathBuf>,

    /// Recurse into sub-directories of each watched directory.
    #[serde(default = "yes")]
    pub recursive: bool,

    /// How long to wait between scans.
    #[serde(default = "default_interval", with = "humantime_serde")]
    pub scan_interval: Duration,

    /// Ignore files modified more recently than this (avoids grabbing partial downloads).
    #[serde(default = "default_min_age", with = "humantime_serde")]
    pub min_file_age: Duration,

    /// Give up on a file after this many failed conversion attempts.
    #[serde(default = "default_attempts")]
    pub max_attempts: u32,

    /// Where the "already processed" database lives.
    #[serde(default = "default_state_file")]
    pub state_file: PathBuf,

    #[serde(default)]
    pub encode: EncodeConfig,

    #[serde(default)]
    pub output: OutputConfig,

    #[serde(default)]
    pub log: LogConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct EncodeConfig {
    /// Constant-quality level, 0-63 (lower = better quality, bigger files).
    #[serde(default = "default_crf")]
    pub crf: u8,
    /// Encoder speed knob: SVT-AV1 `-preset`, libaom `-cpu-used`, rav1e `-speed`.
    #[serde(default = "default_preset")]
    pub preset: i32,
    /// Opus bitrate per audio stream, kbit/s.
    #[serde(default = "default_audio_bitrate")]
    pub audio_bitrate: u32,
    /// 8 (max hardware compatibility) or 10 (smaller at equal quality).
    #[serde(default = "default_bit_depth")]
    pub bit_depth: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct OutputConfig {
    /// `beside` = write next to the source; `directory` = mirror the tree under `directory`.
    #[serde(default)]
    pub mode: OutputMode,
    /// Target root when `mode = "directory"`.
    #[serde(default)]
    pub directory: Option<PathBuf>,
    /// Text inserted into the output file name before `.webm`.
    #[serde(default)]
    pub suffix: String,
    /// Delete the source file after a successful conversion.
    #[serde(default)]
    pub replace: bool,
    /// Skip files whose video stream is already AV1.
    #[serde(default = "yes")]
    pub skip_av1: bool,
    /// Copy the source file's modification and access times onto the output.
    /// (Linux has no way to set a file's *birth* time, so that will be "now".)
    #[serde(default = "yes")]
    pub preserve_timestamps: bool,
    /// Throw away the re-encode and keep the original if the new file would be
    /// bigger than `original_size * max_output_ratio`. `1.0` = only reject a
    /// genuine enlargement; `0.9` = require at least a 10% saving to bother.
    #[serde(default = "default_max_output_ratio")]
    pub max_output_ratio: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum OutputMode {
    #[default]
    Beside,
    Directory,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct LogConfig {
    /// `error`, `warn`, `info`, `debug` or `trace`.
    #[serde(default = "default_log_level")]
    pub level: String,
    /// Also append logs to this file (in addition to stderr / the journal).
    #[serde(default)]
    pub file: Option<PathBuf>,
}

impl Config {
    /// Resolve which config file to read and load it (or return defaults).
    pub fn load(explicit: Option<&Path>) -> Result<(Self, Option<PathBuf>)> {
        let path = Self::resolve_path(explicit);
        match &path {
            Some(p) => {
                let text = std::fs::read_to_string(p)
                    .with_context(|| format!("reading config {}", p.display()))?;
                let cfg: Config = toml::from_str(&text)
                    .with_context(|| format!("parsing config {}", p.display()))?;
                cfg.validate()?;
                Ok((cfg, path))
            }
            None => {
                let cfg: Config = toml::from_str("").expect("empty config uses all defaults");
                Ok((cfg, None))
            }
        }
    }

    fn resolve_path(explicit: Option<&Path>) -> Option<PathBuf> {
        if let Some(p) = explicit {
            return Some(p.to_path_buf());
        }
        if let Some(p) = std::env::var_os(ENV_CONFIG) {
            return Some(PathBuf::from(p));
        }
        if let Some(dir) = dirs::config_dir() {
            let p = dir.join("vidwatcher/config.toml");
            if p.is_file() {
                return Some(p);
            }
        }
        let system = PathBuf::from(SYSTEM_CONFIG);
        system.is_file().then_some(system)
    }

    fn validate(&self) -> Result<()> {
        if self.encode.bit_depth != 8 && self.encode.bit_depth != 10 {
            anyhow::bail!("encode.bit-depth must be 8 or 10");
        }
        if self.encode.crf > 63 {
            anyhow::bail!("encode.crf must be 0-63");
        }
        if self.output.mode == OutputMode::Directory && self.output.directory.is_none() {
            anyhow::bail!("output.directory is required when output.mode = \"directory\"");
        }
        if self.scan_interval.is_zero() {
            anyhow::bail!("scan-interval must be greater than zero");
        }
        let ratio = self.output.max_output_ratio;
        if ratio.is_nan() || ratio <= 0.0 {
            anyhow::bail!("output.max-output-ratio must be greater than zero");
        }
        Ok(())
    }
}

fn yes() -> bool {
    true
}
fn default_interval() -> Duration {
    Duration::from_secs(15 * 60)
}
fn default_min_age() -> Duration {
    Duration::from_secs(60)
}
fn default_attempts() -> u32 {
    3
}
fn default_state_file() -> PathBuf {
    PathBuf::from("/var/lib/vidwatcher/state.json")
}
fn default_crf() -> u8 {
    32
}
fn default_preset() -> i32 {
    6
}
fn default_audio_bitrate() -> u32 {
    128
}
fn default_bit_depth() -> u8 {
    10
}
fn default_max_output_ratio() -> f64 {
    1.0
}
fn default_log_level() -> String {
    "info".to_string()
}

impl Default for EncodeConfig {
    fn default() -> Self {
        Self {
            crf: default_crf(),
            preset: default_preset(),
            audio_bitrate: default_audio_bitrate(),
            bit_depth: default_bit_depth(),
        }
    }
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            mode: OutputMode::Beside,
            directory: None,
            suffix: String::new(),
            replace: false,
            skip_av1: true,
            preserve_timestamps: true,
            max_output_ratio: default_max_output_ratio(),
        }
    }
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            file: None,
        }
    }
}
