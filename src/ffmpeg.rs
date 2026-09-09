//! Thin wrapper around the `ffmpeg` / `ffprobe` command-line tools.

use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::config::EncodeConfig;

/// AV1 encoders we know how to drive, in order of preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Av1Encoder {
    SvtAv1,
    Aom,
    Rav1e,
}

impl Av1Encoder {
    pub fn ffmpeg_name(self) -> &'static str {
        match self {
            Av1Encoder::SvtAv1 => "libsvtav1",
            Av1Encoder::Aom => "libaom-av1",
            Av1Encoder::Rav1e => "librav1e",
        }
    }

    /// Pick the best AV1 encoder compiled into the local ffmpeg.
    pub fn detect() -> Result<Self> {
        let out = Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .context("failed to run `ffmpeg -encoders`")?;
        let text = String::from_utf8_lossy(&out.stdout);
        let has = |name: &str| {
            text.lines()
                .any(|line| line.split_whitespace().nth(1) == Some(name))
        };
        for enc in [Av1Encoder::SvtAv1, Av1Encoder::Aom, Av1Encoder::Rav1e] {
            if has(enc.ffmpeg_name()) {
                return Ok(enc);
            }
        }
        bail!("this ffmpeg has no AV1 encoder (need libsvtav1, libaom-av1 or librav1e)")
    }
}

pub struct EncodeOptions {
    pub encoder: Av1Encoder,
    pub crf: u8,
    pub preset: i32,
    pub audio_bitrate_kbps: u32,
    pub bit_depth: u8,
}

impl EncodeOptions {
    pub fn new(encoder: Av1Encoder, cfg: &EncodeConfig) -> Self {
        Self {
            encoder,
            crf: cfg.crf,
            preset: cfg.preset,
            audio_bitrate_kbps: cfg.audio_bitrate,
            bit_depth: cfg.bit_depth,
        }
    }
}

pub fn check_tools() -> Result<()> {
    for tool in ["ffmpeg", "ffprobe"] {
        Command::new(tool)
            .arg("-version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("`{tool}` was not found on PATH"))?;
    }
    Ok(())
}

/// Codec name of the first "real" video stream, or `None` for audio-only files
/// and files whose only video stream is cover art / a still image.
pub fn video_codec(input: &Path) -> Result<Option<String>> {
    let out = Command::new("ffprobe")
        .args([
            "-v", "error",
            "-select_streams", "v",
            "-show_entries", "stream=codec_name",
            "-of", "csv=p=0",
        ])
        .arg(input)
        .output()
        .context("failed to run ffprobe")?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }

    const IMAGE_CODECS: &[&str] = &["mjpeg", "png", "bmp", "gif", "tiff", "webp", "ppm", "pgm"];
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let codec = line.trim();
        if codec.is_empty() || IMAGE_CODECS.contains(&codec) {
            continue;
        }
        return Ok(Some(codec.to_string()));
    }
    Ok(None)
}

/// Outcome of an [`encode`] call.
pub enum EncodeResult {
    Ok,
    /// The shutdown flag was set; the ffmpeg child was killed and the partial
    /// output removed. Not an error and not recorded as a failure.
    Interrupted,
}

/// Re-encode `input` to AV1 + Opus in a WebM container at `output`.
///
/// Polls `shutdown`; if it flips to `true` the ffmpeg process is killed.
pub fn encode(
    input: &Path,
    output: &Path,
    opts: &EncodeOptions,
    shutdown: &AtomicBool,
) -> Result<EncodeResult> {
    let mut cmd = Command::new("ffmpeg");
    cmd.arg("-hide_banner")
        .args(["-loglevel", "warning", "-nostats"])
        .arg("-y")
        .arg("-i")
        .arg(input);

    // First video stream + every audio stream. Drop subtitles, data and attachments.
    // Stream metadata (including the display-matrix rotation of phone videos) is kept.
    cmd.args(["-map", "0:v:0", "-map", "0:a?", "-sn", "-dn", "-map_chapters", "-1"]);

    let pix_fmt = if opts.bit_depth == 10 {
        "yuv420p10le"
    } else {
        "yuv420p"
    };
    cmd.args(["-pix_fmt", pix_fmt]);

    let crf = opts.crf.to_string();
    let preset = opts.preset.to_string();
    match opts.encoder {
        Av1Encoder::SvtAv1 => {
            cmd.args([
                "-c:v", "libsvtav1",
                "-crf", &crf,
                "-preset", &preset,
                "-svtav1-params", "tune=0",
            ]);
        }
        Av1Encoder::Aom => {
            cmd.args([
                "-c:v", "libaom-av1",
                "-crf", &crf,
                "-b:v", "0", // true constant-quality mode
                "-cpu-used", &preset,
                "-row-mt", "1",
                "-tiles", "2x2",
            ]);
        }
        Av1Encoder::Rav1e => {
            let qp = (i32::from(opts.crf) * 4).clamp(0, 255).to_string();
            cmd.args([
                "-c:v", "librav1e",
                "-qp", &qp,
                "-rav1e-params", &format!("speed={preset}"),
            ]);
        }
    }

    let audio_bitrate = format!("{}k", opts.audio_bitrate_kbps);
    cmd.args(["-c:a", "libopus", "-b:a", &audio_bitrate]);
    cmd.arg(output);
    cmd.stdin(Stdio::null());

    let mut child = cmd.spawn().context("failed to spawn ffmpeg")?;
    loop {
        if shutdown.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(output);
            return Ok(EncodeResult::Interrupted);
        }
        match child.try_wait().context("waiting on ffmpeg")? {
            Some(status) if status.success() => return Ok(EncodeResult::Ok),
            Some(status) => {
                let _ = std::fs::remove_file(output);
                bail!("ffmpeg exited with {status}");
            }
            None => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}
