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

use std::collections::{BinaryHeap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

/// Extensions we attempt to decode.
pub const IMAGE_EXTS: &[&str] = &[
    "png", "jpg", "jpeg", "jpe", "bmp", "gif", "webp", "ico", "tif", "tiff", "tga",
];

/// Extensions that are obviously video. When the extension is empty or
/// garbled (e.g. `.jpgqm`, `.saveadsfqwer`) we fall back to content sniffing.
pub const VIDEO_EXTS: &[&str] = &[
    "mp4", "m4v", "mov", "avi", "wmv", "mkv", "webm", "flv", "mpg", "mpeg", "m2ts",
    "ts", "3gp", "mp4v", "ogv",
];

/// Long-edge size for grid thumbnails.
pub const THUMB: u16 = 128;
/// Long-edge size for the side preview pane.
pub const PREVIEW: u16 = 720;
/// Long-edge size for the full-window viewer.
pub const FULL: u16 = 1600;

/// Every size we ever cache, ascending. A decode at one size also fills the
/// smaller ones (see `derive_smaller`), so a grid thumbnail or a preview-pane
/// image costs nothing extra once the picture has been decoded once.
const LADDER: [u16; 3] = [THUMB, PREVIEW, FULL];

/// Decode request priority. Higher sorts first in the worker heap, so the
/// image the user is actually looking at never waits behind a backlog of
/// grid thumbnails.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Priority {
    /// Grid thumbnails: a background backlog, lowest priority.
    Grid,
    /// Viewer filmstrip (±5 around the current image).
    Film,
    /// The image being viewed / the selected file's preview pane: decode now.
    Urgent,
}

/// What kind of media a file is, decided by extension first and then by
/// sniffing the file's magic bytes when the extension is empty or unknown.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Video,
    Other,
}

/// `true` when `name` looks like an image we can preview. The extension is the
/// fast path; when it is empty or not a known image extension we sniff the
/// first bytes of `path` so files with garbled extensions (`.jpgqm`,
/// `.saveadsfqwer`) still preview correctly.
pub fn is_image(name: &str, path: &Path) -> bool {
    match kind_from_ext(name) {
        Some(MediaKind::Image) => true,
        Some(_) => false,
        None => sniff_image(path),
    }
}

/// `true` when `name` is a video. Same extension-first, content-sniff-fallback
/// logic as [`is_image`].
pub fn is_video(name: &str, path: &Path) -> bool {
    match kind_from_ext(name) {
        Some(MediaKind::Video) => true,
        Some(_) => false,
        None => sniff_video(path),
    }
}

/// Image or video — anything the internal viewer can show.
pub fn is_previewable(name: &str, path: &Path) -> bool {
    is_image(name, path) || is_video(name, path)
}

/// Extension-based guess only (no I/O). `None` means "unknown — sniff it".
fn kind_from_ext(name: &str) -> Option<MediaKind> {
    let e = Path::new(name)
        .extension()
        .map(|x| x.to_string_lossy().to_ascii_lowercase())?;
    if IMAGE_EXTS.contains(&e.as_str()) {
        Some(MediaKind::Image)
    } else if VIDEO_EXTS.contains(&e.as_str()) {
        Some(MediaKind::Video)
    } else {
        None
    }
}

/// Read up to `n` bytes from the head of `path`. Returns an empty vec on any
/// failure (missing file, locked, etc.) so callers treat sniffing as "not
/// this kind" rather than erroring.
fn head(path: &Path, n: usize) -> Vec<u8> {
    use std::io::Read;
    let mut f = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };
    let mut buf = vec![0u8; n];
    match f.read(&mut buf) {
        Ok(k) => buf.truncate(k),
        Err(_) => return Vec::new(),
    }
    buf
}

/// Image magic-byte signatures (subset covering what `image` decodes plus
/// AVIF/HEIC which the viewer may gain later).
/// `true` when the file is a GIF — extension first, magic bytes when the
/// extension is missing or garbled (`.gifqq`, no extension, …). The viewer
/// uses this to decide whether to decode the whole animation; everything else
/// still shows the first frame as a still.
pub fn is_gif(name: &str, path: &Path) -> bool {
    if name.to_ascii_lowercase().ends_with(".gif") {
        return true;
    }
    let b = head(path, 6);
    b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a")
}

fn sniff_image(path: &Path) -> bool {
    let b = head(path, 16);
    if b.len() < 4 {
        return false;
    }
    // JPEG: FF D8 FF
    if b[0] == 0xFF && b[1] == 0xD8 && b[2] == 0xFF {
        return true;
    }
    // PNG: 89 50 4E 47
    if b[0] == 0x89 && b[1] == 0x50 && b[2] == 0x4E && b[3] == 0x47 {
        return true;
    }
    // GIF: 47 49 46 38
    if b[0] == 0x47 && b[1] == 0x49 && b[2] == 0x46 && b[3] == 0x38 {
        return true;
    }
    // BMP: 42 4D
    if b[0] == 0x42 && b[1] == 0x4D {
        return true;
    }
    // ICO: 00 00 01 00
    if b[0] == 0x00 && b[1] == 0x00 && b[2] == 0x01 && b[3] == 0x00 {
        return true;
    }
    // TIFF: 49 49 2A 00 (little-endian) or 4D 4D 00 2A (big-endian)
    if (b[0] == 0x49 && b[1] == 0x49 && b[2] == 0x2A && b[3] == 0x00)
        || (b[0] == 0x4D && b[1] == 0x4D && b[2] == 0x00 && b[3] == 0x2A)
    {
        return true;
    }
    // WEBP / AVIF / HEIC / MP4-family: RIFF....WEBP or ....ftyp....
    if b.len() >= 12 {
        if &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
            return true;
        }
        // ISO-BMFF / QuickTime: bytes 4..8 == "ftyp"; major brand at 8..12.
        if &b[4..8] == b"ftyp" {
            let brand = &b[8..12];
            if matches!(brand, b"avif" | b"avis" | b"heic" | b"heis" | b"hevc" | b"hevx") {
                return true;
            }
        }
    }
    false
}

/// Video magic-byte signatures.
fn sniff_video(path: &Path) -> bool {
    let b = head(path, 16);
    if b.len() < 4 {
        return false;
    }
    // MP4 / MOV / M4V / 3GP / QuickTime: ISO-BMFF box "ftyp" at offset 4.
    if b.len() >= 12 && &b[4..8] == b"ftyp" {
        let brand = &b[8..12];
        if matches!(
            brand,
            b"mp42" | b"isom" | b"m4v " | b"m4a " | b"3gp" | b"3gp5" | b"qt  " | b"mp4 "
        ) {
            return true;
        }
    }
    // MKV / WebM: EBML header 1A 45 DF A3.
    if b[0] == 0x1A && b[1] == 0x45 && b[2] == 0xDF && b[3] == 0xA3 {
        return true;
    }
    // AVI: RIFF....AVI[ ]
    if b.len() >= 12 && &b[0..4] == b"RIFF" && &b[8..12] == b"AVI " {
        return true;
    }
    // FLV: 46 4C 56
    if b[0] == 0x46 && b[1] == 0x4C && b[2] == 0x56 {
        return true;
    }
    // ASF / WMV: 30 26 B2 75 (16-byte GUID prefix)
    if b[0] == 0x30 && b[1] == 0x26 && b[2] == 0xB2 && b[3] == 0x75 {
        return true;
    }
    false
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

/// Shared decode queue: a max-heap of pending requests, drained by the worker
/// thread in priority order. `Mutex` + `Condvar` let the worker sleep when
/// idle and wake on each new request.
struct DecodeQueue {
    heap: BinaryHeap<(Priority, std::cmp::Reverse<u64>, PathBuf, u16)>,
}

/// LRU-ish cache of decoded images, plus the decode worker.
pub struct Previews {
    map: HashMap<Key, Tex>,
    order: Vec<Key>,
    failed: HashSet<Key>,
    pending: HashSet<Key>,
    /// RGBA bytes currently held in `map` (dropped entries give theirs back).
    bytes: usize,
    /// Monotonic sequence for same-priority FIFO ordering.
    seq: u64,
    /// Priority-sorted decode queue shared with the worker thread.
    queue: Arc<(Mutex<DecodeQueue>, Condvar)>,
    res_rx: Receiver<Reply>,
    /// When false, `Priority::Grid` requests are served from the cache only and
    /// never enqueued. The 小 perf mode turns this off so browsing a photo
    /// folder costs zero background decode; already-cached thumbnails still
    /// show. Urgent / Film requests are unaffected.
    allow_grid: bool,
}

/// Upper bound on cached textures; oldest entries are dropped beyond this.
const MAX_CACHE: usize = 96;
/// Upper bound on in-flight decode requests. Once the queue is this deep we
/// stop enqueuing new low-priority (grid) thumbnails — a fast scroll can
/// otherwise pile up thousands. Urgent / Film requests always enqueue.
const MAX_PENDING: usize = 256;
/// …plus a byte budget on top of the count: one full-window viewer texture
/// (1600 px long edge) is ~10 MB of RGBA, so a count-only cap could still
/// hold ~1 GB once a folder of photos is browsed. Grid thumbnails (128 px)
/// are ~65 KB each, so for them the count cap stays the binding one.
const MAX_CACHE_BYTES: usize = 192 * 1024 * 1024;
/// Files bigger than this are not decoded (protects against 500 MB TIFFs).
const MAX_FILE_BYTES: u64 = 96 * 1024 * 1024;
/// Decode threads. More than one so an urgent request (the picture the user
/// is looking at) never waits for a slow in-flight grid thumbnail.
const WORKERS: usize = 3;

impl Previews {
    pub fn new() -> Self {
        let (res_tx, res_rx) = channel::<Reply>();
        let queue = Arc::new((Mutex::new(DecodeQueue { heap: BinaryHeap::new() }), Condvar::new()));
        for _ in 0..WORKERS {
            let qw = queue.clone();
            let tx = res_tx.clone();
            std::thread::spawn(move || {
                let (lock, cvar) = &*qw;
                loop {
                    let (path, max) = {
                        let mut g = lock.lock().unwrap();
                        while g.heap.is_empty() {
                            g = cvar.wait(g).unwrap();
                        }
                        let (_p, _s, p, m) = g.heap.pop().unwrap();
                        (p, m)
                    };
                    let out = decode(&path, max);
                    // One disk decode feeds every smaller cache entry, so the
                    // grid and the preview pane reuse this work for free.
                    if let Some((w, h, rgba)) = &out {
                        derive_smaller(&path, max, *w, *h, rgba, &tx);
                    }
                    if tx.send((path, max, out)).is_err() {
                        break;
                    }
                }
            });
        }
        Self {
            map: HashMap::new(),
            order: Vec::new(),
            failed: HashSet::new(),
            pending: HashSet::new(),
            bytes: 0,
            seq: 0,
            queue,
            res_rx,
            allow_grid: true,
        }
    }

    /// Enable/disable background decoding of grid thumbnails. Called once per
    /// frame from the 小 / 中 / 大 setting (see `Explorer::perf_repaint_ms`).
    pub fn set_allow_grid(&mut self, on: bool) {
        self.allow_grid = on;
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
    /// Returns `None` on the first call (decode is queued at `prio`) — call
    /// again next frame. `Urgent` requests jump ahead of any grid-thumbnail
    /// backlog, so clicking / double-clicking an image always decodes first.
    pub fn get(&mut self, path: &Path, max: u16, prio: Priority) -> Option<&Tex> {
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
        // 小 perf mode: never start a new grid decode — a cache miss falls back
        // to the shell icon. Cached thumbnails were returned above.
        if prio == Priority::Grid && !self.allow_grid {
            return None;
        }
        if self.pending.insert(key.clone()) {
            // Bound the background grid backlog: if the queue is already deep,
            // drop new low-priority grid requests rather than letting a fast
            // scroll pile up thousands. They simply re-show the shell icon
            // until the backlog drains. Urgent / Film always enqueue.
            if prio == Priority::Grid && self.pending.len() > MAX_PENDING {
                self.pending.remove(&key);
                return None;
            }
            let seq = self.seq;
            self.seq += 1;
            let mut g = self.queue.0.lock().unwrap();
            g.heap.push((prio, std::cmp::Reverse(seq), path.to_path_buf(), max));
            self.queue.1.notify_one();
        }
        None
    }

    /// `true` when `path` was tried and could not be decoded.
    pub fn failed(&self, path: &Path, max: u16) -> bool {
        self.failed.contains(&(path.to_path_buf(), max))
    }

    /// `true` while any decode is in flight. The caller must keep repainting
    /// while this holds: the result arrives on a background thread and no
    /// input event will wake the window on its own.
    pub fn busy(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Texture for `path` at (at most) `max` pixels on the long edge.
    ///
    /// Returns `(Some(tex), true)` once the requested size is available.
    /// While it is still decoding, the *largest already-cached smaller* size is
    /// returned with `false`, so the UI can paint a (slightly soft) picture
    /// right away instead of a "loading" placeholder — the grid thumbnail is
    /// almost always already decoded, so stepping through a folder feels
    /// instant even before the full-size decode lands.
    pub fn get_or_smaller(
        &mut self,
        path: &Path,
        max: u16,
        prio: Priority,
    ) -> (Option<&Tex>, bool) {
        let key = (path.to_path_buf(), max);
        if self.map.contains_key(&key) {
            // touch: move to the back of the eviction order
            if let Some(pos) = self.order.iter().position(|k| *k == key) {
                let k = self.order.remove(pos);
                self.order.push(k);
            }
            return (self.map.get(&key), true);
        }
        if self.failed.contains(&key) {
            return (None, true);
        }
        if self.pending.insert(key.clone()) {
            // Bound the background grid backlog: if the queue is already deep,
            // drop new low-priority grid requests rather than letting a fast
            // scroll pile up thousands. They simply re-show the shell icon
            // until the backlog drains. Urgent / Film always enqueue.
            if prio == Priority::Grid && self.pending.len() > MAX_PENDING {
                self.pending.remove(&key);
                return (None, true);
            }
            let seq = self.seq;
            self.seq += 1;
            let mut g = self.queue.0.lock().unwrap();
            g.heap.push((prio, std::cmp::Reverse(seq), path.to_path_buf(), max));
            self.queue.1.notify_one();
        }
        // Placeholder: the biggest cached size below `max`.
        let mut best: Option<u16> = None;
        for s in LADDER {
            if s < max && self.map.contains_key(&(path.to_path_buf(), s)) {
                best = Some(s);
            }
        }
        match best {
            Some(s) => (self.map.get(&(path.to_path_buf(), s)), false),
            None => (None, false),
        }
    }
}

/// RGBA bytes a cached texture occupies.
fn tex_bytes(tex: &Tex) -> usize {
    tex.w as usize * tex.h as usize * 4
}

/// Downscale an already-decoded frame into every smaller cache size and send
/// those as extra results. Cheap next to a disk decode, and it means the grid
/// never re-decodes a picture the viewer has already opened.
fn derive_smaller(
    path: &Path,
    from: u16,
    w: u32,
    h: u32,
    rgba: &[u8],
    tx: &Sender<Reply>,
) {
    let long = w.max(h);
    if long == 0 {
        return;
    }
    for s in LADDER {
        if s >= from || long <= s as u32 {
            continue;
        }
        let Some(img) = image::RgbaImage::from_raw(w, h, rgba.to_vec()) else {
            continue;
        };
        let scale = s as f32 / long as f32;
        let nw = ((w as f32 * scale).round()).max(1.0) as u32;
        let nh = ((h as f32 * scale).round()).max(1.0) as u32;
        let out = image::imageops::resize(&img, nw, nh, image::imageops::FilterType::Triangle);
        let (ow, oh) = (out.width(), out.height());
        if tx
            .send((path.to_path_buf(), s, Some((ow, oh, out.into_raw()))))
            .is_err()
        {
            return;
        }
    }
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
                .find(|p| p.is_file() && is_image(&p.file_name().to_string_lossy(), p))
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
