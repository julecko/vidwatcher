//! Persistent record of which files have already been handled, so the daemon
//! does not re-convert the same video on every scan.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Identity of a file on disk. If either field changes we treat it as a new file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileId {
    pub size: u64,
    pub mtime: u64,
}

impl FileId {
    pub fn of(meta: &std::fs::Metadata) -> Self {
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        Self {
            size: meta.len(),
            mtime,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Done { output: PathBuf, at: u64 },
    Failed { attempts: u32, error: String, at: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    #[serde(flatten)]
    pub id: FileId,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct State {
    #[serde(default)]
    entries: BTreeMap<String, Entry>,
    #[serde(skip)]
    path: PathBuf,
    #[serde(skip)]
    dirty: bool,
}

impl State {
    pub fn load(path: &Path) -> Result<Self> {
        let mut state = match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<State>(&text)
                .with_context(|| format!("parsing state file {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        state.path = path.to_path_buf();
        Ok(state)
    }

    fn key(path: &Path) -> String {
        std::fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .to_string_lossy()
            .into_owned()
    }

    /// Should this file be (re)processed given its current on-disk identity?
    pub fn needs_work(&self, path: &Path, id: FileId, max_attempts: u32) -> bool {
        match self.entries.get(&Self::key(path)) {
            None => true,
            Some(e) if e.id != id => true, // file changed since we last saw it
            Some(e) => match &e.outcome {
                Outcome::Done { .. } => false,
                Outcome::Failed { attempts, .. } => *attempts < max_attempts,
            },
        }
    }

    pub fn record_done(&mut self, path: &Path, id: FileId, output: &Path) {
        self.entries.insert(
            Self::key(path),
            Entry {
                id,
                outcome: Outcome::Done {
                    output: output.to_path_buf(),
                    at: now(),
                },
            },
        );
        self.dirty = true;
    }

    pub fn record_failure(&mut self, path: &Path, id: FileId, error: &str) {
        let key = Self::key(path);
        let attempts = match self.entries.get(&key) {
            Some(Entry {
                outcome: Outcome::Failed { attempts, .. },
                id: prev,
                ..
            }) if *prev == id => attempts + 1,
            _ => 1,
        };
        self.entries.insert(
            key,
            Entry {
                id,
                outcome: Outcome::Failed {
                    attempts,
                    error: error.to_string(),
                    at: now(),
                },
            },
        );
        self.dirty = true;
    }

    /// Drop entries whose source file no longer exists. Returns how many were removed.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|k, _| Path::new(k).exists());
        let removed = before - self.entries.len();
        if removed > 0 {
            self.dirty = true;
        }
        removed
    }

    pub fn save_if_dirty(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)
            .with_context(|| format!("writing {}", tmp.display()))?;
        // Force world-readable perms so a restrictive root umask (e.g. 077) during
        // a manual `--once` run can't leave a state file the service user can't read.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644))
                .with_context(|| format!("chmod {}", tmp.display()))?;
        }
        std::fs::rename(&tmp, &self.path)
            .with_context(|| format!("replacing {}", self.path.display()))?;
        self.dirty = false;
        Ok(())
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
