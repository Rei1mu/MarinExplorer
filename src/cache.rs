//! On-disk persistence for computed folder sizes and app metadata.
//!
//! Layout (one text file, tab-separated, so no JSON dependency):
//!   `S<TAB><size><TAB><dir_mtime><TAB><percent-encoded-path>`  folder sizes
//!   `M<TAB><key><TAB><percent-encoded-value>`                  metadata
//!
//! Folder sizes are invalidated by the folder's own mtime (which changes when
//! entries are added/removed), so a stale value is recomputed in the background
//! rather than shown wrong.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::fs_util::{percent_decode, percent_encode};

pub const CACHE_SUBDIR: &str = "MarinExplorer";
pub const STORE_FILE: &str = "store.txt";

/// How long a computed size is good enough to keep showing without rescanning.
///
/// Scanning is the expensive part (every file has to be visited), while the
/// *purpose* is only "roughly how big is this folder". So a value stays valid
/// for a while even after the folder changed — a few MB of drift in a 40 GB
/// folder is invisible, and rescanning it constantly is not. Bigger folders
/// get a longer grace period: they cost more to scan and any absolute change
/// is relatively smaller.
pub fn size_ttl_secs(size: u64) -> u64 {
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * MB;
    const DAY: u64 = 24 * 3600;
    match size {
        s if s >= 4 * GB => 14 * DAY,
        s if s >= GB => 7 * DAY,
        s if s >= 128 * MB => 3 * DAY,
        _ => DAY,
    }
}

/// How long a *budget-stopped* size stays on screen.
///
/// Such a value is a lower bound that can miss entire sub-trees, so it is not
/// in the same class as an exact number: it is re-checked after an hour
/// instead of after days. Without this the "~" number survives its own
/// staleness — the parent shows 200 MB while a child inside it is 400 MB.
const EST_TTL_SECS: u64 = 3600;

/// Wall-clock seconds, 0 if the clock is somehow before the epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Where the store lives.
///
/// Preferred location is **next to the running executable** (`<exe dir>/
/// MarinExplorer/store.txt`), so the app stays self-contained: nothing is
/// scattered into the user profile, and installing to another drive moves
/// everything with it.
///
/// If that directory is not writable — a machine-wide install under Program
/// Files, typically — we fall back to the per-user data directory.
pub fn cache_dir() -> PathBuf {
    if let Some(exe) = crate::fs_util::exe_dir() {
        let candidate = exe.join(CACHE_SUBDIR);
        if is_writable(&candidate) {
            return candidate;
        }
    }
    let base = std::env::var("LOCALAPPDATA")
        .or_else(|_| std::env::var("APPDATA"))
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join(CACHE_SUBDIR)
}

/// True when `dir` exists (or can be created) and accepts writes. Probing once
/// at startup is cheap; assuming write access and failing later is not.
fn is_writable(dir: &Path) -> bool {
    if fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".w");
    let ok = fs::write(&probe, b"").is_ok();
    let _ = fs::remove_file(&probe);
    ok
}

pub struct Store {
    /// folder path -> (folder mtime, recursive size, computed_at, complete)
    /// `computed_at` is unix seconds and drives the TTL below: a value that is
    /// still fresh is reused even when the folder changed, which is what keeps
    /// a busy folder (Downloads, caches, AppData) off the scanner.
    /// `complete` is false for a budget-stopped scan: the size is a lower
    /// bound and must keep its `~` even after a round-trip through disk.
    pub size_map: HashMap<PathBuf, (u64, u64, u64, bool)>,
    /// arbitrary app metadata: sid, cwd_left, cwd_right, ...
    pub meta: HashMap<String, String>,
    pub file: PathBuf,
    pub dirty: bool,
}

impl Store {
    pub fn load() -> Self {
        let dir = cache_dir();
        let file = dir.join(STORE_FILE);
        let mut s = Self {
            size_map: HashMap::new(),
            meta: HashMap::new(),
            file: file.clone(),
            dirty: false,
        };
        if let Ok(text) = fs::read_to_string(&file) {
            for line in text.lines() {
                let mut f = line.split('\t');
                match f.next() {
                    // `S` = exact scan, `E` = budget-stopped estimate. Same
                    // field layout; the tag carries the `~` across restarts.
                    // Older stores only ever wrote `S`.
                    Some(tag) if tag == "S" || tag == "E" => {
                        let complete = tag == "S";
                        let size = f.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                        let mtime = f.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                        // New layout carries a fourth numeric column
                        // (`computed_at`). Older stores only have the path
                        // here, so a field that does not parse as a number has
                        // to be the path — and the value counts as expired.
                        let next = f.next().unwrap_or_default();
                        let (computed_at, path_str) = match next.parse::<u64>() {
                            Ok(at) => (at, f.next().unwrap_or_default()),
                            Err(_) => (0, next),
                        };
                        let path = PathBuf::from(percent_decode(path_str));
                        if !path.as_os_str().is_empty() {
                            s.size_map
                                .insert(path, (mtime, size, computed_at, complete));
                        }
                    }
                    Some("M") => {
                        let key = f.next().unwrap_or_default().to_string();
                        let val = f
                            .next()
                            .map(percent_decode)
                            .unwrap_or_default();
                        if !key.is_empty() {
                            s.meta.insert(key, val);
                        }
                    }
                    _ => {}
                }
            }
        }
        s
    }

    /// A size that is good enough to show right now without rescanning.
    ///
    /// Reused when either the folder is unchanged (mtime match — the value is
    /// exact) or the value is still inside its TTL (the folder changed, but a
    /// few hours of drift is within the point of "how big is this folder").
    ///
    /// Returns `(size, complete)`: `complete == false` marks a budget-stopped
    /// lower bound, which the UI renders with a `~`. Estimates expire much
    /// sooner than exact values (see `EST_TTL_SECS`).
    pub fn size_for_lenient(&self, path: &Path, dir_mtime: u64, now: u64) -> Option<(u64, bool)> {
        let (m, sz, at, complete) = self.size_map.get(path)?;
        if *m == dir_mtime {
            return Some((*sz, *complete));
        }
        let ttl = if *complete {
            size_ttl_secs(*sz)
        } else {
            EST_TTL_SECS
        };
        if now.saturating_sub(*at) < ttl {
            Some((*sz, *complete))
        } else {
            None
        }
    }

    pub fn record_size(&mut self, path: PathBuf, dir_mtime: u64, size: u64, complete: bool) {
        self.size_map
            .insert(path, (dir_mtime, size, now_secs(), complete));
        self.dirty = true;
    }

    pub fn set_meta(&mut self, key: &str, value: &str) {
        self.meta.insert(key.to_string(), value.to_string());
        self.dirty = true;
    }

    pub fn meta(&self, key: &str) -> Option<&str> {
        self.meta.get(key).map(|s| s.as_str())
    }

    /// User-pinned Quick-Access folders (persisted, de-duped, in order).
    pub fn qa_pins(&self) -> Vec<PathBuf> {
        self.meta
            .get("qa_pins")
            .map(|s| {
                s.split('|')
                    .filter(|p| !p.is_empty())
                    .map(|p| PathBuf::from(percent_decode(p)))
                    .filter(|p| !p.as_os_str().is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Replace the user-pinned Quick-Access folders.
    pub fn set_qa_pins(&mut self, pins: &[PathBuf]) {
        let joined = pins
            .iter()
            .map(|p| percent_encode(&p.to_string_lossy()))
            .collect::<Vec<_>>()
            .join("|");
        self.meta.insert("qa_pins".to_string(), joined);
        self.dirty = true;
    }

    /// Drop every `tab<id>_*` key, so closed tabs leave nothing behind on disk.
    pub fn meta_retain_tabs(&mut self, closed: &[u32]) {
        if closed.is_empty() {
            return;
        }
        let prefixes: Vec<String> = closed.iter().map(|id| format!("tab{id}_")).collect();
        self.meta
            .retain(|k, _| !prefixes.iter().any(|p| k.starts_with(p)));
        self.dirty = true;
    }

    /// Atomically write the store if dirty.
    pub fn flush(&mut self) -> std::io::Result<()> {
        if !self.dirty {
            return Ok(());
        }
        if let Some(parent) = self.file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut buf = String::with_capacity(64 * 1024);
        for (path, (mtime, size, computed_at, complete)) in &self.size_map {
            buf.push(if *complete { 'S' } else { 'E' });
            buf.push('\t');
            buf.push_str(&size.to_string());
            buf.push('\t');
            buf.push_str(&mtime.to_string());
            buf.push('\t');
            buf.push_str(&computed_at.to_string());
            buf.push('\t');
            buf.push_str(&percent_encode(&path.to_string_lossy()));
            buf.push('\n');
        }
        for (k, v) in &self.meta {
            buf.push('M');
            buf.push('\t');
            buf.push_str(k);
            buf.push('\t');
            buf.push_str(&percent_encode(v));
            buf.push('\n');
        }
        // Write directly to the store file (no temp+rename). On Windows a
        // `rename` that replaces an already-open / Defender-scanned file fails
        // with ERROR_ACCESS_DENIED / ERROR_SHARING_VIOLATION, which surfaced as
        // "持久化失败: 拒绝访问". A plain truncating write is far more robust,
        // and the few retries below survive a transient antivirus lock.
        let data = buf.into_bytes();
        let mut last_err = None;
        for _ in 0..5 {
            match fs::write(&self.file, &data) {
                Ok(()) => {
                    self.dirty = false;
                    return Ok(());
                }
                Err(e) => {
                    last_err = Some(e);
                    std::thread::sleep(std::time::Duration::from_millis(60));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "store flush failed")
        }))
    }
}
