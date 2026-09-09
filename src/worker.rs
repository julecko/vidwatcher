//! One scan pass: walk every watched directory and convert what needs converting.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::config::Config;
use crate::ffmpeg::{self, Av1Encoder, EncodeOptions, EncodeResult};
use crate::scan;
use crate::state::{FileId, State};

pub struct ScanStats {
    pub converted: u64,
    pub skipped: u64,
    pub failed: u64,
}

/// Run a single pass over all watched directories.
pub fn run_once(
    cfg: &Config,
    encoder: Av1Encoder,
    state: &mut State,
    shutdown: &AtomicBool,
) -> ScanStats {
    let opts = EncodeOptions::new(encoder, &cfg.encode);
    let mut stats = ScanStats {
        converted: 0,
        skipped: 0,
        failed: 0,
    };

    for root in &cfg.watch {
        if shutdown.load(Ordering::Relaxed) {
            break;
        }
        if !root.is_dir() {
            log::warn!("watch path is not a directory, skipping: {}", root.display());
            continue;
        }

        let files = scan::candidates(root, cfg.recursive);
        log::debug!("{}: {} candidate file(s)", root.display(), files.len());

        for input in files {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }
            match handle_file(&input, root, cfg, &opts, state, shutdown) {
                FileResult::Converted => stats.converted += 1,
                FileResult::Skipped => stats.skipped += 1,
                FileResult::Failed => stats.failed += 1,
                FileResult::Interrupted => return stats,
            }
            if let Err(e) = state.save_if_dirty() {
                log::error!("could not save state: {e:#}");
            }
        }
    }

    match state.prune_missing() {
        0 => {}
        n => log::debug!("pruned {n} stale state entries"),
    }
    if let Err(e) = state.save_if_dirty() {
        log::error!("could not save state: {e:#}");
    }
    stats
}

enum FileResult {
    Converted,
    Skipped,
    Failed,
    Interrupted,
}

fn handle_file(
    input: &Path,
    watch_root: &Path,
    cfg: &Config,
    opts: &EncodeOptions,
    state: &mut State,
    shutdown: &AtomicBool,
) -> FileResult {
    let meta = match std::fs::metadata(input) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("stat {}: {e}", input.display());
            return FileResult::Skipped;
        }
    };
    let id = FileId::of(&meta);

    // Skip files that are still being written / freshly dropped in.
    if let Ok(age) = SystemTime::now().duration_since(meta.modified().unwrap_or(SystemTime::UNIX_EPOCH))
        && age < cfg.min_file_age
    {
        log::debug!("too fresh ({}s old): {}", age.as_secs(), input.display());
        return FileResult::Skipped;
    }

    if !state.needs_work(input, id, cfg.max_attempts) {
        return FileResult::Skipped;
    }

    // Confirm it is really a video, and honour skip-av1.
    match ffmpeg::video_codec(input) {
        Ok(None) => {
            log::debug!("no video stream: {}", input.display());
            state.record_done(input, id, input); // remember so we don't re-probe forever
            return FileResult::Skipped;
        }
        Ok(Some(codec)) if cfg.output.skip_av1 && codec == "av1" => {
            log::info!("already AV1, skipping: {}", input.display());
            state.record_done(input, id, input);
            return FileResult::Skipped;
        }
        Ok(Some(_)) => {}
        Err(e) => {
            log::warn!("probe {}: {e:#}", input.display());
            state.record_failure(input, id, &format!("probe: {e}"));
            return FileResult::Failed;
        }
    }

    let output = match scan::output_path(input, watch_root, cfg) {
        Some(p) => p,
        None => {
            log::warn!("cannot form output path for {}", input.display());
            return FileResult::Skipped;
        }
    };

    if output.exists() {
        log::info!(
            "output already exists, recording as done: {}",
            output.display()
        );
        state.record_done(input, id, &output);
        return FileResult::Skipped;
    }
    if let Some(parent) = output.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        log::error!("create {}: {e}", parent.display());
        state.record_failure(input, id, &format!("mkdir: {e}"));
        return FileResult::Failed;
    }

    log::info!("converting {} -> {}", input.display(), output.display());
    let started = SystemTime::now();
    match ffmpeg::encode(input, &output, opts, shutdown) {
        Ok(EncodeResult::Ok) => {
            log::info!(
                "done in {}: {}{}",
                fmt_duration(started.elapsed().unwrap_or_default()),
                output.display(),
                size_delta(input, &output),
            );
            if cfg.output.preserve_timestamps
                && let Err(e) = copy_timestamps(input, &output)
            {
                log::warn!("could not copy timestamps to {}: {e}", output.display());
            }
            state.record_done(input, id, &output);
            if cfg.output.replace {
                match std::fs::remove_file(input) {
                    Ok(()) => log::info!("deleted source {}", input.display()),
                    Err(e) => log::warn!("could not delete source {}: {e}", input.display()),
                }
            }
            FileResult::Converted
        }
        Ok(EncodeResult::Interrupted) => {
            log::info!("interrupted mid-encode: {}", input.display());
            FileResult::Interrupted
        }
        Err(e) => {
            log::error!("ffmpeg failed for {}: {e:#}", input.display());
            state.record_failure(input, id, &e.to_string());
            FileResult::Failed
        }
    }
}

/// Copy the source's modification (and, where available, access) time onto the
/// output. Linux exposes no syscall to set a file's birth/creation time, so that
/// one necessarily stays as the moment ffmpeg wrote the file.
fn copy_timestamps(from: &Path, to: &Path) -> std::io::Result<()> {
    let src = std::fs::metadata(from)?;
    let mut times = std::fs::FileTimes::new().set_modified(src.modified()?);
    if let Ok(accessed) = src.accessed() {
        times = times.set_accessed(accessed);
    }
    std::fs::File::options().write(true).open(to)?.set_times(times)
}

fn size_delta(input: &Path, output: &Path) -> String {
    let (Ok(a), Ok(b)) = (std::fs::metadata(input), std::fs::metadata(output)) else {
        return String::new();
    };
    let (a, b) = (a.len(), b.len());
    if a == 0 {
        return String::new();
    }
    format!(
        " ({} -> {}, {:.0}% of original)",
        human(a),
        human(b),
        100.0 * b as f64 / a as f64
    )
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

fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 60 {
        format!("{}m{:02}s", s / 60, s % 60)
    } else {
        format!("{s}s")
    }
}
