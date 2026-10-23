//! Image preview / thumbnail support.
//!
//! Decoding happens on a dedicated background worker so a folder full of
//! 20 MP photos can never freeze the UI. Results come back as raw RGBA and are
//! uploaded to the GPU (as an `egui::TextureHandle`) on the UI thread, then
//! cached under `(path, max_side)`.
//!
//! Three preview modes are built on top of this cache:
//!   * thumbnail grid (see `ViewMode::Grid` in `main.rs`),
//!   * a side "预览" pane for the selected file,
//!   * a full-window viewer with fit / 1:1 zoom.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};

/// Extensions we attempt to decode.
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "bmp", "gif", "webp", "ico", "tif", "tiff", "tga",
];

/// Long-edge size for grid thumbnails.
pub const THUMB: u16 = 128;
/// Long-edge size for the side preview pane.
pub const PREVIEW: u16 = 720;
/// Long-edge size for the full-window viewer.
pub const FULL: u16 = 1600;

/// `true` when `name` looks like an image we can preview.
pub fn is_image(name: &str) -> bool {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .map(|e| IMAGE_EXTS.contains(&e.as_str()))
        .unwrap_or(false)
}

/// Decoded texture + its natural pixel size.
pub struct Tex {
    pub tex: egui::TextureHandle,
    pub w: u32,
    pub h: u32,
}

impl Tex {
    pub fn aspect(&self) -> f32 {
        if self.h == 0 {
            1.0
        } else {
            self.w as f32 / self.h as f32
        }
    }
}

type Key = (PathBuf, u16);
type Reply = (PathBuf, u16, Option<(u32, u32, Vec<u8>)>);

/// LRU-ish cache of decoded images, plus the decode worker.
pub struct Previews {
    map: HashMap<Key, Tex>,
    order: Vec<Key>,
    failed: HashSet<Key>,
    pending: HashSet<Key>,
    /// RGBA bytes currently held in `map` (dropped entries give theirs back).
    bytes: usize,
    req_tx: Sender<(PathBuf, u16)>,
    res_rx: Receiver<Reply>,
}

/// Upper bound on cached textures; oldest entries are dropped beyond this.
const MAX_CACHE: usize = 96;
/// …plus a byte budget on top of the count: one full-window viewer texture
/// (1600 px long edge) is ~10 MB of RGBA, so a count-only cap could still
/// hold ~1 GB once a folder of photos is browsed. Grid thumbnails (128 px)
/// are ~65 KB each, so for them the count cap stays the binding one.
const MAX_CACHE_BYTES: usize = 192 * 1024 * 1024;
/// Files bigger than this are not decoded (protects against 500 MB TIFFs).
const MAX_FILE_BYTES: u64 = 96 * 1024 * 1024;

impl Previews {
    pub fn new() -> Self {
        let (req_tx, req_rx) = channel::<(PathBuf, u16)>();
        let (res_tx, res_rx) = channel::<Reply>();
        std::thread::spawn(move || {
            while let Ok((path, max)) = req_rx.recv() {
                let out = decode(&path, max);
                res_tx.send((path, max, out)).ok();
            }
        });
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            failed: HashSet::new(),
            pending: HashSet::new(),
            bytes: 0,
            req_tx,
            res_rx,
        }
    }

    /// Drain finished decodes and upload them as textures. Call once per frame.
    pub fn poll(&mut self, ctx: &egui::Context) {
        while let Ok((path, max, out)) = self.res_rx.try_recv() {
            let key = (path.clone(), max);
            self.pending.remove(&key);
            match out {
                Some((w, h, rgba)) => {
                    if w == 0 || h == 0 || rgba.len() < (w as usize * h as usize * 4) {
                        self.failed.insert(key);
                        continue;
                    }
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [w as usize, h as usize],
                        &rgba,
                    );
                    let name = format!("prev:{}@{}", path.display(), max);
                    let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
                    self.insert(key, Tex { tex, w, h });
                }
                None => {
                    self.failed.insert(key);
                }
            }
        }
    }

    fn insert(&mut self, key: Key, tex: Tex) {
        // A re-decode of a cached key replaces its texture; drop the old one
        // from the bookkeeping first so `bytes` stays truthful.
        if let Some(old) = self.map.remove(&key) {
            self.bytes = self.bytes.saturating_sub(tex_bytes(&old));
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                self.order.remove(pos);
            }
        }
        self.bytes += tex_bytes(&tex);
        self.map.insert(key.clone(), tex);
        self.order.push(key);
        // Evict oldest-first until both the count and the byte budget fit.
        // `> 1` keeps one texture even if it alone busts the budget (a single
        // 1600 px texture is ~10 MB, far below the cap, so this is just a
        // guard against an eviction loop).
        while self.order.len() > MAX_CACHE
            || (self.bytes > MAX_CACHE_BYTES && self.order.len() > 1)
        {
            let old = self.order.remove(0);
            if let Some(tex) = self.map.remove(&old) {
                self.bytes = self.bytes.saturating_sub(tex_bytes(&tex));
            }
        }
    }

    /// Texture for `path` at (at most) `max` pixels on the long edge.
    /// Returns `None` on the first call (decode is queued) — call again next frame.
    pub fn get(&mut self, path: &Path, max: u16) -> Option<&Tex> {
        let key = (path.to_path_buf(), max);
        if self.map.contains_key(&key) {
            // touch: move to the back of the eviction order
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                let k = self.order.remove(pos);
                self.order.push(k);
            }
            return self.map.get(&key);
        }
        if self.failed.contains(&key) {
            return None;
        }
        if self.pending.insert(key) {
            self.req_tx.send((path.to_path_buf(), max)).ok();
        }
        None
    }

    /// `true` when `path` was tried and could not be decoded.
    pub fn failed(&self, path: &Path, max: u16) -> bool {
        self.failed.contains(&(path.to_path_buf(), max))
    }
}

/// RGBA bytes a cached texture occupies.
fn tex_bytes(tex: &Tex) -> usize {
    tex.w as usize * tex.h as usize * 4
}

/// Decode `path` down to `max` px on the long edge; returns `(w, h, rgba)`.
fn decode(path: &Path, max: u16) -> Option<(u32, u32, Vec<u8>)> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 || meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let img = if img.width() > max as u32 || img.height() > max as u32 {
        img.thumbnail(max as u32, max as u32)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    Some((w, h, rgba.into_raw()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decode the first image found in the user's Pictures folder, so the
    /// worker path (format sniffing + downscale + RGBA) is exercised for real.
    #[test]
    fn decode_real_image() {
        let dir = std::path::Path::new(r"C:\Users\user\Pictures\1");
        let Some(path) = std::fs::read_dir(dir).ok().and_then(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .find(|p| p.is_file() && is_image(&p.to_string_lossy()))
        }) else {
            eprintln!("skip: no sample image in {}", dir.display());
            return;
        };
        let r = decode(&path, 256);
        println!(
            "decode {} -> {:?}",
            path.display(),
            r.as_ref().map(|(w, h, v)| (*w, *h, v.len()))
        );
        assert!(r.is_some(), "decode returned None for {}", path.display());
    }
}
