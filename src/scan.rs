//! Walking watched directories and deciding the output path for each video.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::config::{Config, OutputMode};

/// Extensions we treat as candidate video files (ffprobe confirms each one later).
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "m4v", "mov", "mkv", "webm", "avi", "wmv", "flv", "f4v", "mpg", "mpeg", "m2v", "ts",
    "m2ts", "mts", "vob", "ogv", "ogm", "3gp", "3g2", "divx", "mxf", "asf", "rm", "rmvb", "y4m",
];

pub fn has_video_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.as_str()))
}

/// All candidate files under `root`, sorted for stable ordering.
pub fn candidates(root: &Path, recursive: bool) -> Vec<PathBuf> {
    let max_depth = if recursive { usize::MAX } else { 1 };
    let mut files: Vec<PathBuf> = WalkDir::new(root)
        .max_depth(max_depth)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| match e {
            Ok(e) => Some(e),
            Err(err) => {
                log::warn!("walk: {err}");
                None
            }
        })
        .filter(|e| e.file_type().is_file() && has_video_extension(e.path()))
        .map(walkdir::DirEntry::into_path)
        .collect();
    files.sort();
    files
}

/// Compute the `.webm` output path for `input`. Never returns a path equal to `input`.
pub fn output_path(input: &Path, watch_root: &Path, cfg: &Config) -> Option<PathBuf> {
    let stem = input.file_stem()?.to_str()?;
    let suffix = &cfg.output.suffix;

    let dir = match cfg.output.mode {
        OutputMode::Beside => input.parent()?.to_path_buf(),
        OutputMode::Directory => {
            let base = cfg.output.directory.as_ref()?;
            match input.strip_prefix(watch_root).ok().and_then(Path::parent) {
                Some(rel) => base.join(rel),
                None => base.clone(),
            }
        }
    };

    let mut candidate = dir.join(format!("{stem}{suffix}.webm"));
    if same_path(&candidate, input) {
        candidate = dir.join(format!("{stem}{suffix}.av1.webm"));
    }
    Some(candidate)
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}
