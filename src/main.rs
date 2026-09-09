mod ffmpeg;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;
use walkdir::WalkDir;

use ffmpeg::{Av1Encoder, EncodeOptions};

/// Extensions we treat as candidate video files (ffprobe confirms each one).
const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "f4v", "mpg", "mpeg", "m2v", "ts",
    "m2ts", "mts", "vob", "ogv", "ogm", "3gp", "3g2", "divx", "mxf", "asf", "rm", "rmvb", "y4m",
];

/// Recursively convert every video in a directory to AV1 video + Opus audio,
/// written as WebM (a good fit for AV1/Opus and well supported on Android).
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Directory to scan recursively for video files.
    dir: PathBuf,

    /// Constant-quality level, 0-63 (lower = better quality and bigger files).
    #[arg(long, default_value_t = 32)]
    crf: u8,

    /// Encoder speed/effort knob (higher = faster, slightly bigger).
    /// Interpreted per encoder: SVT-AV1 -preset, libaom -cpu-used, rav1e -speed.
    #[arg(long, default_value_t = 6)]
    preset: i32,

    /// Opus bitrate per audio stream, in kbit/s.
    #[arg(long, default_value_t = 128)]
    audio_bitrate: u32,

    /// Video bit depth: 8 (max hardware compatibility) or 10 (smaller at equal quality).
    #[arg(long, default_value_t = 10)]
    bit_depth: u8,

    /// Write outputs into this directory, mirroring the input tree,
    /// instead of next to each source file.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Text inserted into the output file name before `.webm`.
    #[arg(long, default_value = "")]
    suffix: String,

    /// Delete each source file after it converts successfully.
    #[arg(long)]
    replace: bool,

    /// Re-encode even if an output file already exists (default: skip it).
    #[arg(long)]
    overwrite: bool,

    /// Skip files whose video stream is already AV1.
    #[arg(long)]
    skip_av1: bool,

    /// Show what would happen without running ffmpeg.
    #[arg(long)]
    dry_run: bool,
}

fn main() -> ExitCode {
    match run() {
        Ok(0) => ExitCode::SUCCESS,
        Ok(failed) => {
            eprintln!("{failed} file(s) failed");
            ExitCode::FAILURE
        }
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<u64> {
    let args = Args::parse();

    if args.bit_depth != 8 && args.bit_depth != 10 {
        bail!("--bit-depth must be 8 or 10");
    }
    let meta = std::fs::metadata(&args.dir)
        .with_context(|| format!("cannot access {}", args.dir.display()))?;
    if !meta.is_dir() {
        bail!("{} is not a directory", args.dir.display());
    }

    ffmpeg::check_tools()?;
    let encoder = Av1Encoder::detect()?;
    eprintln!(
        "AV1 encoder: {}  (crf {}, preset {}, {}-bit, opus {}k)",
        encoder.ffmpeg_name(),
        args.crf,
        args.preset,
        args.bit_depth,
        args.audio_bitrate,
    );

    let opts = EncodeOptions {
        encoder,
        crf: args.crf,
        preset: args.preset,
        audio_bitrate_kbps: args.audio_bitrate,
        bit_depth: args.bit_depth,
    };

    let mut videos: Vec<PathBuf> = WalkDir::new(&args.dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && has_video_extension(e.path()))
        .map(|e| e.into_path())
        .collect();
    videos.sort();

    if videos.is_empty() {
        eprintln!("No video files found under {}", args.dir.display());
        return Ok(0);
    }
    eprintln!("Found {} candidate file(s)\n", videos.len());

    let total = videos.len();
    let (mut converted, mut skipped, mut failed) = (0u64, 0u64, 0u64);

    for (i, input) in videos.iter().enumerate() {
        eprintln!("[{}/{total}] {}", i + 1, input.display());

        match ffmpeg::video_codec(input) {
            Ok(None) => {
                eprintln!("  skip: no video stream");
                skipped += 1;
                continue;
            }
            Ok(Some(codec)) if args.skip_av1 && codec == "av1" => {
                eprintln!("  skip: already AV1");
                skipped += 1;
                continue;
            }
            Ok(Some(_)) => {}
            Err(err) => {
                eprintln!("  FAILED: probe: {err:#}");
                failed += 1;
                continue;
            }
        }

        let output = match output_path(input, &args) {
            Some(p) => p,
            None => {
                eprintln!("  skip: cannot form an output path");
                skipped += 1;
                continue;
            }
        };

        if !args.overwrite && output.exists() {
            eprintln!("  skip: output exists ({})", output.display());
            skipped += 1;
            continue;
        }

        if args.dry_run {
            eprintln!("  would write {}", output.display());
            continue;
        }

        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("cannot create {}", parent.display()))?;
        }

        match ffmpeg::encode(input, &output, &opts) {
            Ok(()) => {
                report_sizes(input, &output);
                converted += 1;
                if args.replace {
                    match std::fs::remove_file(input) {
                        Ok(()) => eprintln!("  deleted source"),
                        Err(e) => eprintln!("  warning: could not delete source: {e}"),
                    }
                }
            }
            Err(err) => {
                eprintln!("  FAILED: {err:#}");
                let _ = std::fs::remove_file(&output);
                failed += 1;
            }
        }
        eprintln!();
    }

    eprintln!("Done: {converted} converted, {skipped} skipped, {failed} failed");
    Ok(failed)
}

fn has_video_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}

/// Build the `.webm` output path, never returning a path equal to `input`.
fn output_path(input: &Path, args: &Args) -> Option<PathBuf> {
    let stem = input.file_stem()?.to_str()?;

    let dir = match &args.output_dir {
        Some(out) => match input.strip_prefix(&args.dir).ok()?.parent() {
            Some(rel) => out.join(rel),
            None => out.clone(),
        },
        None => input.parent()?.to_path_buf(),
    };

    let mut candidate = dir.join(format!("{stem}{}.webm", args.suffix));
    if same_path(&candidate, input) {
        candidate = dir.join(format!("{stem}{}.av1.webm", args.suffix));
    }
    Some(candidate)
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

fn report_sizes(input: &Path, output: &Path) {
    let (Ok(src), Ok(dst)) = (std::fs::metadata(input), std::fs::metadata(output)) else {
        return;
    };
    let (src, dst) = (src.len(), dst.len());
    let pct = if src > 0 {
        100.0 * dst as f64 / src as f64
    } else {
        0.0
    };
    eprintln!(
        "  {} -> {} ({pct:.0}% of original)",
        human(src),
        human(dst)
    );
}

fn human(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}
