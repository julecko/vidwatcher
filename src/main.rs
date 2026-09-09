mod config;
mod ffmpeg;
mod logging;
mod scan;
mod state;
mod worker;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;

use config::Config;
use ffmpeg::Av1Encoder;
use state::State;

/// Daemon that watches directories and re-encodes new videos to AV1 + Opus (WebM).
///
/// With no flags it runs forever, rescanning every `scan-interval`. Use `--once`
/// for a single pass (e.g. from cron) and `--check-config` to validate setup.
#[derive(Parser, Debug)]
#[command(version, about)]
struct Args {
    /// Path to the config file (default: $VIDWATCHER_CONFIG,
    /// ~/.config/vidwatcher/config.toml, then /etc/vidwatcher/config.toml).
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Do one scan pass and exit instead of running as a daemon.
    #[arg(long)]
    once: bool,

    /// Load and print the effective configuration, then exit.
    #[arg(long)]
    check_config: bool,

    /// Override the configured log level (error|warn|info|debug|trace).
    #[arg(long)]
    log_level: Option<String>,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            // The logger may not be up yet, so also go straight to stderr.
            eprintln!("vidwatcher: {err:#}");
            log::error!("{err:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let args = Args::parse();
    let (cfg, source) = Config::load(args.config.as_deref())?;

    if args.check_config {
        match &source {
            Some(p) => println!("# loaded from {}", p.display()),
            None => println!("# no config file found; built-in defaults"),
        }
        println!("{cfg:#?}");
        return Ok(());
    }

    let level = args.log_level.as_deref().unwrap_or(&cfg.log.level);
    logging::init(level, cfg.log.file.as_deref())?;

    match &source {
        Some(p) => log::info!("config: {}", p.display()),
        None => log::warn!("no config file found; using built-in defaults"),
    }
    if cfg.watch.is_empty() {
        log::warn!("no directories configured under `watch` - nothing to do");
    }

    ffmpeg::check_tools()?;
    let encoder = Av1Encoder::detect()?;
    log::info!(
        "AV1 encoder {} (crf {}, preset {}, {}-bit, opus {}k)",
        encoder.ffmpeg_name(),
        cfg.encode.crf,
        cfg.encode.preset,
        cfg.encode.bit_depth,
        cfg.encode.audio_bitrate,
    );

    let mut state = State::load(&cfg.state_file)
        .with_context(|| format!("loading state {}", cfg.state_file.display()))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    {
        let shutdown = Arc::clone(&shutdown);
        ctrlc::set_handler(move || {
            if shutdown.swap(true, Ordering::SeqCst) {
                // second signal: give up immediately
                std::process::exit(130);
            }
            eprintln!("vidwatcher: shutdown requested, finishing up...");
        })
        .context("installing signal handler")?;
    }

    if args.once {
        let stats = worker::run_once(&cfg, encoder, &mut state, &shutdown);
        log::info!(
            "pass complete: {} converted, {} kept (not smaller), {} skipped, {} failed",
            stats.converted,
            stats.kept,
            stats.skipped,
            stats.failed
        );
        return Ok(());
    }

    log::info!(
        "daemon started; scanning every {}",
        humantime::format_duration(cfg.scan_interval)
    );
    while !shutdown.load(Ordering::SeqCst) {
        let started = Instant::now();
        let stats = worker::run_once(&cfg, encoder, &mut state, &shutdown);
        if stats.converted + stats.failed + stats.kept > 0 {
            log::info!(
                "pass complete in {:?}: {} converted, {} kept (not smaller), {} skipped, {} failed",
                started.elapsed(),
                stats.converted,
                stats.kept,
                stats.skipped,
                stats.failed
            );
        } else {
            log::debug!("pass complete: nothing to do ({} skipped)", stats.skipped);
        }
        interruptible_sleep(cfg.scan_interval, &shutdown);
    }

    log::info!("vidwatcher stopped");
    Ok(())
}

/// Sleep for `total`, waking early if `shutdown` is set.
fn interruptible_sleep(total: Duration, shutdown: &AtomicBool) {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(500).min(deadline - Instant::now()));
    }
}
