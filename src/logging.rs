//! Logging setup: always to stderr (captured by systemd / the journal),
//! optionally also appended to a file.

use std::fs::OpenOptions;
use std::path::Path;

use anyhow::{Context, Result};
use log::LevelFilter;
use simplelog::{
    ColorChoice, CombinedLogger, Config as LogFormat, ConfigBuilder, SharedLogger, TermLogger,
    TerminalMode, WriteLogger,
};

pub fn init(level: &str, file: Option<&Path>) -> Result<()> {
    let level: LevelFilter = level
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid log level {level:?}"))?;

    let fmt: LogFormat = ConfigBuilder::new()
        .set_time_format_rfc3339()
        .set_target_level(LevelFilter::Error)
        .build();

    let mut loggers: Vec<Box<dyn SharedLogger>> = vec![TermLogger::new(
        level,
        fmt.clone(),
        TerminalMode::Stderr,
        ColorChoice::Auto,
    )];

    if let Some(path) = file {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating log dir {}", parent.display()))?;
        }
        let handle = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("opening log file {}", path.display()))?;
        loggers.push(WriteLogger::new(level, fmt, handle));
    }

    CombinedLogger::init(loggers).context("initialising logger")?;
    Ok(())
}
