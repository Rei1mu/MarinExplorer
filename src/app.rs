//! Core application state & filesystem operations.
//!
//! `Panel` is one side of the two-pane manager: it owns the current directory,
//! the sorted/filtered listing, navigation history, and selection.
//!
//! Folder sizes are computed lazily by a background worker (see `Explorer`),
//! cached on disk (see `cache::Store`), and invalidated by the folder's own
//! mtime so they are never shown stale.

use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::Sender;

use crate::fs_util::{self, RecItem};

/// One directory entry shown in a panel.
#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64, // valid & `size_known` for files; for dirs filled by the size worker
    pub mtime: u64, // unix seconds (best-effort)
    /// Creation time, unix seconds (0 when the platform cannot report it).
    pub ctime: u64,
    pub size_known: bool,
    pub ext: String, // lowercased extension ("" for dirs / no extension)
}

impl Entry {
    /// Human-readable type label for the "类型" column.
    pub fn type_label(&self, lang: crate::lang::Lang) -> String {
        if self.is_dir {
            crate::lang::tr("文件夹", lang).to_string()
        } else if self.ext.is_empty() {
            crate::lang::tr("文件", lang).to_string()
        } else {
            self.ext.to_uppercase()
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Type,
    Size,
    /// 修改时间 (full timestamp).
    Time,
    /// 创建时间 (full timestamp). Persisted as "ctime"; the pre-rename
    /// id "date" still maps here so an old store keeps its sort.
    Ctime,
}

#[derive(Clone, Copy, PartialEq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    /// Persistence encoding. It is *inverted* on purpose: the default sort
    /// direction used to be ascending ("先小到大 / 旧到新") and every existing
    /// config on disk says "asc", so reading "asc" as descending turns those
    /// saved panels into the new default without a migration step.
    ///
    /// `id` must stay the exact inverse of `from_id`, otherwise every restart
    /// silently flips the direction the user picked (the "旧到新 after
    /// restart" bug).
    pub fn id(self) -> &'static str {
        match self {
            SortDir::Asc => "desc",
            SortDir::Desc => "asc",
        }
    }
    pub fn from_id(s: &str) -> SortDir {
        if s == "desc" { SortDir::Asc } else { SortDir::Desc }
    }
    pub fn arrow(self) -> &'static str {
        match self {
            SortDir::Asc => "▲",
            SortDir::Desc => "▼",
        }
    }
}

/// How a panel renders its listing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// Name / 类型 / 大小 / 修改时间 / 创建时间 columns.
    List,
    /// Thumbnail grid (images get real previews).
    Grid,
}

impl View {
    pub fn id(self) -> &'static str {
        match self {
            View::List => "list",
            View::Grid => "grid",
        }
    }
    pub fn from_id(s: &str) -> View {
        if s == "grid" { View::Grid } else { View::List }
    }
}

impl SortKey {
    pub fn label(self, lang: crate::lang::Lang) -> &'static str {
        match self {
            SortKey::Name => crate::lang::tr("名称", lang),
            SortKey::Type => crate::lang::tr("类型", lang),
            SortKey::Size => crate::lang::tr("大小", lang),
            SortKey::Time => crate::lang::tr("修改时间", lang),
            SortKey::Ctime => crate::lang::tr("创建时间", lang),
        }
    }
    /// Stable string id for persistence.
    pub fn id(self) -> &'static str {
        match self {
            SortKey::Name => "name",
            SortKey::Type => "type",
            SortKey::Size => "size",
            SortKey::Time => "time",
            SortKey::Ctime => "ctime",
        }
    }
    pub fn from_id(s: &str) -> SortKey {
        match s {
            "type" => SortKey::Type,
            "size" => SortKey::Size,
            "time" => SortKey::Time,
            // "date" is the pre-rename id of the same column.
            "ctime" | "date" => SortKey::Ctime,
            _ => SortKey::Name,
        }
    }
}

/// Clipboard state for copy / move. Holds *all* selected paths, so a
/// Ctrl+C on a multi-selection copies every item at once.
#[derive(Clone)]
pub enum Clip {
    Copy(Vec<PathBuf>),
    Move(Vec<PathBuf>),
}

/// A directory listing (one side of the manager).
pub struct Panel {
    pub cwd: PathBuf,
    pub entries: Vec<Entry>,
    pub recycle_items: Vec<RecItem>,
    pub in_recycle: bool,
    pub selected: Option<String>, // entry name, or recycle `suffix`
    /// Multi-selection (Ctrl+click toggles, Shift+click extends, Ctrl+A
    /// selects all). Kept alongside `selected`: `selected` is the "current"
    /// item (what Enter opens, what the preview pane shows), `multi` holds
    /// everything that is highlighted. Empty = plain single selection.
    pub multi: std::collections::HashSet<String>,
    /// Shift+click anchor: extending a range starts from here.
    pub anchor: Option<String>,
    pub filter: String,
    pub sort_key: SortKey,
    pub sort_dir: SortDir,
    pub view: View,
    pub history: Vec<PathBuf>,
    pub index: usize,
    /// Bumped on every `load()`. Lets the renderer hand `entries` out for the
    /// duration of a frame and only put it back when no reload happened
    /// meanwhile (see `render_list`), avoiding a per-frame deep clone.
    pub generation: u64,
    /// True while the directory listing is being read on a worker thread (a
    /// folder with tens of thousands of entries used to freeze the UI thread
    /// for seconds; now the panel shows 加载中… until the entries arrive).
    pub loading: bool,
    /// Set once by `Explorer::new`; from then on `load()` reads the listing on
    /// a worker thread instead of on the UI thread. `None` during the initial
    /// (synchronous) construction load.
    pub list_tx: Option<Sender<(PathBuf, u64)>>,
    /// True while the recycle-bin listing is being read on a worker thread
    /// (a bin can hold thousands of `$I` files; parsing them on the UI
    /// thread used to freeze the whole app).
    pub rec_loading: bool,
}

impl Panel {
    pub fn new(cwd: PathBuf) -> Self {
        let mut p = Self {
            cwd: cwd.clone(),
            entries: vec![],
            recycle_items: vec![],
            in_recycle: false,
            selected: None,
            multi: std::collections::HashSet::new(),
            anchor: None,
            filter: String::new(),
            // Newest first: a freshly opened folder shows what changed last
            // at the top, which is what people usually look for.
            sort_key: SortKey::Time,
            sort_dir: SortDir::Desc,
            view: View::List,
            history: vec![cwd.clone()],
            index: 0,
            generation: 0,
            loading: false,
            list_tx: None,
            rec_loading: false,
        };
        p.load(&cwd);
        p
    }

    /// True when `dir` points *inside* a per-user Recycle-Bin folder
    /// (i.e. `...\$Recycle.Bin\<SID>`, not the bare `$Recycle.Bin` itself).
    pub fn is_recycle_view(dir: &std::path::Path) -> bool {
        let comps: Vec<_> = dir.components().collect();
        let is_rb = |c: &std::path::Component| {
            matches!(c, std::path::Component::Normal(s) if s.to_str() == Some("$Recycle.Bin"))
        };
        let has_rb = comps.iter().any(&is_rb);
        has_rb && !matches!(comps.last(), Some(c) if is_rb(c))
    }

    /// Reload the listing (normal or recycle view) for `dir`.
    ///
    /// The recycle view is *not* read here: parsing a big bin's `$I` files
    /// takes seconds and would block the UI thread. Instead the panel is
    /// flagged `rec_loading` and `Explorer` reads the listing on a worker
    /// thread (see `Explorer::request_recycle_listings`).
    ///
    /// Once a `list_tx` is installed (always, except during construction) the
    /// normal listing is *also* read on a worker thread: a folder with tens of
    /// thousands of entries blocks the UI thread for seconds otherwise. The
    /// panel shows 加载中… until `Explorer::poll_listings` adopts the result.
    pub fn load(&mut self, dir: &std::path::Path) {
        self.generation = self.generation.wrapping_add(1);
        self.cwd = dir.to_path_buf();
        self.selected = None;
        self.multi.clear();
        self.anchor = None;
        self.in_recycle = Self::is_recycle_view(dir);
        if self.in_recycle {
            self.recycle_items.clear();
            self.rec_loading = true;
            self.loading = false;
        } else {
            self.rec_loading = false;
            self.entries.clear();
            self.loading = true;
            let requested = self
                .list_tx
                .as_ref()
                .map(|tx| tx.send((self.cwd.clone(), self.generation)).is_ok())
                .unwrap_or(false);
            if !requested {
                // No worker wired up yet (initial construction): read inline
                // so the first frame already has a listing.
                self.adopt_listing(read_entries(&self.cwd));
            }
        }
    }

    /// Take a worker's listing result (already sorted by the caller's key).
    pub fn adopt_listing(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
        self.loading = false;
        self.resort();
    }

    /// Change directory, recording it in the history.
    pub fn navigate(&mut self, to: PathBuf) {
        if to == self.cwd {
            self.load(&to);
            return;
        }
        self.history.truncate(self.index + 1);
        self.history.push(to.clone());
        self.index = self.history.len() - 1;
        self.load(&to);
    }

    pub fn up(&mut self) {
        if let Some(p) = self.cwd.parent() {
            if p != self.cwd.as_path() {
                self.navigate(p.to_path_buf());
            }
        }
    }

    pub fn back(&mut self) {
        if self.index > 0 {
            self.index -= 1;
            let p = self.history[self.index].clone();
            self.load(&p);
        }
    }

    pub fn forward(&mut self) {
        if self.index < self.history.len() - 1 {
            self.index += 1;
            let p = self.history[self.index].clone();
            self.load(&p);
        }
    }

    pub fn can_back(&self) -> bool {
        self.index > 0
    }

    pub fn can_forward(&self) -> bool {
        self.index < self.history.len() - 1
    }

    /// Apply the current sort key/direction to the normal listing.
    pub fn resort(&mut self) {
        self.entries.sort_by(|a, b| {
            // Directories group together for name/type sorting (Explorer-like).
            match self.sort_key {
                SortKey::Name => {
                    let d = b.is_dir.cmp(&a.is_dir);
                    let c = a.name.to_lowercase().cmp(&b.name.to_lowercase());
                    if self.sort_dir == SortDir::Asc { d.then(c) } else { d.then(c.reverse()) }
                }
                SortKey::Type => {
                    let d = b.is_dir.cmp(&a.is_dir);
                    let c = a
                        .ext
                        .cmp(&b.ext)
                        .then(a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                    if self.sort_dir == SortDir::Asc { d.then(c) } else { d.then(c.reverse()) }
                }
                SortKey::Size => {
                    let c = a.size.cmp(&b.size);
                    if self.sort_dir == SortDir::Asc { c } else { c.reverse() }
                }
                SortKey::Time => {
                    let c = a.mtime.cmp(&b.mtime);
                    if self.sort_dir == SortDir::Asc { c } else { c.reverse() }
                }
                SortKey::Ctime => {
                    let c = a.ctime.cmp(&b.ctime);
                    if self.sort_dir == SortDir::Asc { c } else { c.reverse() }
                }
            }
        });
    }

    /// Indices into `entries` that pass the hidden-file + text filters.
    pub fn visible(&self, show_hidden: bool) -> Vec<usize> {
        let f = self.filter.trim().to_lowercase();
        (0..self.entries.len())
            .filter(|&i| {
                let e = &self.entries[i];
                (show_hidden || !e.name.starts_with('.'))
                    && (f.is_empty() || e.name.to_lowercase().contains(&f))
            })
            .collect()
    }

    pub fn sel_entry(&self) -> Option<&Entry> {
        self.selected.as_deref().and_then(|n| self.entries.iter().find(|e| e.name == n))
    }

    /// Full path of the selected normal entry.
    pub fn sel_path(&self) -> Option<PathBuf> {
        self.sel_entry().map(|e| self.cwd.join(&e.name))
    }

    /// Every highlighted entry name, in listing order. `multi` is the source
    /// of truth; the focus row is added when it somehow fell out of the set
    /// (e.g. it was Ctrl+clicked off and is now the only thing selected).
    pub fn sel_names(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .entries
            .iter()
            .filter(|e| self.multi.contains(&e.name))
            .map(|e| e.name.clone())
            .collect();
        if v.is_empty() {
            if let Some(s) = &self.selected {
                if self.entries.iter().any(|e| &e.name == s) {
                    v.push(s.clone());
                }
            }
        }
        v
    }

    /// Full paths of every highlighted entry (what Ctrl+C / Delete act on).
    pub fn sel_paths(&self) -> Vec<PathBuf> {
        self.sel_names().iter().map(|n| self.cwd.join(n)).collect()
    }

    /// Click handling with Explorer-style modifier keys.
    /// * plain click  — select only `name`
    /// * Ctrl+click   — toggle `name` in the multi-selection (the previous
    ///   focus row stays selected; toggling the focus row off leaves it
    ///   focused but unhighlighted, exactly like Explorer)
    /// * Shift+click  — select the range from `anchor` to `name`
    ///
    /// `vis` is the visible listing order (already sorted + filtered); the
    /// Shift range follows what the user actually sees.
    ///
    /// `multi` is the source of truth for highlighting; `selected` is the
    /// focus row (what the preview pane shows, what Enter opens) and is
    /// normally also a member of `multi`.
    pub fn click_select(&mut self, name: &str, ctrl: bool, shift: bool, vis: &[String]) {
        if shift {
            let from = self
                .anchor
                .as_deref()
                .and_then(|a| vis.iter().position(|n| n == a));
            let to = vis.iter().position(|n| n == name);
            if let (Some(a), Some(b)) = (from, to) {
                let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
                self.multi = vis[lo..=hi].iter().cloned().collect();
                self.selected = Some(name.to_string());
                return;
            }
        }
        if ctrl {
            // Keep the previous focus row selected unless it is the row being
            // toggled off.
            if let Some(s) = self.selected.take() {
                if s != name {
                    self.multi.insert(s);
                }
            }
            if !self.multi.insert(name.to_string()) {
                self.multi.remove(name);
            }
        } else {
            self.multi.clear();
            self.multi.insert(name.to_string());
        }
        self.selected = Some(name.to_string());
        self.anchor = Some(name.to_string());
    }

    pub fn sel_rec(&self) -> Option<&RecItem> {
        self.selected.as_deref().and_then(|s| self.recycle_items.iter().find(|r| &r.suffix == s))
    }

    /// Keyboard navigation within the recycle view.
    pub fn recycle_nav(&mut self, delta: i32, home: bool, end: bool) {
        if self.recycle_items.is_empty() {
            return;
        }
        let cur = self
            .selected
            .as_deref()
            .and_then(|s| self.recycle_items.iter().position(|r| &r.suffix == s));
        let next = match cur {
            None => {
                if delta < 0 || end {
                    self.recycle_items.len() - 1
                } else {
                    0
                }
            }
            Some(pos) => {
                let np = pos as i32 + delta;
                if home {
                    0
                } else if end {
                    self.recycle_items.len() - 1
                } else if np < 0 {
                    0
                } else if np >= self.recycle_items.len() as i32 {
                    self.recycle_items.len() - 1
                } else {
                    np as usize
                }
            }
        };
        if let Some(it) = self.recycle_items.get(next) {
            self.selected = Some(it.suffix.clone());
        }
    }

    /// Open the selected entry. Returns a `RecItem` if it's a recycle item
    /// (caller shows an info dialog); otherwise descends / opens the file.
    pub fn open(&mut self, name: &str) -> Option<RecItem> {
        if self.in_recycle {
            return self.recycle_items.iter().find(|r| r.suffix == name).cloned();
        }
        let p = self.cwd.join(name);
        if p.is_dir() {
            self.navigate(p);
        } else {
            fs_util::open_default_file(&p);
        }
        None
    }
}

/// Read one directory into unsorted `Entry` rows.
///
/// Kept as a free function so the *worker* thread can build a listing without
/// touching a `Panel` (which lives on the UI thread). Folder sizes are left
/// unknown here; the size worker fills them in (see `Explorer`).
pub fn read_entries(cwd: &std::path::Path) -> Vec<Entry> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(cwd) else {
        return out;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        let ext = path
            .extension()
            .map(|x| x.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let (is_dir, size, mtime, ctime, size_known) = match e.metadata() {
            Ok(md) => {
                let t = md
                    .modified()
                    .ok()
                    .and_then(|mt| mt.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                // Creation time: available on Windows; other platforms
                // return Err, which we surface as 0 ("—").
                let c = md
                    .created()
                    .ok()
                    .and_then(|ct| ct.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let is_dir = md.is_dir() || path.is_dir();
                if is_dir {
                    (true, 0, t, c, false) // size filled in by the background worker
                } else {
                    (false, md.len(), t, c, true)
                }
            }
            Err(_) => (false, 0, 0, 0, false),
        };
        out.push(Entry { name, is_dir, size, mtime, ctime, size_known, ext });
    }
    out
}

/// Read the user's per-drive recycle folder (`...\$Recycle.Bin\<SID>`),
/// recovering original names/sizes from the `$I` metadata files.
///
/// Every item is stored as *two* files: `$I<sfx>` (metadata: original path,
/// size, deletion time) and `$R<sfx>` (the actual payload). The two must be
/// paired by suffix — listing every `$R` as an orphan (which this used to
/// do unconditionally) duplicated the whole bin and, because `(` sorts
/// first, filled the screen with "(无元数据)" rows.
pub fn read_recycle_items(cwd: &std::path::Path) -> Vec<fs_util::RecItem> {
    let mut items: Vec<fs_util::RecItem> = Vec::new();
    let Ok(rd) = fs::read_dir(cwd) else {
        return items;
    };

    let mut meta: Vec<(String, Vec<u8>)> = Vec::new();
    let mut payloads: Vec<(String, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let fname = e.file_name().to_string_lossy().into_owned();
        let p = e.path();
        if let Some(sfx) = fname.strip_prefix("$I") {
            if let Ok(bytes) = fs::read(&p) {
                meta.push((sfx.to_string(), bytes));
            }
        } else if let Some(sfx) = fname.strip_prefix("$R") {
            payloads.push((sfx.to_string(), p));
        }
    }

    let mut parsed: Vec<String> = Vec::new();
    for (sfx, bytes) in &meta {
        let Some(mut it) = fs_util::parse_recycle_i(bytes, sfx) else {
            continue; // unreadable metadata: the payload pass below shows it
        };
        // `$I` stores the absolute path it was deleted from. Show just the
        // leaf name in 名称 and keep the path for the details window.
        it.orig_path = it.name.clone();
        if let Some(leaf) = it.orig_path.rsplit(|c| c == '\\' || c == '/').next() {
            if !leaf.is_empty() {
                it.name = leaf.to_string();
            }
        }
        // Fill in what the metadata cannot tell us (dir-ness, and a size
        // for directories, where `$I` always reports 0).
        if let Ok(md) = fs::metadata(cwd.join(format!("$R{sfx}"))) {
            it.is_dir = md.is_dir();
            if it.size == 0 {
                it.size = md.len();
            }
        }
        parsed.push(sfx.clone());
        items.push(it);
    }

    // Payload without usable metadata: still list it once, using the real
    // on-disk size/mtime so it isn't a row of dashes.
    for (sfx, p) in payloads {
        if parsed.iter().any(|s| *s == sfx) {
            continue;
        }
        let md = fs::metadata(&p).ok();
        let deleted_at = md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        items.push(fs_util::RecItem {
            name: format!("$R{sfx}"),
            orig_path: String::new(),
            is_dir: md.as_ref().map(|m| m.is_dir()).unwrap_or(false),
            size: md.as_ref().map(|m| m.len()).unwrap_or(0),
            deleted_at,
            suffix: sfx,
        });
    }

    items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    items
}

/// Human-readable byte size.
pub fn human_size(n: u64) -> String {    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{} {}", n, UNITS[0])
    } else {
        format!("{:.1} {}", v, UNITS[u])
    }
}

/// Rough mtime as "YYYY-MM-DD" (local wall-clock).
pub fn human_time(unix_secs: u64) -> String {
    let days = (unix_secs as i64 + local_offset()) / 86400;
    let (y, m, d) = civil_from_days(days);
    format!("{:04}-{:02}-{:02}", y, m, d)
}

/// Time of day as "HH:MM" (local wall-clock).
pub fn human_clock(unix_secs: u64) -> String {
    let s = (unix_secs as i64 + local_offset()).rem_euclid(86400);
    format!("{:02}:{:02}", s / 3600, (s % 3600) / 60)
}

/// Full timestamp "YYYY-MM-DD HH:MM" (local wall-clock). Used by the
/// 修改时间 / 创建时间 columns and the properties dialog.
pub fn human_datetime(unix_secs: u64) -> String {
    format!("{} {}", human_time(unix_secs), human_clock(unix_secs))
}

/// Timestamp without the year: "MM-DD HH:MM". For narrow columns (split view
/// with the preview pane open, or a small window) where the full stamp would
/// be truncated to uselessness.
pub fn human_datetime_short(unix_secs: u64) -> String {
    let days = (unix_secs as i64 + local_offset()) / 86400;
    let (_y, m, d) = civil_from_days(days);
    format!("{:02}-{:02} {}", m, d, human_clock(unix_secs))
}

/// Date without the year: "MM-DD". Last resort for very narrow columns.
pub fn human_date_short(unix_secs: u64) -> String {
    let days = (unix_secs as i64 + local_offset()) / 86400;
    let (_y, m, d) = civil_from_days(days);
    format!("{:02}-{:02}", m, d)
}

/// The machine's UTC offset, resolved once per process.
fn local_offset() -> i64 {
    use std::sync::OnceLock;
    static OFF: OnceLock<i64> = OnceLock::new();
    *OFF.get_or_init(crate::fs_util::local_offset_secs)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Split a path into clickable breadcrumb segments `(label, prefix)`.
/// `C:\a\b` -> [("C:\\", C:\), ("a", C:\a), ("b", C:\a\b)].
pub fn path_segments(path: &std::path::Path) -> Vec<(String, PathBuf)> {
    use std::path::Component;
    let mut segs = vec![];
    let mut prefix = PathBuf::new();
    for c in path.components() {
        match c {
            Component::Prefix(_) => {
                prefix.push(c.as_os_str());
            }
            Component::RootDir => {
                prefix.push(c.as_os_str());
                segs.push((prefix.to_string_lossy().into_owned(), prefix.clone()));
            }
            Component::Normal(s) => {
                prefix.push(s);
                segs.push((s.to_string_lossy().into_owned(), prefix.clone()));
            }
            _ => {}
        }
    }
    segs
}

#[cfg(test)]
mod recycle_tests {
    use super::*;

    /// Regression: the recycle view used to list every `$R` payload a second
    /// time as "(无元数据)" (the metadata `$I` was never paired with it), so the
    /// bin looked like it had no metadata at all. Every suffix must appear
    /// exactly once and carry a real name.
    #[test]
    fn recycle_view_lists_each_item_once() {
        let Some(sid) = fs_util::get_user_sid() else {
            eprintln!("skip: no SID");
            return;
        };
        let dir = PathBuf::from("C://$Recycle.Bin").join(&sid);
        if !dir.is_dir() {
            eprintln!("skip: no recycle folder");
            return;
        }
        let recycle_items = read_recycle_items(&dir);
        if recycle_items.is_empty() {
            eprintln!("skip: recycle bin is empty");
            return;
        }
        let mut sfx: Vec<&str> = recycle_items.iter().map(|r| r.suffix.as_str()).collect();
        let total = sfx.len();
        sfx.sort_unstable();
        sfx.dedup();
        assert_eq!(total, sfx.len(), "each item must be listed once");
        for r in &recycle_items {
            assert!(!r.name.is_empty(), "item has no name: {:?}", r);
            assert_ne!(r.name, "(无元数据)");
        }
        // Deletion time must actually decode (the FILETIME epoch constant used
        // to be 100× too large, which pinned every date to 1970).
        let with_time = recycle_items.iter().filter(|r| r.deleted_at > 0).count();
        assert!(
            with_time * 10 >= total * 9,
            "deletion time missing for most items: {with_time}/{total}"
        );
        println!("recycle rows: {total}, first: {:?}", recycle_items[0]);
    }
}

#[cfg(test)]
mod select_tests {
    use super::*;

    fn vis() -> Vec<String> {
        ["a.txt", "b.txt", "c.txt", "d.txt"].iter().map(|s| s.to_string()).collect()
    }

    /// A panel whose listing already holds the four test names (no disk
    /// access — `sel_names` filters the listing by membership).
    fn panel() -> Panel {
        let mut p = Panel::new(PathBuf::from("."));
        for n in vis() {
            p.entries.push(Entry {
                name: n,
                is_dir: false,
                size: 0,
                mtime: 0,
                ctime: 0,
                size_known: true,
                ext: "txt".to_string(),
            });
        }
        p
    }

    #[test]
    fn plain_click_selects_one() {
        let mut p = panel();
        let v = vis();
        p.click_select("b.txt", false, false, &v);
        assert_eq!(p.sel_names(), vec!["b.txt".to_string()]);
        assert_eq!(p.selected.as_deref(), Some("b.txt"));
    }

    #[test]
    fn ctrl_click_accumulates() {
        let mut p = panel();
        let v = vis();
        p.click_select("a.txt", false, false, &v);
        p.click_select("c.txt", true, false, &v);
        // The focus row stays selected; the toggled row joins it.
        assert_eq!(p.sel_names(), vec!["a.txt".to_string(), "c.txt".to_string()]);
        assert_eq!(p.selected.as_deref(), Some("c.txt"));
    }

    #[test]
    fn ctrl_click_again_toggles_off() {
        let mut p = panel();
        let v = vis();
        p.click_select("a.txt", false, false, &v);
        p.click_select("c.txt", true, false, &v);
        p.click_select("a.txt", true, false, &v); // toggle a.txt off
        assert_eq!(p.sel_names(), vec!["c.txt".to_string()]);
        // Focus stays on the row even though it is no longer highlighted.
        assert_eq!(p.selected.as_deref(), Some("a.txt"));
    }

    #[test]
    fn shift_click_selects_range() {
        let mut p = panel();
        let v = vis();
        p.click_select("a.txt", false, false, &v); // anchor = a.txt
        p.click_select("c.txt", false, true, &v); // Shift → a..c
        assert_eq!(
            p.sel_names(),
            vec!["a.txt".to_string(), "b.txt".to_string(), "c.txt".to_string()]
        );
        // Shift the other way round must walk backwards too.
        p.click_select("a.txt", false, true, &v); // anchor is still a.txt? no: c.txt
        assert_eq!(p.selected.as_deref(), Some("a.txt"));
    }

    #[test]
    fn plain_click_resets_multi() {
        let mut p = panel();
        let v = vis();
        p.click_select("a.txt", false, false, &v);
        p.click_select("b.txt", true, false, &v);
        p.click_select("d.txt", false, false, &v);
        assert_eq!(p.sel_names(), vec!["d.txt".to_string()]);
    }
}
