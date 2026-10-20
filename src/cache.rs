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

pub const CACHE_SUBDIR: &str = "rust-explorer";
pub const STORE_FILE: &str = "store.txt";

/// Where the store lives. APPDATA on Windows, `~/.config` elsewhere.
pub fn cache_dir() -> PathBuf {
    let base = std::env::var("APPDATA")
        .map(PathBuf::from)
        .or_else(|_| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .map(|h| h.join(".config"))
        })
        .unwrap_or_else(|_| PathBuf::from("."));
    base.join(CACHE_SUBDIR)
}

pub struct Store {
    /// folder path -> (folder mtime, recursive size)
    pub size_map: HashMap<PathBuf, (u64, u64)>,
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
                    Some("S") => {
                        let size = f.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                        let mtime = f.next().and_then(|x| x.parse().ok()).unwrap_or(0);
                        let path = f
                            .next()
                            .map(percent_decode)
                            .map(PathBuf::from)
                            .unwrap_or_default();
                        if !path.as_os_str().is_empty() {
                            s.size_map.insert(path, (mtime, size));
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

    /// A cached size for `path`, valid only if the folder's mtime still matches.
    pub fn size_for(&self, path: &Path, dir_mtime: u64) -> Option<u64> {
        self.size_map
            .get(path)
            .filter(|&&(m, _)| m == dir_mtime)
            .map(|&(_, sz)| sz)
    }

    pub fn record_size(&mut self, path: PathBuf, dir_mtime: u64, size: u64) {
        self.size_map.insert(path, (dir_mtime, size));
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
        for (path, (mtime, size)) in &self.size_map {
            buf.push('S');
            buf.push('\t');
            buf.push_str(&size.to_string());
            buf.push('\t');
            buf.push_str(&mtime.to_string());
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
