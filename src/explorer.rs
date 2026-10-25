//! `Explorer`: top-level state, filesystem operations and background jobs.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Instant;

use crate::app::{Clip, Entry, Panel};
use crate::cache::Store;
use crate::fs_util::{self, DriveInfo};

/// Messages from background work.
pub enum JobMsg {
    Progress(f32),
    /// Live count of items already deleted by a streaming job (e.g. the
    /// Recycle-Bin empty). Carries no language, so job threads stay agnostic;
    /// the UI renders it as "已删除 N".
    Deleted(u64),
    /// `(key, arg)`: `key` is a translation key (may contain `%s`), `arg` is
    /// substituted for `%s` when non-empty. Lets job threads stay language-agnostic.
    Done(&'static str, String),
    Failed(String),
}

/// Folders per pane queued for a size scan by default: roughly what fits on
/// screen — nobody looks at folder 300's size.
const FOLDERS_PER_PANEL: usize = 24;
/// Ceiling on in-flight size scans, so one huge listing cannot flood the
/// single worker thread.
const MAX_PENDING_SIZES: usize = 64;

/// A running background job (empty recycle bin, big copy/move, delete batch).
pub struct Job {
    pub label: String,
    pub progress: f32,
    /// True when the job cannot report real completion (e.g. the shell owns
    /// the actual work and only signals when it is done). The UI then draws an
    /// *indeterminate* animated bar instead of a percentage — used for the
    /// Recycle-Bin empty, where `SHEmptyRecycleBin` gives no per-file progress
    /// and we must avoid the shell's own (stall-prone) modal dialog.
    pub indeterminate: bool,
    /// Live count of items already deleted (Recycle-Bin empty). Shown in the
    /// progress bar as "已删除 N" so the user gets concrete progress without
    /// the shell's stall-prone enumeration.
    pub deleted: u64,
    pub note: Option<(&'static str, String)>,
    pub err: Option<String>,
    pub rx: Receiver<JobMsg>,
    /// When the job started: the finish line reports the elapsed time, so a
    /// 10-minute copy says so instead of just "done".
    pub started: Instant,
}

impl Job {
    fn finish(&mut self, m: JobMsg) {
        match m {
            JobMsg::Progress(p) => self.progress = p,
            JobMsg::Deleted(n) => self.deleted = n,
            JobMsg::Done(k, a) => self.note = Some((k, a)),
            JobMsg::Failed(e) => self.err = Some(e),
        }
    }
}

/// One browser-style tab: a pair of panels. When the app is not split, only
/// panel 0 is drawn, but both keep their state so re-splitting is instant.
pub struct Tab {
    pub id: u32,
    pub panes: Vec<Panel>,
}

impl Tab {
    fn new(id: u32, left: PathBuf, right: PathBuf) -> Self {
        Self { id, panes: vec![Panel::new(left), Panel::new(right)] }
    }
}

/// Select `name` in `panel` once its listing is in: right away when the
/// entries are already there (the inline first load), otherwise when the
/// worker delivers them. Only selects when the name really got listed — a
/// filter could hide it, or the folder may have failed to read.
fn select_when_listed(panel: &mut Panel, name: String) {
    if panel.loading {
        panel.pending_select = Some(name);
    } else if panel.entries.iter().any(|e| e.name == name) {
        panel.selected = Some(name.clone());
        panel.multi = std::iter::once(name).collect();
    }
}

/// Where a Quick-Access row came from — which group it is listed under and
/// whether Marin is allowed to remove it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum QaKind {
    /// One of Marin's own favourites: stored in `store.txt`, removable here.
    Favorite,
    /// Pinned by Windows Explorer: read from the jump list, read-only to us.
    WinPin,
    /// Windows' jump-list MRU entry — neither pinned nor ours.
    Recent,
}

/// State of the bottom-left folder-size box (see `Explorer::size_scan`).
pub enum SizeScanUi {
    /// Scanning: the folder being walked right now, plus the bytes counted so
    /// far. A full scan never pre-counts, so this is bytes — not a percentage.
    Running(String, u64),
    /// Finished: the ready-made line, elapsed time included. Stays on screen
    /// until the next manual recompute replaces it.
    Done(String),
}

pub struct Explorer {
    /// Tabs, browser style. Persisted, so the next launch reopens them all.
    pub tabs: Vec<Tab>,
    pub active_tab: usize,
    /// 0 = left pane, 1 = right pane (only meaningful when `split`).
    pub active_pane: usize,
    /// Two panes visible. Persisted; defaults to a single pane.
    pub split: bool,
    /// Split-pane divider position as a fraction of the centre width
    /// (0.5 = equal halves). Draggable; persisted.
    pub split_frac: f32,
    pub next_tab_id: u32,
    pub clip: Option<Clip>,
    pub status: String,
    /// Last `status` value we have seen; when it changes the pane status band
    /// lights up for a moment (a Ctrl+C has no other visual cue).
    pub status_prev: String,
    /// When `status` last changed.
    pub status_at: Instant,
    /// `status` changed within the last few seconds → draw it highlighted.
    pub status_fresh: bool,
    pub show_hidden: bool,
    // drives
    pub drives: Vec<DriveInfo>,
    pub drives_rx: Option<Receiver<Vec<DriveInfo>>>,
    pub drives_at: Option<Instant>,
    // recycle bin
    pub sid: Option<String>,
    pub sid_rx: Option<Receiver<Option<String>>>,
    /// Async recycle-bin listing: worker sends `(dir, items)`; a panel whose
    /// `rec_loading` is set adopts the result when it arrives.
    pub rec_tx: Sender<(PathBuf, Vec<fs_util::RecItem>)>,
    pub rec_rx: Receiver<(PathBuf, Vec<fs_util::RecItem>)>,
    pub pending_rec: HashSet<PathBuf>,
    // quick access (system jump-list pins + the user's own favourites)
    /// Windows' own Quick-Access entries as `(path, pinned)`; `pinned` is
    /// Explorer's pin flag, so its "已固定" group can be mirrored exactly.
    pub system_qa: Vec<(PathBuf, bool)>,
    /// Marin's own list ("收藏"), persisted in `store.txt` under `qa_pins`.
    ///
    /// Renamed from "pin" on purpose: these are ours alone and can be added /
    /// removed anywhere, while Windows' pins live in the jump list and are
    /// read-only to us.
    pub favorites: Vec<PathBuf>,
    pub qa_rx: Option<Receiver<Vec<(PathBuf, bool)>>>,
    /// Quick-Access list, rebuilt only when `qa_dirty` (a favourite or
    /// Windows pin changed). The rail used to rebuild it every frame with an
    /// O(n²) de-dup; now it clones this cached vec instead.
    pub qa_cache: Vec<(PathBuf, QaKind)>,
    pub qa_dirty: bool,
    // folder sizes
    pub store: Store,
    pub size_tx: Sender<(PathBuf, bool)>, // (path, full_scan)
    pub size_rx: Receiver<(PathBuf, u64, u64, bool, bool)>, // (path, mtime, size, complete, was_full)
    /// Live progress of a full (manual recompute) scan: (folder, bytes so far).
    /// A full scan never pre-counts, so the total is unknown — this is bytes,
    /// not a percentage.
    pub size_prog_rx: Receiver<(PathBuf, u64)>,
    /// The bottom-left size box. It deliberately **survives completion**: the
    /// finish line ("计算完成 · 耗时 X") stays on screen until the next manual
    /// recompute, instead of vanishing or overwriting the operation log at the
    /// far left of the status bar.
    pub size_scan: Option<SizeScanUi>,
    /// When the current manual recompute batch started; the finish line
    /// reports how long the whole batch took.
    pub size_scan_started: Option<Instant>,
    /// Outstanding full scans from the manual batch; zero means done.
    pub size_full_left: usize,
    /// Bytes counted by the current manual batch's full-scan results. Summed
    /// as results land; the finish line reports it ("已扫 X GiB") so the user
    /// sees the volume the exact numbers cost. Reset when a batch starts.
    pub size_full_bytes: u64,
    pub pending_sizes: HashSet<PathBuf>,
    /// Wall clock of the last periodic folder-size top-up (see
    /// `refresh_folder_sizes_throttled`).
    pub last_size_poll: Instant,
    /// Wall clock of the last inbox check (see `poll_inbox_throttled`):
    /// paths a second launch handed over while this instance was running.
    pub last_inbox_poll: Instant,
    /// Set whenever a background result lands. The UI needs one more repaint
    /// *after* a queue drains (the last size, the last listing), and egui is
    /// otherwise idle by then.
    pub bg_signal: Option<Instant>,
    // directory listings (read on a worker so a huge folder can't freeze the UI)
    pub list_tx: Sender<(PathBuf, u64)>,
    pub list_rx: Receiver<(PathBuf, u64, Vec<Entry>)>,
    // background job
    pub job: Option<Job>,
    // dialogs
    pub delete_open: bool,
    pub delete_targets: Vec<PathBuf>,
    pub delete_rec_suffix: Option<String>,
    /// How `confirm_delete` disposes of `delete_targets`: true = permanent,
    /// false = move to the Recycle Bin. Set by whichever opener (Shift+Delete
    /// or the context menu) armed the dialog.
    pub delete_permanent: bool,
    pub empty_open: bool,
    pub new_folder_open: bool,
    pub new_folder_name: String,
    pub go_open: bool,
    pub go_path: String,
    pub go_target: bool,
    pub rec_info: Option<RecInfo>,
    // rename / properties
    pub rename_open: bool,
    pub rename_name: String,
    pub rename_target: Option<PathBuf>,
    pub props: Option<PropsInfo>,
    // image preview (thumbnails / side pane / full viewer)
    pub previews: crate::preview::Previews,
    pub show_preview: bool,
    pub viewer: Option<PathBuf>,
    pub viewer_zoom: f32,
    /// Built-in viewer: the list of images in the folder the viewer was opened
    /// from, so ← / → can walk through them. `viewer_index` selects the shown.
    pub viewer_list: Vec<PathBuf>,
    pub viewer_index: usize,
    /// Drag-pan offset of the zoomed viewer image (reset on nav / zoom).
    pub viewer_pan: egui::Vec2,
    /// Last time the mouse wheel switched images in the viewer; one notch =
    /// one step, so a long continuous scroll doesn't skip a dozen images.
    pub viewer_wheel_at: Option<std::time::Instant>,
    /// Cached side-pane text preview: (path, text). Files up to 10 KiB are
    /// read once and kept, so a repaint never hits the disk again.
    pub preview_text: Option<(PathBuf, String)>,
    /// Image → ICO / PNG conversion dialog (opened from the viewer's
    /// right-click menu). Holds the decoded source until it closes.
    pub convert: Option<crate::convert::ConvertState>,
            // ---- appearance / i18n (persisted) ----
            pub dark: bool,
    pub lang: crate::lang::Lang,
    /// UI font scale (1.0 = default). Persisted as `font_scale`.
    pub font_scale: f32,
    /// Custom text colours, one pair per theme. Persisted as `text_dark`,
    /// `weak_dark`, `text_light`, `weak_light` (each "r,g,b,a"). `weak` is the
    /// dimmer secondary text (counts, hints, captions); `text` is the main
    /// body / label colour. Defaults keep the old readable look when unset.
    pub text_dark: egui::Color32,
    pub weak_dark: egui::Color32,
    pub text_light: egui::Color32,
    pub weak_light: egui::Color32,
    /// Theme / font-scale already pushed to egui this run. `update()` only
    /// re-applies `set_visuals` / `set_style` when these differ from the
    /// current setting, instead of rebuilding the whole style every frame.
    pub applied_dark: Option<bool>,
    pub applied_font_scale: f32,
    /// Icon source: 0 = shell, 1 = built-in vector, 2 = font emoji.
    /// Persisted as `icon_mode`; the two light modes never touch the shell.
    pub icon_mode: u8,
    /// Selected UI font key — a font file name in `%WINDIR%\Fonts`
    /// (e.g. "simhei.ttf"). Default `DEFAULT_FONT`.
    /// `reload_font` applies a change on the next frame, because rebuilding the
    /// font definitions needs the egui context.
    pub ui_font: String,
    /// Set when the UI font (or language) changed: `update()` rebuilds the font definitions.
    pub reload_font: bool,
    /// Lazy cache of installable system fonts (key, display name). `None` until
    /// the font dropdown is first opened, so we never scan `%WINDIR%\Fonts`
    /// unless the user actually reaches for the picker.
    pub avail_fonts: Option<Vec<(String, String)>>,
    /// Font dropdown expand state (persists across frames inside the settings popup).
    pub font_picker_open: bool,
    /// Whether the Explorer context-menu entry ("通过 MarinExplorer 打开")
    /// is registered under `HKCU\Software\Classes`. The value is re-read from
    /// the registry whenever the toggle is used, so it reflects reality even
    /// if something outside the app changed it.
    pub ctx_menu: bool,
    pub last_flush: Instant,
    /// Set every frame: true when the filter text-edit currently has focus.
    pub text_focus: bool,
    // ---- list-view column widths (user-resizable; persisted) ----
    // The 修改日期 column is not stored: it flexes to absorb the leftover
    // horizontal space, so the table never overflows or leaves a gap.
    pub col_name: f32,
    pub col_type: f32,
    pub col_size: f32,
    pub col_time: f32,
    // ---- native (full-colour) file icons, cached as egui textures ----
    pub icons: crate::shellicon::Icons,
    /// The OS window handle (Windows only). Lets us repaint the title bar to
    /// match the in-app theme; `None` until `main()` grabs it.
    pub hwnd: Option<isize>,
    // ---- inline address editor ----
    /// Which pane is typing a path right now (`true` = left).
    pub addr_edit: Option<bool>,
    pub addr_text: String,
    /// True on the frame the editor opened, so it can grab the keyboard focus
    /// exactly once.
    pub addr_fresh: bool,
    /// Set when Enter commits the address editor. Swallows the same key
    /// press in the global keyboard handler (it must not also open the
    /// selected row on the frame the address jumps).
    pub enter_consumed: bool,
}

/// Info shown in the "属性" window.
#[derive(Clone)]
pub struct PropsInfo {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: u64,
    /// Creation time, unix seconds (0 when unknown).
    pub ctime: u64,
    pub entries: Option<usize>,
}

/// Info shown when a recycle item is double-clicked.
#[derive(Clone)]
pub struct RecInfo {
    pub name: String,
    /// Original location the item was deleted from ("" when unknown).
    pub orig_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub deleted_at: u64,
    pub suffix: String,
}

fn recycle_root(drive: &std::path::Path, sid: &str) -> PathBuf {
    drive.join("$Recycle.Bin").join(sid)
}

/// Which custom text colour a settings picker is editing.
#[derive(Clone, Copy)]
pub enum TextColor {
    TextDark,
    WeakDark,
    TextLight,
    WeakLight,
}

impl Explorer {
    pub fn new(
        store: Store,
        size_tx: Sender<(PathBuf, bool)>,
        size_prog_rx: Receiver<(PathBuf, u64)>,
        size_rx: Receiver<(PathBuf, u64, u64, bool, bool)>,
    ) -> Self {
        let (rec_tx, rec_rx) = channel::<(PathBuf, Vec<fs_util::RecItem>)>();
        // Directory-listing worker: one persistent thread. A folder with tens
        // of thousands of entries blocks for seconds when read inline, so the
        // panels hand their cwd over here and adopt the result later.
        let (list_tx, list_rx) = channel::<(PathBuf, u64, Vec<Entry>)>();
        let (list_req_tx, list_req_rx) = channel::<(PathBuf, u64)>();
        thread::spawn(move || {
            while let Ok((dir, generation)) = list_req_rx.recv() {
                let entries = crate::app::read_entries(&dir);
                list_tx.send((dir, generation, entries)).ok();
            }
        });
        let fallback = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let split = store.meta("split").map(|s| s == "1").unwrap_or(false);
        let split_frac = store
            .meta("split_frac")
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.5)
            .clamp(0.15, 0.85);
        let store_show_preview = store.meta("show_preview") == Some("1");

        // ---- restore the tab list (order comes from `tab_ids`) ----
        let ids: Vec<u32> = store
            .meta("tab_ids")
            .map(|s| {
                s.split(',')
                    .filter_map(|x| x.trim().parse::<u32>().ok())
                    .collect()
            })
            .unwrap_or_default();
        let mut tabs: Vec<Tab> = Vec::new();
        for id in &ids {
            let l = store
                .meta(&format!("tab{id}_cwd"))
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| fallback.clone());
            let r = store
                .meta(&format!("tab{id}_cwd_r"))
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| l.clone());
            tabs.push(Tab::new(*id, l, r));
        }
        if tabs.is_empty() {
            // First run (or a stale store): migrate the legacy single-pane keys.
            let l = store
                .meta("cwd_left")
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| fallback.clone());
            let r = store
                .meta("cwd_right")
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| l.clone());
            tabs.push(Tab::new(1, l, r));
        }
        let active_tab = store
            .meta("tab_active")
            .and_then(|s| s.parse::<usize>().ok())
            .filter(|&i| i < tabs.len())
            .unwrap_or(0);
        let next_tab_id = tabs.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        // Default to dark mode when the preference has never been set.
        let dark = store.meta("dark").map_or(true, |v| v == "1");
        let lang = crate::lang::Lang::from_id(store.meta("lang").as_deref().unwrap_or("zh"));
        let font_scale = store
            .meta("font_scale")
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(1.12)
            .clamp(0.8, 2.0);
        let icon_mode = store
            .meta("icon_mode")
            .and_then(|v| v.parse::<u8>().ok())
            .unwrap_or(0);
        let ui_font =
            crate::normalize_font_key(store.meta("ui_font").unwrap_or(crate::DEFAULT_FONT));
        let text_dark = Self::meta_color(&store, "text_dark").unwrap_or(egui::Color32::from_gray(240));
        let weak_dark = Self::meta_color(&store, "weak_dark").unwrap_or(egui::Color32::from_gray(170));
        let text_light = Self::meta_color(&store, "text_light").unwrap_or(egui::Color32::from_gray(20));
        let weak_light = Self::meta_color(&store, "weak_light").unwrap_or(egui::Color32::from_gray(120));
        // Read-only probe of the actual registry state (the app never writes it).
        let ctx_menu = crate::shell::is_installed();

        let mut me = Self {
            tabs,
            active_tab,
            active_pane: 0,
            split,
            split_frac,
            next_tab_id,
            clip: None,
            status_prev: String::new(),
            status_at: Instant::now(),
            status_fresh: false,
            status: String::new(),
            show_hidden: false,
            drives: vec![],
            drives_rx: None,
            drives_at: None,
            sid: store.meta("sid").map(|s| s.to_string()),
            sid_rx: None,
            rec_tx,
            rec_rx,
            pending_rec: HashSet::new(),
            system_qa: vec![],
            favorites: store.qa_pins(),
            qa_rx: None,
            qa_cache: vec![],
            qa_dirty: true,
            store,
            size_tx,
            size_rx,
            size_prog_rx,
            size_scan: None,
            size_scan_started: None,
            size_full_left: 0,
            size_full_bytes: 0,
            pending_sizes: HashSet::new(),
            last_size_poll: Instant::now(),
            // Back-dated so the first frame already drains the inbox: a
            // path handed over by the launch that is replacing us must not
            // wait a full interval to open.
            last_inbox_poll: Instant::now() - std::time::Duration::from_millis(500),
            bg_signal: None,
            list_tx: list_req_tx,
            list_rx,
            job: None,
            delete_open: false,
            delete_targets: Vec::new(),
            delete_rec_suffix: None,
            delete_permanent: true,
            empty_open: false,
            new_folder_open: false,
            new_folder_name: String::new(),
            go_open: false,
            go_path: String::new(),
            go_target: false,
            rec_info: None,
            rename_open: false,
            rename_name: String::new(),
            rename_target: None,
            props: None,
            previews: crate::preview::Previews::new(),
            show_preview: store_show_preview,
            viewer: None,
            viewer_zoom: 0.0,
            viewer_list: vec![],
            viewer_index: 0,
            viewer_pan: egui::Vec2::ZERO,
            viewer_wheel_at: None,
            preview_text: None,
            convert: None,
            dark,
            lang,
            font_scale,
            text_dark,
            weak_dark,
            text_light,
            weak_light,
            applied_dark: None,
            applied_font_scale: -1.0,
            icon_mode,
            ui_font,
            reload_font: false,
            avail_fonts: None,
            font_picker_open: false,
            ctx_menu,
            last_flush: Instant::now(),
            text_focus: false,
            col_name: crate::DEF_NAME,
            col_type: crate::DEF_TYPE,
            col_size: crate::DEF_SIZE,
            col_time: crate::DEF_TIME,
            icons: crate::shellicon::Icons::new(),
            // Filled in by `main()` once the native window exists.
            hwnd: None,
            addr_edit: None,
            addr_text: String::new(),
            addr_fresh: false,
            enter_consumed: false,
        };
        me.icons
            .set_mode(crate::shellicon::IconMode::from_id(icon_mode));
        me.restore_layout();
        me.restore_columns();
        // From here on every navigation reads its listing on the worker
        // thread (the construction-time loads above ran inline).
        for tab in &mut me.tabs {
            for p in &mut tab.panes {
                p.list_tx = Some(me.list_tx.clone());
            }
        }
        me.status = format!(
            "{} {}",
            crate::lang::tr("就绪。", me.lang),
            crate::lang::tr("单击选中，双击打开；Alt+←/→ 前进后退；Ctrl+T 新标签页。", me.lang)
        );
        me
    }

    // ---------- panes / tabs ----------

    pub fn p0(&mut self) -> &mut Panel {
        &mut self.tabs[self.active_tab].panes[0]
    }

    pub fn p1(&mut self) -> &mut Panel {
        &mut self.tabs[self.active_tab].panes[1]
    }

    /// Panel 0 for the left side, panel 1 for the right side.
    pub fn pane_of(&mut self, is_left: bool) -> &mut Panel {
        if is_left {
            self.p0()
        } else {
            self.p1()
        }
    }

    pub fn active_panel(&mut self) -> &mut Panel {
        if self.split && self.active_pane == 1 {
            self.p1()
        } else {
            self.p0()
        }
    }

    /// Which pane takes input when split (and where side effects land).
    pub fn set_active_pane(&mut self, is_left: bool) {
        if is_left || !self.split {
            self.active_pane = 0;
        } else {
            self.active_pane = 1;
        }
    }

    /// Toggle the two-pane layout (persisted).
    pub fn set_split(&mut self, on: bool) {
        if on == self.split {
            return;
        }
        self.split = on;
        if !on {
            self.active_pane = 0;
        } else {
            // Make sure the second pane has a listing ready.
            let fresh = self.p1().entries.is_empty();
            if fresh {
                let c = self.p0().cwd.clone();
                self.p1().load(&c);
            }
        }
        self.persist_layout();
    }

    /// Make sure the two-pane layout is on. Used by the "open in the right
    /// pane" menu entries, which must work from single-pane mode too.
    pub fn ensure_split(&mut self) {
        if !self.split {
            self.set_split(true);
        }
    }

    /// Show tab `idx`'s folder in the right-hand pane (split turns itself on
    /// when it was off) and make that pane the active one.
    pub fn open_tab_in_right_pane(&mut self, idx: usize) {
        let Some(tab) = self.tabs.get(idx) else {
            return;
        };
        let dir = tab.panes[0].cwd.clone();
        self.ensure_split();
        self.p1().navigate(dir.clone());
        self.set_active_pane(false);
        self.apply_cached_sizes();
        self.persist_layout();
        self.status = format!(
            "{} {}",
            crate::lang::tr("在右栏打开", self.lang),
            dir.display()
        );
    }

    /// Open `path` in a brand-new tab and switch to it.
    pub fn open_path_in_new_tab(&mut self, path: std::path::PathBuf) {
        self.new_tab();
        self.p0().navigate(path.clone());
        self.apply_cached_sizes();
        self.persist_layout();
        self.status = format!(
            "{} {}",
            crate::lang::tr("新标签页", self.lang),
            path.display()
        );
    }

    /// Show `path` in the right-hand pane (split turns itself on when it was
    /// off) and make that pane the active one.
    pub fn open_path_in_right_pane(&mut self, path: std::path::PathBuf) {
        self.ensure_split();
        self.p1().navigate(path.clone());
        self.set_active_pane(false);
        self.apply_cached_sizes();
        self.persist_layout();
        self.status = format!(
            "{} {}",
            crate::lang::tr("在右栏打开", self.lang),
            path.display()
        );
    }

    /// Close every tab except `keep`.
    pub fn close_other_tabs(&mut self, keep: usize) {
        let Some(tab) = self.tabs.get(keep) else {
            return;
        };
        let id = tab.id;
        self.tabs.retain(|t| t.id == id);
        self.active_tab = 0;
        self.active_pane = 0;
        self.store.meta_retain_tabs(&[id]);
        self.status = crate::lang::tr("已关闭其他标签页", self.lang).into();
        self.persist_layout();
    }

    /// Close tab `idx` and every tab after it.
    pub fn close_tabs_to_right(&mut self, idx: usize) {
        if idx + 1 >= self.tabs.len() {
            return;
        }
        let ids: Vec<u32> = self.tabs[..=idx].iter().map(|t| t.id).collect();
        self.tabs.truncate(idx + 1);
        if self.active_tab > idx {
            self.active_tab = idx;
        }
        self.active_pane = 0;
        self.store.meta_retain_tabs(&ids);
        self.status = crate::lang::tr("已关闭右侧标签页", self.lang).into();
        self.persist_layout();
    }

    /// Open a new tab (a clone of the active one) and switch to it.
    pub fn new_tab(&mut self) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let (l, r) = (self.p0().cwd.clone(), self.p1().cwd.clone());
        self.tabs.insert(self.active_tab + 1, Tab::new(id, l, r));
        self.active_tab += 1;
        self.active_pane = 0;
        self.sync_new_tab_prefs(self.active_tab);
        // The new panels were built with `list_tx: None` (inline load); wire
        // them to the worker like every other panel.
        let tx = self.list_tx.clone();
        for p in &mut self.tabs[self.active_tab].panes {
            p.list_tx = Some(tx.clone());
        }
        self.status = format!("{} {id}", crate::lang::tr("新标签页", self.lang));
        self.persist_layout();
    }

    /// Close tab `idx` (never closes the last one).
    pub fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 {
            self.status = crate::lang::tr("至少保留一个标签页", self.lang).into();
            return;
        }
        let id = self.tabs[idx].id;
        self.tabs.remove(idx);
        if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        self.active_pane = 0;
        self.store.meta_retain_tabs(&[id]);
        self.status = format!("{} {id}", crate::lang::tr("已关闭标签页", self.lang));
        self.persist_layout();
    }

    /// Restore the per-pane-side sort key / direction / view mode and the
    /// global toggles from the on-disk store. Sort and view are remembered
    /// *per pane* (left vs right), not per tab, so switching tabs never
    /// changes how a pane is sorted — only the folder changes.
    fn restore_layout(&mut self) {
        let read = |side: &str| -> (crate::app::SortKey, crate::app::SortDir, crate::app::View) {
            let sort = self
                .store
                .meta(&format!("pane_{side}_sort"))
                .map(crate::app::SortKey::from_id)
                .unwrap_or(crate::app::SortKey::Time);
            let dir = self
                .store
                .meta(&format!("pane_{side}_sortd"))
                .map(crate::app::SortDir::from_id)
                .unwrap_or(crate::app::SortDir::Desc);
            let view = self
                .store
                .meta(&format!("pane_{side}_view"))
                .map(crate::app::View::from_id)
                .unwrap_or(crate::app::View::List);
            (sort, dir, view)
        };
        let (l_sk, l_sd, l_vw) = read("left");
        let (r_sk, r_sd, r_vw) = read("right");
        for tab in &mut self.tabs {
            let p = &mut tab.panes[0];
            p.sort_key = l_sk;
            p.sort_dir = l_sd;
            p.view = l_vw;
            p.resort();
            let p = &mut tab.panes[1];
            p.sort_key = r_sk;
            p.sort_dir = r_sd;
            p.view = r_vw;
            p.resort();
        }
        self.show_hidden = self.store.meta("show_hidden") == Some("1");
    }

    /// Copy the active tab's per-side sort/view onto a freshly created tab so a
    /// new tab opens with the same pane arrangement the user left.
    fn sync_new_tab_prefs(&mut self, idx: usize) {
        let (l_sk, l_sd, l_vw, r_sk, r_sd, r_vw) = {
            let a = &self.tabs[self.active_tab];
            (
                a.panes[0].sort_key,
                a.panes[0].sort_dir,
                a.panes[0].view,
                a.panes[1].sort_key,
                a.panes[1].sort_dir,
                a.panes[1].view,
            )
        };
        let tab = &mut self.tabs[idx];
        tab.panes[0].sort_key = l_sk;
        tab.panes[0].sort_dir = l_sd;
        tab.panes[0].view = l_vw;
        tab.panes[0].resort();
        tab.panes[1].sort_key = r_sk;
        tab.panes[1].sort_dir = r_sd;
        tab.panes[1].view = r_vw;
        tab.panes[1].resort();
    }

    /// Apply a sort (key + direction) to *every* panel on one side and resort
    /// them, so the setting stays consistent across all tabs.
    fn apply_side_sort(&mut self, is_left: bool, key: crate::app::SortKey, dir: crate::app::SortDir) {
        for tab in &mut self.tabs {
            let p = &mut tab.panes[if is_left { 0 } else { 1 }];
            p.sort_key = key;
            p.sort_dir = dir;
            p.resort();
        }
    }

    /// Set the view mode (List / Grid) for *every* panel on one side, so it
    /// too is remembered per pane and does not flip when switching tabs.
    pub fn set_view(&mut self, is_left: bool, view: crate::app::View) {
        for tab in &mut self.tabs {
            tab.panes[if is_left { 0 } else { 1 }].view = view;
        }
        self.persist_layout();
    }

    /// Persist the tab list and every layout bit that must survive a restart.
    pub fn persist_layout(&mut self) {
        let ids: Vec<String> = self.tabs.iter().map(|t| t.id.to_string()).collect();
        self.store.set_meta("tab_ids", &ids.join(","));
        self.store.set_meta("tab_active", &self.active_tab.to_string());
        self.store.set_meta("split", if self.split { "1" } else { "0" });
        self.store
            .set_meta("split_frac", &format!("{:.4}", self.split_frac));
        for tab in &self.tabs {
            for (i, key) in [(0usize, "cwd"), (1usize, "cwd_r")] {
                let p = &tab.panes[i];
                self.store.set_meta(
                    &format!("tab{}_{}_cwd", tab.id, key),
                    &p.cwd.to_string_lossy(),
                );
            }
        }
        // Sort + view are remembered per pane (left / right), shared across all
        // tabs, so they persist independently of which folder each tab shows.
        let (l_sk, l_sd, l_vw, r_sk, r_sd, r_vw) = {
            let a = &self.tabs[self.active_tab];
            (
                a.panes[0].sort_key.id(),
                a.panes[0].sort_dir.id(),
                a.panes[0].view.id(),
                a.panes[1].sort_key.id(),
                a.panes[1].sort_dir.id(),
                a.panes[1].view.id(),
            )
        };
        self.store.set_meta("pane_left_sort", l_sk);
        self.store.set_meta("pane_left_sortd", l_sd);
        self.store.set_meta("pane_left_view", l_vw);
        self.store.set_meta("pane_right_sort", r_sk);
        self.store.set_meta("pane_right_sortd", r_sd);
        self.store.set_meta("pane_right_view", r_vw);
        self.persist_columns();
        self.store
            .set_meta("show_hidden", if self.show_hidden { "1" } else { "0" });
        self.store
            .set_meta("show_preview", if self.show_preview { "1" } else { "0" });
        self.store.set_meta("dark", if self.dark { "1" } else { "0" });
        self.store.set_meta("lang", self.lang.id());
        // Legacy keys, kept in sync so an older build still finds a cwd.
        let legacy_l = self.p0().cwd.to_string_lossy().into_owned();
        let legacy_r = self.p1().cwd.to_string_lossy().into_owned();
        self.store.set_meta("cwd_left", &legacy_l);
        self.store.set_meta("cwd_right", &legacy_r);
    }

    /// Read the persisted list-column widths (name/type/size/time). Falls back
    /// to the built-in defaults when absent or un-parseable.
    fn restore_columns(&mut self) {
        let parse = |k: &str, d: f32| {
            self.store
                .meta(k)
                .and_then(|s| s.parse::<f32>().ok())
                .unwrap_or(d)
        };
        self.col_name = parse("col_name", crate::DEF_NAME);
        self.col_type = parse("col_type", crate::DEF_TYPE);
        self.col_size = parse("col_size", crate::DEF_SIZE);
        self.col_time = parse("col_time", crate::DEF_TIME);
    }

    /// Persist the list-column widths so a resize survives a restart.
    pub(crate) fn persist_columns(&mut self) {
        self.store.set_meta("col_name", &self.col_name.to_string());
        self.store.set_meta("col_type", &self.col_type.to_string());
        self.store.set_meta("col_size", &self.col_size.to_string());
        self.store.set_meta("col_time", &self.col_time.to_string());
    }

    // ---------- appearance / i18n ----------

    /// Parse a "r,g,b,a" meta string into a colour. `a` defaults to 255.
    fn meta_color(store: &crate::cache::Store, key: &str) -> Option<egui::Color32> {
        let s = store.meta(key)?;
        let mut it = s.split(',').map(|p| p.trim().parse::<u8>().ok());
        let r = it.next()??;
        let g = it.next()??;
        let b = it.next()??;
        let a = it.next().unwrap_or(Some(255))?;
        Some(egui::Color32::from_rgba_unmultiplied(r, g, b, a))
    }

    /// Format a colour as the "r,g,b,a" string stored in meta.
    fn color_meta(c: egui::Color32) -> String {
        let [r, g, b, a] = c.to_srgba_unmultiplied();
        format!("{},{},{},{}", r, g, b, a)
    }

    /// Main body / label text colour for the active theme.
    pub fn text_color(&self) -> egui::Color32 {
        if self.dark {
            self.text_dark
        } else {
            self.text_light
        }
    }

    /// Secondary (dimmer) text colour for the active theme.
    pub fn weak_color(&self) -> egui::Color32 {
        if self.dark {
            self.weak_dark
        } else {
            self.weak_light
        }
    }

    /// Persist one of the four custom text colours and re-apply the theme.
    pub fn set_text_color(&mut self, which: TextColor, c: egui::Color32) {
        match which {
            TextColor::TextDark => {
                self.text_dark = c;
                self.store.set_meta("text_dark", &Self::color_meta(c));
            }
            TextColor::WeakDark => {
                self.weak_dark = c;
                self.store.set_meta("weak_dark", &Self::color_meta(c));
            }
            TextColor::TextLight => {
                self.text_light = c;
                self.store.set_meta("text_light", &Self::color_meta(c));
            }
            TextColor::WeakLight => {
                self.weak_light = c;
                self.store.set_meta("weak_light", &Self::color_meta(c));
            }
        }
        // Force `update()` to re-push the visuals next frame.
        self.applied_dark = None;
    }

    /// Translate a UI string for the current language. Note: call sites use
    /// `crate::lang::tr(key, self.lang)` directly, because `tr` only reads the
    /// `self.lang` field and so can appear in the same expression as
    /// `&mut self.<field>` — a `&self` method would borrow all of `self` and
    /// conflict with it.
    #[allow(dead_code)]
    pub fn t(&self, key: &'static str) -> &'static str {
        let lang = self.lang;
        crate::lang::tr(key, lang)
    }

    /// Switch language and persist it.
    pub fn set_lang(&mut self, lang: crate::lang::Lang) {
        self.lang = lang;
        self.store.set_meta("lang", lang.id());
        // The Japanese fallback font depends on the language, so rebuild the
        // font definitions on the next frame.
        self.reload_font = true;
        // The lazy font-list cache was built with the old language's display
        // names; drop it so the picker re-scans in the new language.
        self.avail_fonts = None;
        // The context-menu text lives in the registry, written once at install
        // time. Rewrite it so the menu follows the UI language.
        if self.ctx_menu {
            self.set_ctx_menu(true);
        }
    }

    /// Switch light/dark and persist it.
    pub fn set_dark(&mut self, on: bool) {
        self.dark = on;
        self.store.set_meta("dark", if on { "1" } else { "0" });
        self.apply_frame_theme();
    }

    /// Set the UI font scale and persist it.
    pub fn set_font_scale(&mut self, scale: f32) {
        self.font_scale = scale.clamp(0.8, 2.0);
        self.store.set_meta("font_scale", &format!("{:.2}", self.font_scale));
    }

    /// Switch icon source (0 shell / 1 vector / 2 emoji) and persist it.
    ///
    /// Leaving shell mode drops the whole texture cache, so that memory goes
    /// back immediately instead of waiting for the process to exit.
    pub fn set_icon_mode(&mut self, mode: u8) {
        self.icon_mode = mode;
        self.store.set_meta("icon_mode", &mode.to_string());
        self.icons.set_mode(crate::shellicon::IconMode::from_id(mode));
        self.status = crate::lang::tr("图标模式已切换", self.lang).into();
    }

    /// Switch the UI font and persist it. The swap is done on the next frame
    /// (`reload_font`), because rebuilding the font definitions needs the egui
    /// context.
    pub fn set_ui_font(&mut self, key: String) {
        self.ui_font = key.clone();
        self.store.set_meta("ui_font", &key);
        self.reload_font = true;
    }

    /// Turn the "通过 MarinExplorer 打开" context menu on or off.
    ///
    /// Writes `HKCU\Software\Classes` — the per-user hive, so this works
    /// without administrator rights and never prompts UAC. Anything that goes
    /// wrong is reported in the status bar instead of failing silently.
    pub fn set_ctx_menu(&mut self, on: bool) {
        let label = crate::lang::tr("通过 MarinExplorer 打开", self.lang);
        let result = if on {
            crate::shell::install(&label)
        } else {
            crate::shell::uninstall()
        };
        match result {
            Ok(()) => {
                self.ctx_menu = crate::shell::is_installed();
                self.status = if self.ctx_menu {
                    crate::lang::tr("右键菜单已添加", self.lang).into()
                } else {
                    crate::lang::tr("右键菜单已移除", self.lang).into()
                };
            }
            Err(e) => {
                // Re-read the real state so the badge is honest even after a
                // partial failure.
                self.ctx_menu = crate::shell::is_installed();
                self.status = format!("{}: {}", crate::lang::tr("右键菜单操作失败", self.lang), e);
            }
        }
    }

    /// Open a path handed to us on the command line (Explorer context menu,
    /// drag-and-drop onto the exe, `start MarinExplorer D:\`). Directories are
    /// opened directly; for a file we open its parent and select the file,
    /// which is what users expect from "open this file's folder".
    ///
    /// This is the **first launch** behaviour: the path lands in the left
    /// pane of the initial tab. When an instance is already running the path
    /// is handed over instead and opens a new tab (`open_arg_in_tab`).
    pub fn open_arg(&mut self, arg: &str) {
        let Some((dir, select)) = Self::resolve_arg(arg) else {
            return;
        };
        self.pane_of(true).navigate(dir);
        if let Some(name) = select {
            select_when_listed(self.pane_of(true), name);
        }
        self.apply_cached_sizes();
    }

    /// Split a command-line argument into the directory to open and the file
    /// to select inside it. `None` when the path exists as neither a folder
    /// nor a file (a stale favourite, a typo) — nothing to open.
    fn resolve_arg(arg: &str) -> Option<(std::path::PathBuf, Option<String>)> {
        let raw = arg.trim_matches('"');
        let p = std::path::PathBuf::from(raw);
        if p.is_dir() {
            Some((p, None))
        } else if p.is_file() {
            let parent = p.parent()?.to_path_buf();
            let name = p.file_name().map(|n| n.to_string_lossy().to_string())?;
            Some((parent, Some(name)))
        } else {
            None
        }
    }

    /// Open a path in a **new tab** — the tab starts life on that path
    /// (title included), it is not a clone of the current one that then
    /// navigates. Used when a second launch hands a path over (see
    /// `crate::single`).
    ///
    /// If some tab already shows that directory (either pane, compared
    /// case-insensitively — Windows paths are), that tab is brought forward
    /// instead: right-clicking the same folder twice must not fill the tab
    /// strip with duplicates.
    pub fn open_arg_in_tab(&mut self, arg: &str) {
        let Some((dir, select)) = Self::resolve_arg(arg) else {
            return;
        };
        let key = dir.to_string_lossy().to_lowercase();
        if let Some(idx) = self.tabs.iter().position(|t| {
            t.panes
                .iter()
                .any(|p| p.cwd.to_string_lossy().to_lowercase() == key)
        }) {
            self.active_tab = idx;
            self.active_pane = 0;
            self.persist_layout();
            return;
        }
        self.new_tab_at(dir, select);
    }

    /// Open a new tab that starts on `dir`: the right pane keeps the active
    /// tab's right pane, so the two-pane context survives, and `select` (a
    /// file inside `dir`) is selected once the listing is in.
    pub fn new_tab_at(&mut self, dir: std::path::PathBuf, select: Option<String>) {
        let id = self.next_tab_id;
        self.next_tab_id += 1;
        let right = self.p1().cwd.clone();
        // Built with the target cwd from the start: `Tab::new` reads the
        // listing inline (no worker wired yet), so the first frame already
        // shows the folder and the tab title is right from birth — no
        // clone-then-navigate flash of the old location.
        self.tabs
            .insert(self.active_tab + 1, Tab::new(id, dir, right));
        self.active_tab += 1;
        self.active_pane = 0;
        self.sync_new_tab_prefs(self.active_tab);
        // The new panels were built with `list_tx: None` (inline load); wire
        // them to the worker like every other panel.
        let tx = self.list_tx.clone();
        for p in &mut self.tabs[self.active_tab].panes {
            p.list_tx = Some(tx.clone());
        }
        if let Some(name) = select {
            select_when_listed(&mut self.tabs[self.active_tab].panes[0], name);
        }
        self.status = format!("{} {id}", crate::lang::tr("新标签页", self.lang));
        self.persist_layout();
    }

    /// Paths a second launch handed over through the inbox, at most every
    /// 400ms: a `read_dir` over a near-empty box is free, but there is no
    /// reason to do it per frame either. Each path opens its own tab
    /// (`open_arg_in_tab`), so a burst of right-clicks lands as a burst of
    /// tabs, in launch order.
    pub fn poll_inbox_throttled(&mut self) {
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(400);
        if self.last_inbox_poll.elapsed() < INTERVAL {
            return;
        }
        self.last_inbox_poll = std::time::Instant::now();
        for p in crate::single::take_inbox() {
            self.open_arg_in_tab(&p.to_string_lossy());
        }
    }

    /// Repaint the OS window frame (title bar + border) to match the theme.
    /// This is the reason the app needs its native window handle: egui can
    /// only theme the client area, so without this a dark app still sits in a
    /// bright white frame.
    pub fn apply_frame_theme(&self) {
        if let Some(hwnd) = self.hwnd {
            crate::shellicon::set_dark_frame(hwnd, self.dark);
        }
    }

    // ---------- inline address editor ----------

    /// Start typing a path in `is_left`'s address row (double-click on empty
    /// space, or the pencil button).
    pub fn begin_addr_edit(&mut self, is_left: bool) {
        self.set_active_pane(is_left);
        self.addr_text = self.pane_of(is_left).cwd.to_string_lossy().into_owned();
        self.addr_edit = Some(is_left);
        self.addr_fresh = true;
    }

    /// Jump to the typed path. A bad path is reported in the status bar and
    /// leaves the pane where it was — never a hard error.
    pub fn commit_addr(&mut self, is_left: bool) {
        let p = PathBuf::from(self.addr_text.trim().to_string());
        self.addr_edit = None;
        self.set_active_pane(is_left);
        if p.is_dir() {
            self.pane_of(is_left).navigate(p.clone());
            self.apply_cached_sizes();
            self.status = format!(
                "{} {}",
                crate::lang::tr("已跳转到", self.lang),
                p.display()
            );
            self.persist_layout();
        } else {
            self.status = format!(
                "{}: {}",
                crate::lang::tr("不是目录", self.lang),
                p.display()
            );
        }
    }

    /// Sort a pane by `key`, flipping the direction when it is already active.
    /// The change applies to every panel on that side (all tabs) so the sort
    /// setting stays put when you switch tabs.
    pub fn sort_by(&mut self, is_left: bool, key: crate::app::SortKey) {
        let (new_key, new_dir) = {
            let p = self.pane_of(is_left);
            if p.sort_key == key {
                (
                    key,
                    if p.sort_dir == crate::app::SortDir::Asc {
                        crate::app::SortDir::Desc
                    } else {
                        crate::app::SortDir::Asc
                    },
                )
            } else {
                (key, crate::app::SortDir::Desc)
            }
        };
        self.apply_side_sort(is_left, new_key, new_dir);
        self.persist_layout();
    }

    /// Select `name` in the panel chosen by `is_left` (used by context menus).
    pub fn set_selection(&mut self, is_left: bool, name: &str) {
        self.set_active_pane(is_left);
        let p = self.pane_of(is_left);
        p.selected = Some(name.to_string());
    }

    // ---------- drives / sid ----------

    /// Refresh drive capacity info (background).
    pub fn refresh_drives(&mut self) {
        let (tx, rx) = channel();
        self.drives_rx = Some(rx);
        thread::spawn(move || {
            let d = fs_util::enumerate_drives();
            tx.send(d).ok();
        });
    }

    /// Fetch the user SID for the Recycle-Bin view (background).
    pub fn refresh_sid(&mut self) {
        if self.sid.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.sid_rx = Some(rx);
        thread::spawn(move || {
            let s = fs_util::get_user_sid();
            tx.send(s).ok();
        });
    }

    /// Spawn a worker for every panel sitting in a recycle-bin view whose
    /// listing is still loading. The `pending_rec` set keeps a directory that
    /// is being read from being requested twice (navigate away and back).
    pub fn request_recycle_listings(&mut self) {
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            if !panel.rec_loading {
                continue;
            }
            let dir = panel.cwd.clone();
            if !self.pending_rec.insert(dir.clone()) {
                continue;
            }
            let tx = self.rec_tx.clone();
            thread::spawn(move || {
                let items = crate::app::read_recycle_items(&dir);
                tx.send((dir, items)).ok();
            });
        }
    }

    /// Adopt finished recycle listings. A result only lands on a panel that is
    /// still loading *and* still shows the same directory (a late reply for a
    /// folder the user already left is dropped).
    pub fn poll_recycle(&mut self) {
        while let Ok((dir, items)) = self.rec_rx.try_recv() {
            self.pending_rec.remove(&dir);
            let mut items = Some(items);
            for i in 0..2 {
                let panel = &mut self.tabs[self.active_tab].panes[i];
                if panel.rec_loading && panel.cwd == dir {
                    panel.recycle_items = items.take().unwrap_or_default();
                    panel.rec_loading = false;
                }
            }
        }
    }

    /// Where the Recycle-Bin entry of the drive panel jumps to.
    /// We target the drive that actually hosts the user profile (normally
    /// `C:`), because that is where the OS Recycle Bin the user sees lives;
    /// removable drives before `C:` (e.g. `A:`) would be wrong.
    pub fn recycle_target(&self) -> Option<PathBuf> {
        let sid = self.sid.clone()?;
        let drive = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("SystemDrive"))
            .ok()
            .and_then(|s| {
                // First component of an absolute Windows path is the drive root
                // (e.g. "C:"), which is what `$Recycle.Bin` lives under.
                std::path::Path::new(&s)
                    .components()
                    .next()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
            })
            .filter(|d| d.ends_with(':'))
            .map(|d| PathBuf::from(format!("{d}\\")))
            .or_else(|| {
                self.drives
                    .first()
                    .map(|d| PathBuf::from(format!("{}:\\", d.letter)))
            })
            .unwrap_or_else(|| PathBuf::from("C:\\"));
        Some(recycle_root(&drive, &sid))
    }

    // ---------- quick access ----------

    /// Load Windows Explorer's pinned Quick-Access folders once (background).
    pub fn refresh_quick_access(&mut self) {
        if self.qa_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.qa_rx = Some(rx);
        thread::spawn(move || {
            let list: Vec<(PathBuf, bool)> = crate::quickaccess::load_windows_quick_access()
                .into_iter()
                .map(|e| (e.path, e.pinned))
                .collect();
            tx.send(list).ok();
        });
    }

    /// Drain the Quick-Access loader channel once its background read finishes.
    /// The (empty) receiver is kept as a "loaded" sentinel so we never re-spawn.
    ///
    /// The loaded count is deliberately **not** written to the status log: it is
    /// a one-shot startup message that would clobber the "就绪 + shortcut hints"
    /// line, and the folders are already visible in the sidebar — the number
    /// tells the user nothing.
    pub fn poll_quick_access(&mut self) {
        if let Some(rx) = &self.qa_rx {
            if let Ok(list) = rx.try_recv() {
                self.system_qa = list;
                self.qa_dirty = true;
            }
        }
    }

    /// Combined Quick-Access list as `(path, origin)`, in display order: the
    /// user's own favourites first, then Windows' pinned folders, then its
    /// recent ones.
    ///
    /// Windows' pin flags are what populate the "已固定" heading — without
    /// them that group only ever held Marin's own entries and looked empty
    /// even though Explorer showed a dozen.
    pub fn quick_access_list(&mut self) -> Vec<(PathBuf, QaKind)> {
        if self.qa_dirty {
            let mut out: Vec<(PathBuf, QaKind)> = Vec::new();
            for p in &self.favorites {
                if !out.iter().any(|(q, _)| q == p) {
                    out.push((p.clone(), QaKind::Favorite));
                }
            }
            for (p, pinned) in &self.system_qa {
                if *pinned {
                    // Always show Windows' own pins under "已固定", even when the
                    // same folder is also one of our 收藏 — the two groups mean
                    // different things (Explorer owns the pin; we own the bookmark).
                    out.push((p.clone(), QaKind::WinPin));
                } else if !out.iter().any(|(q, _)| q == p) {
                    out.push((p.clone(), QaKind::Recent));
                }
            }
            self.qa_cache = out;
            self.qa_dirty = false;
        }
        self.qa_cache.clone()
    }

    /// Upper bound on Marin's own favourites.
    ///
    /// Windows' own pinned group has no fixed limit (the jump list just grows),
    /// but ours lives in `store.txt` and is shown in a rail of fixed height, so
    /// an unbounded list would only push everything else out of reach.
    pub const MAX_FAVORITES: usize = 30;

    /// Add the active panel's current directory to the favourites.
    pub fn favorite_current(&mut self) {
        let cwd = self.active_panel().cwd.clone();
        self.favorite_path(cwd);
    }

    /// Add an arbitrary folder or file (toolbar, folder menu, tab menu, rail
    /// menu, file menu) to the favourites.
    ///
    /// Refuses two cases with a status line instead of failing silently: the
    /// path is already a favourite, or the cap is reached. Windows-pinned
    /// folders are *allowed* — a folder can sit in both "已固定" (read-only,
    /// from Explorer) and "收藏" (ours, removable), and the × only ever
    /// touches the latter.
    pub fn favorite_path(&mut self, path: PathBuf) {
        // Folders and files both go into 收藏: a folder is a jump target, a
        // file is a one-click launcher. Reject anything that isn't on disk so a
        // stale or phantom path can't squat in the rail.
        if !(path.is_dir() || path.is_file()) {
            return;
        }
        if self.favorites.iter().any(|p| *p == path) {
            self.status = crate::lang::tr("该目录已收藏", self.lang).into();
            return;
        }
        if self.favorites.len() >= Self::MAX_FAVORITES {
            self.status = format!(
                "{}（{} {}）",
                crate::lang::tr("收藏数量已达上限", self.lang),
                Self::MAX_FAVORITES,
                crate::lang::tr("个", self.lang),
            );
            return;
        }
        self.favorites.push(path.clone());
        self.store.set_qa_pins(&self.favorites);
        self.qa_dirty = true;
        self.status = format!("{} {}", crate::lang::tr("已收藏:", self.lang), path.display());
    }

    /// "加入收藏" / "取消收藏" menu item for `path` — one entry that flips
    /// with the current state, shared by the folder menu, the pane background
    /// menu, the tab menu and the Quick-Access rail menu.
    pub fn favorite_menu_item(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        if self.favorites.iter().any(|p| p.as_path() == path) {
            if ui.button(crate::lang::tr("取消收藏", self.lang)).clicked() {
                self.unfavorite(path);
                ui.close_menu();
            }
        } else if ui
            .button(crate::lang::tr("加入收藏", self.lang))
            .on_hover_text(path.to_string_lossy().to_string())
            .clicked()
        {
            self.favorite_path(path.to_path_buf());
            ui.close_menu();
        }
    }

    /// Remove a path from the favourites (Windows' pins stay).
    pub fn unfavorite(&mut self, path: &std::path::Path) {
        let before = self.favorites.len();
        self.favorites.retain(|p| p.as_path() != path);
        if self.favorites.len() != before {
            self.store.set_qa_pins(&self.favorites);
            self.qa_dirty = true;
            self.status = crate::lang::tr("已取消收藏", self.lang).into();
        }
    }

    /// Right-click menu for a Quick-Access rail row.
    ///
    /// A rail entry is a folder, so it gets the folder actions the listing
    /// offers — open, new tab, VS Code, terminal, reveal, copy path,
    /// properties — plus the favourite toggle. Editing actions (rename,
    /// delete) are left out on purpose: those belong to the listing, where
    /// the row is the real directory entry.
    pub fn qa_context_menu(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        let path = path.to_path_buf();
        let is_dir = path.is_dir();
        let hint = path.to_string_lossy().to_string();
        if ui
            .button(crate::lang::tr("打开", self.lang))
            .on_hover_text(&hint)
            .clicked()
        {
            if is_dir {
                self.active_panel().navigate(path.clone());
                self.apply_cached_sizes();
            } else {
                crate::fs_util::open_default_file(&path);
            }
            ui.close_menu();
        }
        // Folder-only affordances: a file has no "open in pane" target.
        if is_dir {
            if ui
                .button(crate::lang::tr("在新标签页打开", self.lang))
                .on_hover_text(&hint)
                .clicked()
            {
                self.open_path_in_new_tab(path.clone());
                ui.close_menu();
            }
            if ui
                .button(crate::lang::tr("在右栏打开", self.lang))
                .on_hover_text(&hint)
                .clicked()
            {
                self.open_path_in_right_pane(path.clone());
                ui.close_menu();
            }
        }
        ui.separator();
        // Only offered when the program is installed, so a menu item never
        // dead-ends.
        if crate::fs_util::vscode_exe().is_some()
            && ui
                .button(crate::lang::tr("通过 VS Code 打开", self.lang))
                .clicked()
        {
            crate::fs_util::open_with_vscode(&path);
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("在此打开终端", self.lang))
            .clicked()
        {
            let dir = if is_dir {
                path.clone()
            } else {
                path.parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or_else(|| path.clone())
            };
            crate::fs_util::open_terminal(&dir);
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("在资源管理器中显示", self.lang))
            .clicked()
        {
            crate::fs_util::reveal_in_explorer(&path);
            ui.close_menu();
        }
        ui.separator();
        if ui.button(crate::lang::tr("复制路径", self.lang)).clicked() {
            ui.ctx().copy_text(hint.clone());
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("属性", self.lang)).clicked() {
            self.show_props(path.clone());
            ui.close_menu();
        }
        ui.separator();
        self.favorite_menu_item(ui, &path);
    }

    // ---------- size worker ----------

    /// Re-apply cached sizes and queue the missing ones, at most once a second.
    ///
    /// This used to run on every frame: two full walks over both panels' rows,
    /// sixty times a second, for a result that only changes when the worker
    /// delivers something or the user navigates. It is now called from
    /// navigation as well, so the periodic call is only a top-up.
    pub fn refresh_folder_sizes_throttled(&mut self) {
        const INTERVAL: std::time::Duration = std::time::Duration::from_millis(1000);
        if self.last_size_poll.elapsed() < INTERVAL {
            return;
        }
        self.last_size_poll = std::time::Instant::now();
        self.apply_cached_sizes();
        self.request_folder_sizes();
    }

    /// Ask the background worker to (re)compute sizes for unknown folders.
    ///
    /// Two brakes on the cost: only the first `FOLDERS_PER_PANEL` folders of a
    /// listing are queued (roughly what fits on screen — nobody looks at
    /// folder 300's size), and a value that is still inside its TTL is reused
    /// instead of rescanned.
    pub fn request_folder_sizes(&mut self) {
        self.request_sizes_limited(FOLDERS_PER_PANEL, MAX_PENDING_SIZES, false);
    }

    /// Same, with caller-chosen ceilings and scan mode. The manual
    /// "重新计算大小" action uses this: there the "only what fits on screen"
    /// limit does not apply, the user explicitly asked for everything to be
    /// recomputed — and with `full = true` the scan runs *without* the
    /// 400 ms / 150k-entry budget, so the number is exact instead of a lower
    /// bound. A 700 GB folder needs tens of seconds for that; the budgeted
    /// mode stays for passive browsing, where a fast `~` beats a slow exact.
    /// Returns how many folders were queued (the manual action uses this to
    /// know whether anything was left to do at all).
    fn request_sizes_limited(&mut self, per_panel: usize, max_pending: usize, full: bool) -> usize {
        if self.pending_sizes.len() >= max_pending {
            return 0;
        }
        let now = crate::cache::now_secs();
        let mut queued = 0usize;
        // Index-based loop: borrowing `self.tabs` directly (not through a
        // method) lets the other fields be touched inside the loop.
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            let mut per = 0usize;
            for e in panel.entries.iter().filter(|e| e.is_dir && !e.size_known) {
                if per >= per_panel {
                    break;
                }
                let p = panel.cwd.join(&e.name);
                let mtime = e.mtime;
                // Fresh enough, or unchanged: keep the cached value.
                if self.store.size_for_lenient(&p, mtime, now).is_some() {
                    continue;
                }
                let already = !self.pending_sizes.insert(p.clone());
                // `full` re-sends even when a passive scan for the same folder
                // is already in flight: the manual action must not come back
                // with a budgeted `~` just because browsing queued it a moment
                // earlier. FIFO order keeps the exact result last, so it wins.
                if full || !already {
                    self.size_tx.send((p, full)).ok();
                    if full {
                        self.size_full_left += 1;
                    }
                    queued += 1;
                    per += 1;
                }
            }
        }
        queued
    }

    /// Forget every cached folder size in view and queue a fresh scan.
    ///
    /// The TTL in `Store` keeps a stale — or budget-stopped — number on screen
    /// for hours. This is the manual override behind the 重新计算大小 button.
    /// Both panes are recomputed: the sibling pane is the one being compared
    /// against, and it is usually the pane holding the stale parent.
    pub fn recompute_sizes(&mut self) {
        let mut paths: Vec<PathBuf> = Vec::new();
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            for e in panel.entries.iter_mut().filter(|e| e.is_dir) {
                e.size_known = false;
                e.size = 0;
                e.size_est = false;
                paths.push(panel.cwd.join(&e.name));
            }
            panel.resort_sizes();
        }
        // Drop the cached values as well: `size_for_lenient` would otherwise
        // hand the very same number straight back and nothing would rescan.
        for p in &paths {
            self.store.size_map.remove(p);
        }
        self.store.dirty = true;
        // An explicit request, so the "roughly what fits on screen" ceiling is
        // raised to everything in view: the user asked for *all* of it. `full
        // = true`: no scan budget, so every folder gets an exact number
        // instead of a `~` lower bound (that is what made a 700 GB folder
        // report ~140 GB before).
        self.request_sizes_limited(usize::MAX, usize::MAX, true);
        // Fresh volume counter for the new batch: the finish line reports how
        // much data the exact walk covered ("已扫 X GiB").
        self.size_full_bytes = 0;
        // Time the whole batch: the finish line says how long it took, which
        // is the only warning the user gets that a 700 GB folder costs real
        // minutes (and that the exact number was worth the wait).
        //
        // `size_scan_started` is set even when nothing was queued: an instant
        // batch still deserves its finish line ("耗时 不到 1 秒"), delivered
        // through the same path on the next frame. The status line is never
        // touched — the size box is the whole feedback surface, so the
        // operation log at the far left keeps whatever it was showing. The
        // outstanding-scan counter lives in `size_full_left` and is only ever
        // decremented as results land, so the frame loop's "batch finished?"
        // check cannot fire early.
        self.size_scan_started = Some(Instant::now());
        // A new run replaces the previous run's finish line.
        self.size_scan = None;
    }

    /// One short line per CPU-heavy background task currently running.
    ///
    /// The bottom status bar draws these on the left: a copy / delete or a big
    /// directory read is invisible without them. The manual full size scan is
    /// **not** here — it has its own box (`size_scan`), which also carries the
    /// finish line and stays on screen. Passive (budgeted) size scans share
    /// one line — each of them is over in well under a second — and are
    /// suppressed while a manual batch owns the box.
    pub fn running_tasks(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        if let Some(job) = &self.job {
            let mut line = job.label.clone();
            if job.deleted > 0 {
                line = format!("{line} 已删除 {}", job.deleted);
            } else if !job.indeterminate {
                line = format!("{line} {:.0}%", job.progress * 100.0);
            }
            v.push(line);
        }
        if self.size_scan_started.is_none() && !self.pending_sizes.is_empty() {
            v.push(crate::lang::tr("正在计算文件夹大小…", self.lang).into());
        }
        // One line per kind, not per pane: both panes loading at once is the
        // common case and would otherwise read twice.
        if self.tabs.iter().any(|t| t.panes.iter().any(|p| p.loading)) {
            v.push(crate::lang::tr("正在读取目录…", self.lang).into());
        }
        if self
            .tabs
            .iter()
            .any(|t| t.panes.iter().any(|p| p.rec_loading))
        {
            v.push(crate::lang::tr("正在读取回收站…", self.lang).into());
        }
        v
    }

    /// Apply a size result coming back from the worker.
    ///
    /// `complete == false` marks a budget-stopped scan: the number is a lower
    /// bound, so it is flagged and rendered with `~`.
    pub fn apply_size_result(&mut self, path: PathBuf, mtime: u64, size: u64, complete: bool) {
        self.pending_sizes.remove(&path);
        self.store.record_size(path.clone(), mtime, size, complete);
        // Match each entry by the result's parent dir + file name instead of
        // `cwd.join(name)` per entry: a 5k-entry folder × many size results
        // used to allocate 5k PathBufs on every result. `parent()` /
        // `file_name()` are borrow-only and free.
        let parent = path.parent().map(|p| p.to_path_buf());
        let fname = path.file_name().map(|n| n.to_string_lossy().into_owned());
        if let (Some(parent), Some(fname)) = (parent, fname) {
            for i in 0..2 {
                let panel = &mut self.tabs[self.active_tab].panes[i];
                if panel.cwd != parent {
                    continue;
                }
                for e in panel.entries.iter_mut() {
                    if e.is_dir && e.mtime == mtime && e.name == fname {
                        e.size = size;
                        e.size_known = true;
                        e.size_est = !complete;
                    }
                }
            }
        }
    }

    /// Adopt directory listings that finished on the worker thread.
    ///
    /// A result is only used when the panel still shows the same folder *and*
    /// the same generation: the user may have navigated on (or reloaded) while
    /// the worker was reading, and a stale listing must not overwrite the
    /// newer one. `generation` is bumped by every `load()`, so this check is
    /// exact, not a heuristic.
    pub fn poll_listings(&mut self) {
        let mut got = false;
        while let Ok((dir, generation, entries)) = self.list_rx.try_recv() {
            for tab in &mut self.tabs {
                for p in &mut tab.panes {
                    if p.loading && p.cwd == dir && p.generation == generation {
                        p.adopt_listing(entries.clone());
                        got = true;
                    }
                }
            }
        }
        if got {
            // The rows just arrived: cached folder sizes apply immediately and
            // the rest get requested from the size worker.
            self.apply_cached_sizes();
            self.request_folder_sizes();
            self.bg_signal = Some(Instant::now());
        }
    }

    /// Fill in cached sizes for the current listings (called after loads).
    pub fn apply_cached_sizes(&mut self) {
        let now = crate::cache::now_secs();
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            let mut filled = false;
            for e in panel.entries.iter_mut() {
                if e.is_dir && !e.size_known {
                    let p = panel.cwd.join(&e.name);
                    if let Some((sz, est)) = self.store.size_for_lenient(&p, e.mtime, now) {
                        e.size = sz;
                        e.size_known = true;
                        // Persisted since the estimate flag is stored on disk:
                        // a `~` value must not lose its tilde on the next visit.
                        e.size_est = est;
                        filled = true;
                    }
                }
            }
            // The rows now carry real sizes: a size-sorted panel has to be
            // re-sorted or it would keep the order they had when every folder
            // still looked like 0 bytes.
            if filled {
                panel.resort_sizes();
            }
        }
    }

    /// Re-sort any size-sorted pane, after a batch of sizes came back from
    /// the worker. Batched here (not per result) so a burst of results costs
    /// one sort per pane instead of one per folder.
    pub fn resort_size_sorted_panels(&mut self) {
        for i in 0..2 {
            self.tabs[self.active_tab].panes[i].resort_sizes();
        }
    }

    /// Persist the store if it has been dirty for a while.
    pub fn maybe_flush(&mut self) {
        if self.store.dirty && self.last_flush.elapsed() >= std::time::Duration::from_secs(5) {
            if let Err(e) = self.store.flush() {
                self.status = format!("{}: {e}", crate::lang::tr("持久化失败", self.lang));
            }
            self.last_flush = Instant::now();
        }
    }

    pub fn flush_now(&mut self) {
        let _ = self.store.flush();
        self.last_flush = Instant::now();
    }

    // ---------- clipboard ops ----------

    pub fn do_copy(&mut self) {
        if self.active_panel().in_recycle {
            // Clone the name so the `&mut self` borrow ends before we read `self.lang`.
            let rec = self.active_panel().sel_rec().map(|r| r.name.clone());
            if let Some(name) = rec {
                self.status = format!("{}: {}", crate::lang::tr("回收站项目不能复制", self.lang), name);
            }
            return;
        }
        self.copy_selection(false);
    }

    pub fn do_move(&mut self) {
        if self.active_panel().in_recycle {
            let rec = self.active_panel().sel_rec().map(|r| r.name.clone());
            if let Some(name) = rec {
                self.status = format!("{}: {}", crate::lang::tr("回收站项目不能移动", self.lang), name);
            }
            return;
        }
        self.copy_selection(true);
    }

    /// Put `paths` on the clipboard (cut or copy), including the Windows
    /// file-list format so a later paste in another program works too.
    fn clip_paths(&mut self, paths: Vec<PathBuf>, cut: bool) {
        let n = paths.len();
        self.clip = Some(if cut {
            Clip::Move(paths.clone())
        } else {
            Clip::Copy(paths.clone())
        });
        fs_util::set_clipboard_files(&paths);
        self.status = format!(
            "{} {} {}",
            crate::lang::tr(if cut { "已准备剪贴" } else { "已复制" }, self.lang),
            n,
            crate::lang::tr("项", self.lang)
        );
    }

    pub fn do_paste(&mut self) {
        if self.job.is_some() {
            self.status = crate::lang::tr("任务进行中，请稍候", self.lang).into();
            return;
        }
        if self.active_panel().in_recycle {
            self.status = crate::lang::tr("不能粘贴到回收站", self.lang).into();
            return;
        }
        // Image on the system clipboard (and nothing else): paste it as a PNG
        // file. This is what makes "copy a screenshot → paste here" work.
        if self.clip.is_none() {
            if let Some(img) = fs_util::get_clipboard_image() {
                self.paste_clipboard_image(img);
                return;
            }
        }
        // In-app clipboard first; when it is empty, fall back to files copied
        // in another program (Explorer, a browser, …) through the Windows
        // clipboard — that is what makes Ctrl+V behave like a real file
        // manager.
        let clip = match self.clip.clone() {
            Some(c) => Some(c),
            None => fs_util::get_clipboard_files()
                .map(|paths| if paths.is_empty() { None } else { Some(Clip::Copy(paths)) })
                .flatten(),
        };
        let Some(clip) = clip else {
            self.status = crate::lang::tr("剪贴板为空", self.lang).into();
            return;
        };
        let (paths, is_move) = match clip {
            Clip::Copy(p) => (p, false),
            Clip::Move(p) => (p, true),
        };
        let dest_dir = self.active_panel().cwd.clone();

        // Plain files are handled synchronously; directories go to a
        // background job (a big tree would freeze the UI thread).
        let ok = 0usize;
        let mut skip = 0usize;
        let fail = 0usize;
        let mut renamed = 0usize;
        let mut dirs: Vec<(PathBuf, PathBuf)> = Vec::new();
        for src in &paths {
            let name = src
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "item".to_string());
            let plain = dest_dir.join(&name);
            // Pasting back into the folder the item already lives in has to
            // produce a copy (`name - 副本.ext`) — that is what Explorer does,
            // and both panes very often show the same folder, so skipping it
            // made the very first Ctrl+C / Ctrl+V attempt look broken.
            // A name clash with a *different* folder is still skipped: there
            // the item is not the same file and overwriting would be wrong.
            let dest = if plain.exists() {
                let same_dir = src.parent() == Some(dest_dir.as_path());
                if same_dir && !is_move {
                    renamed += 1;
                    unique_copy_path(&plain, self.lang)
                } else {
                    skip += 1;
                    continue;
                }
            } else {
                plain
            };
            // Files go to the worker as well: a multi-gigabyte fs::copy on the
            // UI thread freezes the window until it finishes.
            dirs.push((src.clone(), dest));
        }

        if !dirs.is_empty() {
            let verb = crate::lang::tr(if is_move { "正在移动" } else { "正在复制" }, self.lang);
            let label = format!("{} {} {}…", verb, dirs.len(), crate::lang::tr("项", self.lang));
            self.spawn_copy_jobs(dirs, is_move, ok, skip, fail, renamed, label, dest_dir);
            return;
        }
        if is_move && ok > 0 {
            self.clip = None;
        }
        self.paste_status(ok, skip, fail, renamed, is_move);
        self.reload_both();
    }

    /// Paste a `Clip::Copy(paths)`... no — this is the image branch of
    /// [`Self::do_paste`]: write a clipboard image to a fresh PNG in the
    /// active folder.
    fn paste_clipboard_image(&mut self, img: image::RgbaImage) {
        let dest_dir = self.active_panel().cwd.clone();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let base = format!("剪贴板图片_{stamp}.png");
        // Avoid clobbering an existing file with the same second-stamp.
        let mut dest = dest_dir.join(&base);
        let mut n = 1u32;
        while dest.exists() {
            n += 1;
            dest = dest_dir.join(format!("剪贴板图片_{stamp}_{n}.png"));
        }
        match crate::convert::encode_png(&img, 6) {
            Ok(bytes) => match std::fs::write(&dest, &bytes) {
                Ok(_) => {
                    self.status = format!(
                        "{}: {}",
                        crate::lang::tr("已粘贴图片", self.lang),
                        dest.file_name()
                            .map(|f| f.to_string_lossy().into_owned())
                            .unwrap_or_default()
                    );
                    self.reload_both();
                }
                Err(_) => {
                    self.status = crate::lang::tr("粘贴图片失败", self.lang).into();
                }
            },
            Err(_) => {
                self.status = crate::lang::tr("粘贴图片失败", self.lang).into();
            }
        }
    }

    /// Status line summarising a paste of N items.
    fn paste_status(
        &mut self,
        ok: usize,
        skip: usize,
        fail: usize,
        renamed: usize,
        is_move: bool,
    ) {
        let verb = crate::lang::tr(if is_move { "已移动" } else { "已复制" }, self.lang);
        let mut s = format!("{} {} {}", verb, ok, crate::lang::tr("项", self.lang));
        if renamed > 0 {
            s.push_str(&format!(
                "，{} {}",
                renamed,
                crate::lang::tr("个存为副本", self.lang)
            ));
        }
        if skip > 0 {
            s.push_str(&format!(
                "，{} {} {}",
                crate::lang::tr("跳过", self.lang),
                skip,
                crate::lang::tr("个已存在", self.lang)
            ));
        }
        if fail > 0 {
            s.push_str(&format!("，{} {}", fail, crate::lang::tr("个失败", self.lang)));
        }
        self.status = s;
    }

    /// Background copy/move of several *directories* at once. Progress counts
    /// items: byte-level progress from `copy_tree` is discarded, because
    /// mixing the two scales makes the bar jump wildly across many trees.
    fn spawn_copy_jobs(
        &mut self,
        items: Vec<(PathBuf, PathBuf)>,
        is_move: bool,
        ok: usize,
        skip: usize,
        fail: usize,
        renamed: usize,
        label: String,
        dest_dir: PathBuf,
    ) {
        let (tx, rx) = channel();
        let lang = self.lang; // the worker thread must not capture `self`
        thread::spawn(move || {
            let total = items.len().max(1) as f32;
            let mut done = 0f32;
            let mut ok = ok;
            let mut fail = fail;
            let renamed = renamed;
            // Byte progress would fight the per-item progress below; dump it.
            let (dtx, drx) = channel();
            thread::spawn(move || while drx.recv().is_ok() {});
            for (src, dest) in &items {
                let res: Result<(), String> = if is_move {
                    if fs::rename(src, dest).is_ok() {
                        Ok(())
                    } else {
                        copy_tree(src, dest, &dtx, 0).and_then(|_| {
                            let c = if src.is_dir() {
                                fs::remove_dir_all(src)
                            } else {
                                fs::remove_file(src)
                            };
                            c.map_err(|e| e.to_string())
                        })
                    }
                } else {
                    copy_tree(src, dest, &dtx, 0)
                };
                match res {
                    Ok(_) => ok += 1,
                    Err(_) => fail += 1,
                }
                done += 1.0;
                tx.send(JobMsg::Progress(done / total)).ok();
            }
            let verb = crate::lang::tr(if is_move { "已移动" } else { "已复制" }, lang);
            let mut s = format!("{} {} {}", verb, ok, crate::lang::tr("项", lang));
            if renamed > 0 {
                s.push_str(&format!(
                    "，{} {}",
                    renamed,
                    crate::lang::tr("个存为副本", lang)
                ));
            }
            if skip > 0 {
                s.push_str(&format!(
                    "，{} {} {}",
                    crate::lang::tr("跳过", lang),
                    skip,
                    crate::lang::tr("个已存在", lang)
                ));
            }
            if fail > 0 {
                s.push_str(&format!("，{} {}", fail, crate::lang::tr("个失败", lang)));
            }
            // An empty key means "the argument is the whole status line".
            // Only when *nothing* worked do we report a hard failure, so the
            // usual partial result still shows its counts.
            if ok == 0 && fail > 0 {
                tx.send(JobMsg::Failed(format!(
                    "{} {}",
                    fail,
                    crate::lang::tr("项全部失败", lang)
                )))
                .ok();
            } else {
                tx.send(JobMsg::Done("", format!("{s} → {}", dest_dir.display())))
                    .ok();
            }
        });
        self.status = format!("{}: {label}", crate::lang::tr("正在运行", self.lang));
        self.job = Some(Job {
            label,
            progress: 0.0,
            deleted: 0,
            indeterminate: false,
            note: None,
            err: None,
            rx,
            started: Instant::now(),
        });
    }

    /// Delete a batch on a worker thread (see `delete_paths`). Progress is
    /// per-item; the finish message carries the same counts as the sync path.
    fn spawn_delete_job(&mut self, paths: Vec<PathBuf>, permanent: bool) {
        let (tx, rx) = channel();
        let n = paths.len();
        let lang = self.lang;
        thread::spawn(move || {
            let total = paths.len().max(1) as f32;
            let mut done = 0f32;
            let mut ok = 0usize;
            let mut fail = 0usize;
            let mut first_err: Option<(String, String)> = None;
            for p in &paths {
                let res: Result<(), String> = if permanent {
                    let is_dir = fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false);
                    let r = if is_dir { fs::remove_dir_all(p) } else { fs::remove_file(p) };
                    r.map_err(|e| e.to_string())
                } else {
                    fs_util::move_to_recycle_bin(p)
                };
                match res {
                    Ok(()) => ok += 1,
                    Err(e) => {
                        fail += 1;
                        if first_err.is_none() {
                            let name = p
                                .file_name()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| p.to_string_lossy().into_owned());
                            first_err = Some((name, e));
                        }
                    }
                }
                done += 1.0;
                tx.send(JobMsg::Progress(done / total)).ok();
            }
            let verb = crate::lang::tr(if permanent { "已永久删除" } else { "已移到回收站" }, lang);
            let line = match first_err {
                None => format!("{} {} {}", verb, ok, crate::lang::tr("项", lang)),
                Some((name, why)) => format!(
                    "{} {} {}，{} {}：{}（{}）",
                    verb,
                    ok,
                    crate::lang::tr("项", lang),
                    fail,
                    crate::lang::tr("个失败", lang),
                    name,
                    why
                ),
            };
            tx.send(JobMsg::Done("", line)).ok();
        });
        let verb = crate::lang::tr(if permanent { "正在删除" } else { "正在移到回收站" }, self.lang);
        let label = format!("{} {n} {}…", verb, crate::lang::tr("项", self.lang));
        self.status = format!("{}: {label}", crate::lang::tr("正在运行", self.lang));
        self.job = Some(Job {
            label,
            progress: 0.0,
            deleted: 0,
            indeterminate: false,
            note: None,
            err: None,
            rx,
            started: Instant::now(),
        });
    }

    /// Drain job messages; returns a final status line when the job finishes.
    pub fn poll_job(&mut self) -> Option<String> {
        let Some(job) = self.job.as_mut() else { return None };
        let mut finished: Option<String> = None;
        let mut took = std::time::Duration::ZERO;
        while let Ok(m) = job.rx.try_recv() {
            if job.note.is_none() && job.err.is_none() {
                job.finish(m);
            }
            if let Some((k, a)) = &job.note {
                // An empty key means the argument already is the whole line
                // (used by the multi-item paste summary, which has no fixed
                // translation template).
                let s = if k.is_empty() {
                    a.clone()
                } else {
                    let t = crate::lang::tr(*k, self.lang);
                    if a.is_empty() {
                        t.to_string()
                    } else {
                        t.replace("%s", a)
                    }
                };
                finished = Some(s);
                took = job.started.elapsed();
            }
            if let Some(err) = &job.err {
                finished = Some(format!("{}: {err}", crate::lang::tr("失败", self.lang)));
                took = job.started.elapsed();
            }
        }
        if let Some(line) = finished {
            self.job = None;
            // Every job kind can change what the Recycle Bin holds (a move
            // puts items *in*, a delete or empty takes them *out*), so the
            // cached full/empty glyph is dropped for all of them.
            self.icons.invalidate(&crate::shellicon::Kind::Recycle);
            self.refresh_both();
            // Say how long it ran: a 10-minute copy must not look like an
            // instant one in the log of what just happened.
            let took_s = crate::app::human_duration(took, self.lang);
            Some(format!(
                "{line} · {}",
                crate::lang::tr("耗时 %s", self.lang).replace("%s", &took_s)
            ))
        } else {
            None
        }
    }

    fn refresh_both(&mut self) {
        let lc = self.p0().cwd.clone();
        let rc = self.p1().cwd.clone();
        self.p0().load(&lc);
        self.p1().load(&rc);
        self.apply_cached_sizes();
        self.persist_layout();
    }

    // ---------- delete / recycle ----------

    /// Delete key / toolbar button: move to the Recycle Bin like Explorer does.
    /// Inside the Recycle-Bin view it asks for permanent-delete confirmation.
    pub fn do_delete(&mut self) {
        if self.active_panel().in_recycle {
            if let Some(r) = self.active_panel().sel_rec().cloned() {
                self.delete_rec_suffix = Some(r.suffix);
                self.delete_open = true;
            }
            return;
        }
        // Delete acts on the whole selection, not just the current row.
        let paths = self.active_panel().sel_paths();
        if paths.is_empty() {
            return;
        }
        // One code path for both: `delete_paths` keeps a lone *file*
        // synchronous (instant) and sends everything heavier to a worker.
        self.delete_paths(paths, false);
    }

    /// Shift+Delete: permanent deletion, guarded by a confirmation dialog.
    pub fn do_delete_permanent(&mut self) {
        if self.active_panel().in_recycle {
            if let Some(r) = self.active_panel().sel_rec().cloned() {
                self.delete_rec_suffix = Some(r.suffix);
                self.delete_open = true;
            }
            return;
        }
        let paths = self.active_panel().sel_paths();
        if paths.is_empty() {
            return;
        }
        self.delete_targets = paths;
        self.delete_permanent = true;
        self.delete_open = true;
    }

    pub fn confirm_delete(&mut self) {
        if let Some(suffix) = self.delete_rec_suffix.take() {
            // Permanently remove the $R/$I pair — instant, no recursion.
            let dir = self.active_panel().cwd.clone();
            let mut n = 0;
            for pre in ["$R", "$I"] {
                if fs::remove_file(dir.join(format!("{pre}{suffix}"))).is_ok() {
                    n += 1;
                }
            }
            self.status = if n > 0 {
                format!("{} {n} {}", crate::lang::tr("已彻底删除（", self.lang), crate::lang::tr("个文件）", self.lang))
            } else {
                crate::lang::tr("删除失败：文件不存在或无权限", self.lang).into()
            };
            self.reload_both();
            self.icons.invalidate(&crate::shellicon::Kind::Recycle);
        } else if !self.delete_targets.is_empty() {
            let targets = std::mem::take(&mut self.delete_targets);
            let permanent = self.delete_permanent;
            self.delete_paths(targets, permanent);
        }
        self.delete_open = false;
    }

    /// Delete one recycle-bin item, working around the two reasons a plain
/// `remove_file` refuses: the `\\?\` prefix lifts the 260-character path
/// limit, and read-only entries have to be un-read-only'd first.
fn force_remove(p: &std::path::Path) -> std::io::Result<()> {
    let verbatim = std::path::PathBuf::from(format!(r"\\?\{}", p.display()));
    // `$R` payloads of deleted *folders* are directories: a plain remove_file
    // fails on them (which is how "empty" silently left every deleted folder
    // behind).
    if verbatim.is_dir() {
        Self::remove_tree(&verbatim)
    } else {
        Self::remove_one(&verbatim)
    }
}

/// Delete one file, clearing the read-only attribute first (Windows refuses
/// to delete a read-only file outright).
fn remove_one(p: &std::path::Path) -> std::io::Result<()> {
    if let Ok(m) = fs::metadata(p) {
        let mut perms = m.permissions();
        if perms.readonly() {
            perms.set_readonly(false);
            let _ = fs::set_permissions(p, perms);
        }
    }
    fs::remove_file(p)
}

/// Delete a whole tree, files first and directories deepest-first.
///
/// `remove_dir_all` stops at the first read-only child, and a recycled folder
/// is full of them — anything deleted from read-only media, or carrying its
/// original attributes, came back as a failure. So attributes are cleared on
/// the way down and every child is removed explicitly.
fn remove_tree(p: &std::path::Path) -> std::io::Result<()> {
    let mut dirs: Vec<std::path::PathBuf> = vec![p.to_path_buf()];
    let mut i = 0;
    while i < dirs.len() {
        if let Ok(rd) = fs::read_dir(&dirs[i]) {
            for e in rd.flatten() {
                let c = e.path();
                if c.is_dir() {
                    dirs.push(c);
                } else {
                    let _ = Self::remove_one(&c);
                }
            }
        }
        i += 1;
    }
    // Reverse = deepest first, so a parent is only removed once it is empty.
    for d in dirs.iter().rev() {
        let _ = fs::remove_dir(d);
    }
    if p.exists() {
        Err(std::io::Error::new(std::io::ErrorKind::Other, "not removed"))
    } else {
        Ok(())
    }
}

    /// Empty the active recycle bin in the background, with real per-file
    /// progress and no shell enumeration — so it cannot hit the Explorer freeze.
    pub fn spawn_empty_recycle(&mut self) {
        let target = Self::recycle_target(self);
        let (tx, rx) = channel();
        thread::spawn(move || {
            let target = match target {
                Some(d) => d,
                None => {
                    // No per-user `$Recycle.Bin\{SID}` resolvable — essentially
                    // never, but fall back to the shell so the bin can clear.
                    let _ = crate::shell::empty_recycle_bin(None);
                    tx.send(JobMsg::Done("已清空回收站", String::new())).ok();
                    return;
                }
            };
            // Streaming self-delete. We deliberately avoid `SHEmptyRecycleBin`
            // / `SHQueryRecycleBin`: the shell enumerates the bin and recomputes
            // total size up front, and at tens-to-hundreds of thousands of items
            // that is exactly what makes File Explorer hang / fail to empty.
            // Instead we walk the names (cheap, no size calc) and delete each
            // `$I`/`$R` pair as we go, so the count emerges from the delete
            // itself — no pre-read of "how many files", and the work *is* the
            // actual deletion (zero extra overhead). We keep the full path list
            // up front only so every entry is guaranteed removed (an in-iterator
            // delete can skip under directory churn).
            let mut entries: Vec<std::path::PathBuf> = vec![];
            if let Ok(rd) = fs::read_dir(&target) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.starts_with('$') {
                        entries.push(e.path());
                    }
                }
            }
            let total = entries.len().max(1) as f32;
            let mut deleted = 0u64;
            let mut failed = 0u64;
            for p in &entries {
                if Self::force_remove(p).is_ok() {
                    deleted += 1;
                } else {
                    failed += 1;
                }
                tx.send(JobMsg::Deleted(deleted)).ok();
                tx.send(JobMsg::Progress(deleted as f32 / total)).ok();
            }
            if failed > 0 {
                // Say so instead of claiming success: a silent partial empty
                // is what made this look broken.
                tx.send(JobMsg::Done(
                    "回收站已清空，但 %s 项未能删除（可尝试以管理员身份运行后重试）",
                    failed.to_string(),
                ))
                .ok();
            } else {
                tx.send(JobMsg::Done("已清空回收站（删除 %s 个文件）", deleted.to_string())).ok();
            }
        });
        self.job = Some(Job {
            label: crate::lang::tr("清空回收站…", self.lang).into(),
            progress: 0.0,
            // We now drive a real percentage, so the bar is determinate.
            indeterminate: false,
            deleted: 0,
            note: None,
            err: None,
            rx,
            started: Instant::now(),
        });
        self.empty_open = false;
        self.status = crate::lang::tr("正在清空回收站…", self.lang).into();
    }

    /// True while something on screen is still changing.
    ///
    /// Calling `request_repaint()` unconditionally pins egui in continuous
    /// mode — a full redraw every frame, which costs an entire core while the
    /// window just sits there. Repaint only while there is a reason to.
    pub fn needs_repaint(&self) -> bool {
        // A status band highlighted for a few seconds, or a running job.
        if self.status_fresh || self.job.is_some() {
            return true;
        }
        // Sizes still being computed, or listings still being read.
        if !self.pending_sizes.is_empty() {
            return true;
        }
        // The frame right after the last background result landed: the queue
        // is empty by then, but the value has not been drawn yet.
        if let Some(at) = self.bg_signal {
            if at.elapsed() < std::time::Duration::from_millis(600) {
                return true;
            }
        }
        self.tabs
            .iter()
            .any(|t| t.panes.iter().any(|p| p.loading || p.rec_loading))
    }

    /// Record that a background result just arrived (see `needs_repaint`).
    pub fn mark_bg_activity(&mut self) {
        self.bg_signal = Some(Instant::now());
    }

    // ---------- misc ops ----------

    pub fn create_folder(&mut self) {
        let name = self.new_folder_name.trim().to_string();
        let dir = self.active_panel().cwd.clone();
        if name.is_empty() {
            self.status = crate::lang::tr("文件夹名为空", self.lang).into();
            return;
        }
        if self.active_panel().in_recycle {
            self.status = crate::lang::tr("不能在回收站内建文件夹", self.lang).into();
            return;
        }
        match fs::create_dir_all(dir.join(&name)) {
            Ok(_) => {
                self.status = format!("{} {name}", crate::lang::tr("已创建文件夹", self.lang));
                self.active_panel().load(&dir);
                self.apply_cached_sizes();
            }
            Err(e) => self.status = format!("{}: {e}", crate::lang::tr("创建失败", self.lang)),
        }
        self.new_folder_name.clear();
    }

    pub fn go_to(&mut self) {
        let p: PathBuf = self.go_path.trim().to_string().into();
        let to_right = self.go_target;
        let is_dir = p.is_dir();
        if is_dir {
            self.status = format!("{} {}", crate::lang::tr("已跳转到", self.lang), p.display());
        } else {
            self.status = format!("{}: {}", crate::lang::tr("不是目录", self.lang), p.display());
        }
        self.set_active_pane(!to_right);
        if is_dir {
            self.pane_of(!to_right).navigate(p);
            self.persist_layout();
        }
    }

    /// Double-click on a row of the active panel.
    pub fn open_selected(&mut self) {
        let name = match self.active_panel().selected.clone() {
            Some(n) => n,
            None => return,
        };
        if self.active_panel().in_recycle {
            if let Some(r) = self.active_panel().sel_rec().cloned() {
                self.rec_info = Some(RecInfo {
                    name: r.name,
                    orig_path: r.orig_path,
                    is_dir: r.is_dir,
                    size: r.size,
                    deleted_at: r.deleted_at,
                    suffix: r.suffix,
                });
            }
            return;
        }
        self.active_panel().open(&name);
    }

    /// Breadcrumb click: navigate the given panel to `to`.
    pub fn go_breadcrumb(&mut self, to: PathBuf, _is_left: bool) {
        // Addresses navigate the *active* pane (classic dual-pane rule):
        // click a pane to select it, then any address — this pane's, the
        // other pane's, the left rail's — navigates it. `_is_left` is the
        // pane whose breadcrumb was clicked; it only matters when that pane
        // is already the active one.
        let is_left = self.active_pane == 0;
        let panel = self.active_panel();
        panel.navigate(to.clone());
        self.status = format!(
            "{} {} {}",
            crate::lang::tr("已跳转到", self.lang),
            crate::lang::tr(if is_left { "左栏" } else { "右栏" }, self.lang),
            to.display()
        );
    }

    /// Move selection within the focused panel (keyboard). `delta` +1/-1, or
    /// `home`/`end` to jump.
    pub fn move_selection(&mut self, delta: i32, home: bool, end: bool) {
        let show_hidden = self.show_hidden;
        let panel = self.active_panel();
        if panel.in_recycle {
            panel.recycle_nav(delta, home, end);
            return;
        }
        let vis = panel.visible(show_hidden);
        if vis.is_empty() {
            return;
        }
        let cur = panel
            .selected
            .as_deref()
            .and_then(|n| vis.iter().position(|&i| panel.entries[i].name == n));
        let next = match cur {
            None => {
                if delta < 0 || end { vis.len() - 1 } else { vis.first().copied().unwrap_or(0) }
            }
            Some(pos) => {
                let np = pos as i32 + delta;
                if home {
                    0
                } else if end {
                    vis.len() - 1
                } else if np < 0 {
                    0
                } else if np >= vis.len() as i32 {
                    vis.len() - 1
                } else {
                    np as usize
                }
            }
        };
        let idx = *vis.get(next).unwrap_or(&0);
        let name = panel.entries[idx].name.clone();
        panel.selected = Some(name.clone());
        // Arrow-key navigation is single-select semantics (same as Explorer):
        // moving the focus row drops the multi-selection.
        panel.multi.clear();
        panel.multi.insert(name.clone());
        panel.anchor = Some(name);
    }

    // ---------- file actions (context menu) ----------

    /// Absolute path of a row, as text.
    pub fn abs_path_text(path: &std::path::Path) -> String {
        path.to_string_lossy().into_owned()
    }

    /// Relative path of a row, as text.
    ///
    /// Preferred base is the *other* pane's directory (that is the useful case:
    /// copy a path that is valid for a command running in the other pane);
    /// otherwise it falls back to the current pane and finally to the absolute
    /// path when the file is not underneath either of them.
    pub fn rel_path_text(&self, path: &std::path::Path, is_left: bool) -> String {
        let (cur, other) = if is_left {
            (&self.tabs[self.active_tab].panes[0].cwd, &self.tabs[self.active_tab].panes[1].cwd)
        } else {
            (&self.tabs[self.active_tab].panes[1].cwd, &self.tabs[self.active_tab].panes[0].cwd)
        };
        fs_util::relative_to(path, other)
            .or_else(|| fs_util::relative_to(path, cur))
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    }

    /// Delete several paths at once (Delete / Shift+Delete with a multi-
    /// selection, or the context menu acting on the whole selection). One bad
    /// item must not abort the rest, so failures are counted, not thrown.
    ///
    /// More than two items — or anything that contains a folder — goes to a
    /// worker thread: `remove_dir_all` / `move_to_recycle_bin` on a big tree
    /// blocks the UI thread for seconds otherwise.
    pub fn delete_paths(&mut self, paths: Vec<PathBuf>, permanent: bool) {
        let heavy = paths.len() > 2 || paths.iter().any(|p| p.is_dir());
        if heavy {
            self.spawn_delete_job(paths, permanent);
            return;
        }
        let mut ok = 0usize;
        let mut fail = 0usize;
        // Remember the first failure (file + reason) so a locked or read-only
        // item is never a silent no-op — the status bar names it.
        let mut first_err: Option<(String, String)> = None;
        for p in &paths {
            let res: Result<(), String> = if permanent {
                let is_dir = fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false);
                let r = if is_dir { fs::remove_dir_all(p) } else { fs::remove_file(p) };
                r.map_err(|e| e.to_string())
            } else {
                fs_util::move_to_recycle_bin(p)
            };
            match res {
                Ok(_) => ok += 1,
                Err(e) => {
                    fail += 1;
                    if first_err.is_none() {
                        let name = p
                            .file_name()
                            .map(|s| s.to_string_lossy().into_owned())
                            .unwrap_or_else(|| p.to_string_lossy().into_owned());
                        first_err = Some((name, e));
                    }
                }
            }
        }
        let verb = crate::lang::tr(if permanent { "已永久删除" } else { "已移到回收站" }, self.lang);
        self.status = match first_err {
            None => format!("{} {} {}", verb, ok, crate::lang::tr("项", self.lang)),
            Some((name, why)) => format!(
                "{} {} {}，{} {}：{}（{}）",
                verb,
                ok,
                crate::lang::tr("项", self.lang),
                fail,
                crate::lang::tr("个失败", self.lang),
                name,
                why
            ),
        };
        self.icons.invalidate(&crate::shellicon::Kind::Recycle);
        self.reload_both();
    }

    /// Ctrl+C / Ctrl+X: put every highlighted item on the clipboard. The
    /// Windows clipboard is written too, so a later Ctrl+V inside *another*
    /// program (Explorer, an editor) pastes the same files.
    pub fn copy_selection(&mut self, cut: bool) {
        let paths = self.active_panel().sel_paths();
        if paths.is_empty() {
            self.status = crate::lang::tr("没有选中项", self.lang).into();
            return;
        }
        let n = paths.len();
        self.clip = Some(if cut {
            Clip::Move(paths.clone())
        } else {
            Clip::Copy(paths.clone())
        });
        fs_util::set_clipboard_files(&paths);
        self.status = format!(
            "{} {} {}",
            crate::lang::tr(if cut { "已剪切" } else { "已复制" }, self.lang),
            n,
            crate::lang::tr("项", self.lang)
        );
    }

    /// Ctrl+A: highlight every visible entry of the active pane.
    pub fn select_all(&mut self) {
        let show_hidden = self.show_hidden;
        let panel = self.active_panel();
        if panel.in_recycle {
            // Multi-select in the bin is not wired up; single selection only.
            return;
        }
        let names: Vec<String> = panel
            .visible(show_hidden)
            .iter()
            .map(|&i| panel.entries[i].name.clone())
            .collect();
        if names.is_empty() {
            return;
        }
        let panel = self.active_panel();
        panel.multi = names.iter().cloned().collect();
        if panel.selected.is_none() {
            panel.selected = Some(names[0].clone());
        }
        panel.anchor = Some(names[0].clone());
        let n = names.len();
        self.status = format!(
            "{} {} {}",
            crate::lang::tr("已选择", self.lang),
            n,
            crate::lang::tr("项", self.lang)
        );
    }

    fn reload_both(&mut self) {
        let lc = self.p0().cwd.clone();
        let rc = self.p1().cwd.clone();
        self.p0().load(&lc);
        self.p1().load(&rc);
        self.apply_cached_sizes();
        self.persist_layout();
    }

    /// Open the built-in image viewer for `path`, building the list of every
    /// image in the active panel so ← / → can walk through them.
    /// Open the built-in image viewer for `path`. The image list is built from
    /// `listing` (the panel's already-loaded entries) when it is available, so
    /// opening a picture in a 50k-file folder does not re-read the whole
    /// directory. Falls back to a live `read_dir` only when `listing` is empty
    /// (e.g. the listing is still loading).
    pub fn open_viewer(&mut self, listing: &[crate::app::Entry], path: PathBuf) {
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut list: Vec<PathBuf> = Vec::new();
        if listing.is_empty() {
            // Fallback: read the directory fresh.
            if let Ok(rd) = fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let p = e.path();
                    let is_file = e.file_type().map(|t| t.is_file()).unwrap_or(false);
                    if !is_file {
                        continue;
                    }
                    let name = e.file_name().to_string_lossy().into_owned();
                    if crate::preview::is_image(&name) {
                        list.push(p);
                    }
                }
            }
        } else {
            // Reuse the in-memory listing — avoids 50k disk stat calls.
            for e in listing {
                if e.is_dir {
                    continue;
                }
                if crate::preview::is_image(&e.name) {
                    list.push(dir.join(&e.name));
                }
            }
        }
        list.sort_by(|a, b| {
            a.file_name()
                .unwrap_or_default()
                .cmp(b.file_name().unwrap_or_default())
        });
        let idx = list.iter().position(|p| *p == path).unwrap_or(0);
        self.viewer_list = if list.is_empty() {
            vec![path.clone()]
        } else {
            list
        };
        self.viewer_index = idx.min(self.viewer_list.len().saturating_sub(1));
        self.viewer = Some(path);
        self.viewer_zoom = 0.0; // 0.0 = fit to window
        self.viewer_pan = egui::Vec2::ZERO;
    }

    /// Step through the image list opened in the viewer (← / →). Wraps around.
    pub fn viewer_nav(&mut self, delta: isize) {
        if self.viewer_list.is_empty() {
            return;
        }
        let n = self.viewer_list.len() as isize;
        let mut i = self.viewer_index as isize + delta;
        if i < 0 {
            i = n - 1;
        } else if i >= n {
            i = 0;
        }
        self.viewer_index = i as usize;
        self.viewer = Some(self.viewer_list[self.viewer_index].clone());
        self.viewer_zoom = 0.0;
        self.viewer_pan = egui::Vec2::ZERO;
    }

    /// Open the image-conversion dialog for `path`. The source is decoded
    /// here (once); a failure only sets the status line.
    pub fn open_convert(
        &mut self,
        mode: crate::convert::ConvertMode,
        path: PathBuf,
        ctx: &egui::Context,
    ) {
        match crate::convert::ConvertState::open(mode, &path, ctx) {
            Some(cv) => self.convert = Some(cv),
            None => {
                self.status = crate::lang::tr("无法读取图片", self.lang).into();
            }
        }
    }

    /// Right-click menu of the full-window image viewer: the usual copy /
    /// reveal actions plus the two conversion entries.
    pub fn viewer_context_menu(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        let path = path.to_path_buf();
        if ui
            .button(crate::lang::tr("复制图片", self.lang))
            .clicked()
        {
            self.status = if crate::fs_util::set_clipboard_image(&path) {
                crate::lang::tr("已复制图片", self.lang).into()
            } else {
                crate::lang::tr("复制失败", self.lang).into()
            };
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("复制文件", self.lang))
            .clicked()
        {
            crate::fs_util::set_clipboard_files(std::slice::from_ref(&path));
            self.status = crate::lang::tr("已复制文件", self.lang).into();
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("复制文件路径", self.lang))
            .clicked()
        {
            let t = Self::abs_path_text(&path);
            ui.ctx().copy_text(t.clone());
            self.status = format!("{}: {t}", crate::lang::tr("已复制路径", self.lang));
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("在资源管理器中显示", self.lang))
            .clicked()
        {
            crate::fs_util::reveal_in_explorer(&path);
            ui.close_menu();
        }
        ui.separator();
        if ui
            .button(crate::lang::tr("转换为 ICO", self.lang))
            .clicked()
        {
            self.open_convert(
                crate::convert::ConvertMode::Ico,
                path.clone(),
                ui.ctx(),
            );
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("转换为 PNG", self.lang))
            .clicked()
        {
            self.open_convert(
                crate::convert::ConvertMode::Png,
                path.clone(),
                ui.ctx(),
            );
            ui.close_menu();
        }
        ui.separator();
        if ui.button(crate::lang::tr("属性", self.lang)).clicked() {
            self.show_props(path.clone());
            ui.close_menu();
        }
    }

    /// Text content of `path` for the side-pane preview, or `None` when the
    /// file is not previewable text: bigger than 10 KiB, a directory, or
    /// binary (a NUL byte anywhere is a reliable "not text" test).
    ///
    /// The result is cached per path — a repaint must not re-read the disk.
    pub fn text_preview(&mut self, path: &std::path::Path) -> Option<String> {
        const MAX_BYTES: u64 = 10 * 1024;
        if let Some((p, t)) = &self.preview_text {
            if p == path {
                return Some(t.clone());
            }
        }
        let md = std::fs::metadata(path).ok()?;
        if !md.is_file() || md.len() > MAX_BYTES {
            self.preview_text = None;
            return None;
        }
        let bytes = std::fs::read(path).ok()?;
        if bytes.contains(&0) {
            self.preview_text = None;
            return None;
        }
        let text = String::from_utf8_lossy(&bytes).into_owned();
        self.preview_text = Some((path.to_path_buf(), text.clone()));
        Some(text)
    }

    pub fn start_rename(&mut self, path: PathBuf) {
        self.rename_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.rename_target = Some(path);
        self.rename_open = true;
    }

    pub fn confirm_rename(&mut self) {
        let Some(src) = self.rename_target.take() else {
            self.rename_open = false;
            return;
        };
        let name = self.rename_name.trim().to_string();
        self.rename_open = false;
        if name.is_empty() {
            self.status = crate::lang::tr("新名称为空", self.lang).into();
            return;
        }
        let dst = src.with_file_name(&name);
        if dst == src {
            return;
        }
        if dst.exists() {
            self.status = format!("{}: {name}", crate::lang::tr("目标已存在", self.lang));
            return;
        }
        self.status = match fs::rename(&src, &dst) {
            Ok(_) => format!("{} {name}", crate::lang::tr("已重命名为", self.lang)),
            Err(e) => format!("{}: {e}", crate::lang::tr("重命名失败", self.lang)),
        };
        self.reload_both();
    }

    pub fn show_props(&mut self, path: PathBuf) {
        let md = fs::metadata(&path).ok();
        let is_dir = md.as_ref().map(|m| m.is_dir()).unwrap_or(false);
        let size = md.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = md
            .as_ref()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let ctime = md
            .as_ref()
            .and_then(|m| m.created().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let entries = if is_dir {
            fs::read_dir(&path).ok().map(|rd| rd.flatten().count())
        } else {
            None
        };
        self.props = Some(PropsInfo {
            name: path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string()),
            path,
            is_dir,
            size,
            mtime,
            ctime,
            entries,
        });
    }

    /// Explorer-like right-click menu for a listing row.
    pub fn file_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        name: &str,
        is_dir: bool,
        path: &std::path::Path,
    ) {
        let path = path.to_path_buf();
        let is_left = self.active_pane == 0;
        let is_img = !is_dir && crate::preview::is_image(name);

        if is_dir {
            if ui.button(crate::lang::tr("打开", self.lang)).clicked() {
                let p = self.pane_of(is_left);
                p.navigate(path.clone());
                self.apply_cached_sizes();
                ui.close_menu();
            }
            // Folder-only: send it to either pane, or pin it to Quick Access.
            ui.separator();
            if ui.button(crate::lang::tr("在右栏打开", self.lang)).clicked() {
                self.open_path_in_right_pane(path.clone());
                ui.close_menu();
            }
            if ui.button(crate::lang::tr("在左栏打开", self.lang)).clicked() {
                let p = self.pane_of(true);
                p.navigate(path.clone());
                self.apply_cached_sizes();
                self.persist_layout();
                ui.close_menu();
            }
        } else if ui.button(crate::lang::tr("打开", self.lang)).clicked() {
            fs_util::open_default_file(&path);
            ui.close_menu();
        }
        // Both folders and files can be dropped into 收藏 (the rail holds
        // either a jump target or a one-click launcher).
        self.favorite_menu_item(ui, &path);
        // "Open with" entries — only shown when the program is actually
        // installed, so a menu item never dead-ends.
        if crate::fs_util::vscode_exe().is_some()
            && ui
                .button(crate::lang::tr("通过 VS Code 打开", self.lang))
                .clicked()
        {
            crate::fs_util::open_with_vscode(&path);
            ui.close_menu();
        }
        if !is_dir
            && ui
                .button(crate::lang::tr("通过记事本打开", self.lang))
                .clicked()
        {
            crate::fs_util::open_with_notepad(&path);
            ui.close_menu();
        }
        if is_img && ui.button(crate::lang::tr("预览图片", self.lang)).clicked() {
            self.open_viewer(&[], path.clone());
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("在资源管理器中显示", self.lang)).clicked() {
            fs_util::reveal_in_explorer(&path);
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("在此打开终端", self.lang)).clicked() {
            let dir = if is_dir { path.clone() } else { path.parent().map(|p| p.to_path_buf()).unwrap_or_default() };
            fs_util::open_terminal(&dir);
            ui.close_menu();
        }
        ui.separator();

        if ui.button(crate::lang::tr("复制绝对路径", self.lang)).clicked() {
            let t = Self::abs_path_text(&path);
            ui.ctx().copy_text(t.clone());
            self.status = format!("{}: {t}", crate::lang::tr("已复制绝对路径", self.lang));
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("复制相对路径", self.lang)).clicked() {
            let t = self.rel_path_text(&path, is_left);
            ui.ctx().copy_text(t.clone());
            self.status = format!("{}: {t}", crate::lang::tr("已复制相对路径", self.lang));
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("复制文件名", self.lang)).clicked() {
            ui.ctx().copy_text(name.to_string());
            self.status = format!("{}: {name}", crate::lang::tr("已复制文件名", self.lang));
            ui.close_menu();
        }
        ui.separator();

        // Cut / copy / delete act on the whole selection when the row that
        // was right-clicked is part of it (same rule as Explorer); on a lone
        // row they act on that row alone.
        //
        // NB: this menu runs while `render_list` / `render_grid` have
        // `mem::take`n `entries` out of the panel, so `sel_paths()` (which
        // filters by the listing) would return an empty vector — the
        // "已复制 0 项" bug. Build the target list from `multi` + `cwd`
        // directly; neither field depends on the listing.
        let (cwd, in_multi) = {
            let panel = self.active_panel();
            (panel.cwd.clone(), panel.multi.contains(name))
        };
        let targets: Vec<PathBuf> = if in_multi {
            let mut names: Vec<String> = self.active_panel().multi.iter().cloned().collect();
            names.sort();
            names.into_iter().map(|n| cwd.join(n)).collect()
        } else {
            vec![path.clone()]
        };
        if ui.button(crate::lang::tr("剪切", self.lang)).clicked() {
            self.clip_paths(targets.clone(), true);
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("复制", self.lang)).clicked() {
            self.clip_paths(targets.clone(), false);
            ui.close_menu();
        }
        let can_paste = self.clip.is_some()
            || fs_util::get_clipboard_files().is_some()
            || fs_util::clipboard_has_image();
        if ui.add_enabled(can_paste, egui::Button::new(crate::lang::tr("粘贴到此处", self.lang))).clicked() {
            self.do_paste();
            ui.close_menu();
        }
        ui.separator();

        if ui.button(crate::lang::tr("重命名", self.lang)).clicked() {
            self.start_rename(path.clone());
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("删除（移到回收站）", self.lang)).clicked() {
            // Both menu deletes go through the confirmation dialog — a
            // straight call looked like a silent no-op when the shell refused
            // (locked file), and permanent delete must never be one click.
            self.delete_targets = targets.clone();
            self.delete_permanent = false;
            self.delete_open = true;
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("永久删除", self.lang)).clicked() {
            self.delete_targets = targets;
            self.delete_permanent = true;
            self.delete_open = true;
            ui.close_menu();
        }
        ui.separator();
        if ui.button(crate::lang::tr("属性", self.lang)).clicked() {
            self.show_props(path.clone());
            ui.close_menu();
        }
    }

    /// Context menu on empty space of a pane. `sel_rec` is the focused
    /// recycle item, resolved by the caller from its local listing snapshot —
    /// `sel_rec()` on the panel would read a `mem::take`n field here.
    pub fn pane_context_menu(
        &mut self,
        ui: &mut egui::Ui,
        is_left: bool,
        sel_rec: Option<&fs_util::RecItem>,
    ) {
        // Inside the Recycle-Bin view the normal folder actions (new folder,
        // paste) make no sense — Explorer shows 清空/还原 there instead.
        if self.pane_of(is_left).in_recycle {
            if ui
                .button(crate::lang::tr("清空回收站", self.lang))
                .clicked()
            {
                self.set_active_pane(is_left);
                self.empty_open = true;
                ui.close_menu();
            }
            // 还原选中的: the focused row (`selected` holds the recycle suffix;
            // the full item is passed in by the caller — see above).
            let sel = sel_rec.cloned();
            let can_restore = sel
                .as_ref()
                .map(|r| !r.orig_path.trim().is_empty())
                .unwrap_or(false);
            if ui
                .add_enabled(
                    can_restore,
                    egui::Button::new(crate::lang::tr("还原选中的", self.lang)),
                )
                .on_hover_text(crate::lang::tr("把选中的项目移回原来的位置", self.lang))
                .clicked()
            {
                if let Some(r) = sel {
                    self.restore_rec(is_left, &r);
                }
                ui.close_menu();
            }
            if ui.button(crate::lang::tr("刷新", self.lang)).clicked() {
                let cwd = self.pane_of(is_left).cwd.clone();
                self.pane_of(is_left).load(&cwd);
                self.apply_cached_sizes();
                ui.close_menu();
            }
            return;
        }
        if ui.button(crate::lang::tr("编辑地址", self.lang)).clicked() {
            self.begin_addr_edit(is_left);
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("新建文件夹", self.lang)).clicked() {
            self.set_active_pane(is_left);
            self.new_folder_open = true;
            ui.close_menu();
        }
        let can_paste = self.clip.is_some()
            || fs_util::get_clipboard_files().is_some()
            || fs_util::clipboard_has_image();
        if ui.add_enabled(can_paste, egui::Button::new(crate::lang::tr("粘贴", self.lang))).clicked() {
            self.set_active_pane(is_left);
            self.do_paste();
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("刷新", self.lang)).clicked() {
            let cwd = self.pane_of(is_left).cwd.clone();
            self.pane_of(is_left).load(&cwd);
            self.apply_cached_sizes();
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("在此打开终端", self.lang)).clicked() {
            let cwd = self.pane_of(is_left).cwd.clone();
            fs_util::open_terminal(&cwd);
            ui.close_menu();
        }
        let cwd_fav = self.pane_of(is_left).cwd.clone();
        self.favorite_menu_item(ui, &cwd_fav);
        if ui.button(crate::lang::tr("复制当前目录路径", self.lang)).clicked() {
            let cwd = self.pane_of(is_left).cwd.clone();
            let t = cwd.to_string_lossy().into_owned();
            ui.ctx().copy_text(t.clone());
            self.status = format!("{}: {t}", crate::lang::tr("已复制目录", self.lang));
            ui.close_menu();
        }
    }

    // ---------- recycle-bin view ----------

    /// Explorer-like right-click menu on a Recycle-Bin row. `is_left` is the
    /// pane the row lives in (not necessarily the active one).
    pub fn rec_context_menu(&mut self, ui: &mut egui::Ui, is_left: bool, it: &fs_util::RecItem) {
        self.set_active_pane(is_left);
        let has_orig = !it.orig_path.trim().is_empty();
        let payload = self
            .pane_of(is_left)
            .cwd
            .join(format!("$R{}", it.suffix));
        let can_restore = has_orig && payload.exists();
        if ui
            .add_enabled(
                can_restore,
                egui::Button::new(crate::lang::tr("还原", self.lang)),
            )
            .on_hover_text(if has_orig {
                crate::lang::tr("把文件移回原来的位置", self.lang)
            } else {
                crate::lang::tr("没有原始路径信息，无法还原", self.lang)
            })
            .clicked()
        {
            self.restore_rec(is_left, it);
            ui.close_menu();
        }
        if ui
            .button(crate::lang::tr("彻底删除", self.lang))
            .on_hover_text(crate::lang::tr("直接删除回收站文件，不可恢复", self.lang))
            .clicked()
        {
            self.delete_rec_suffix = Some(it.suffix.clone());
            self.delete_open = true;
            ui.close_menu();
        }
        if ui
            .add_enabled(
                has_orig,
                egui::Button::new(crate::lang::tr("在资源管理器中显示", self.lang)),
            )
            .on_hover_text(crate::lang::tr("打开它原来所在的文件夹", self.lang))
            .clicked()
        {
            fs_util::reveal_in_explorer(std::path::Path::new(&it.orig_path));
            ui.close_menu();
        }
        if ui
            .add_enabled(
                has_orig,
                egui::Button::new(crate::lang::tr("复制原始路径", self.lang)),
            )
            .clicked()
        {
            let t = it.orig_path.clone();
            ui.ctx().copy_text(t.clone());
            self.status = format!("{}: {t}", crate::lang::tr("已复制原始路径", self.lang));
            ui.close_menu();
        }
        // "清空回收站" must be reachable from anywhere inside the bin — both
        // the background menu (pane_context_menu) and this per-row menu.
        ui.separator();
        if ui.button(crate::lang::tr("清空回收站", self.lang)).clicked() {
            self.set_active_pane(is_left);
            self.empty_open = true;
            ui.close_menu();
        }
        if ui.button(crate::lang::tr("属性", self.lang)).clicked() {
            self.rec_info = Some(RecInfo {
                name: it.name.clone(),
                orig_path: it.orig_path.clone(),
                is_dir: it.is_dir,
                size: it.size,
                deleted_at: it.deleted_at,
                suffix: it.suffix.clone(),
            });
            ui.close_menu();
        }
    }

    /// Explorer's 还原: move the `$R` payload back to the path it was
    /// deleted from and drop the `$I` metadata. A name clash gets " (2)"
    /// appended, exactly like Explorer. Runs synchronously — on the same
    /// volume a rename is instant whatever the size.
    pub fn restore_rec(&mut self, is_left: bool, it: &fs_util::RecItem) {
        let orig = it.orig_path.trim().to_string();
        if orig.is_empty() {
            self.status = crate::lang::tr("无法还原：没有原始路径信息", self.lang).into();
            return;
        }
        let dir = self.pane_of(is_left).cwd.clone();
        let payload = dir.join(format!("$R{}", it.suffix));
        let meta = dir.join(format!("$I{}", it.suffix));
        if !payload.exists() {
            self.status = format!(
                "{}: {}",
                crate::lang::tr("还原失败", self.lang),
                payload.display()
            );
            return;
        }
        let dest = unique_dest_path(std::path::Path::new(&orig));
        if let Some(parent) = dest.parent() {
            if !parent.as_os_str().is_empty() {
                if let Err(e) = fs::create_dir_all(parent) {
                    self.status = format!("{}: {e}", crate::lang::tr("还原失败", self.lang));
                    return;
                }
            }
        }
        if let Err(e) = fs::rename(&payload, &dest) {
            self.status = format!("{}: {e}", crate::lang::tr("还原失败", self.lang));
            return;
        }
        // Metadata without a payload is junk; keep the bin tidy.
        let _ = fs::remove_file(&meta);
        self.status = format!(
            "{} {}",
            crate::lang::tr("已还原到", self.lang),
            dest.display()
        );
        self.pane_of(is_left).load(&dir);
        self.apply_cached_sizes();
        self.icons.invalidate(&crate::shellicon::Kind::Recycle);
    }

    /// Open `dir` in a new tab and bring the app window to the front.
    /// Used by the convert dialog's "open output folder": the folder shows
    /// up in *this* file manager, not in a separate Explorer window that
    /// may open behind it.
    pub fn open_dir_in_new_tab(&mut self, dir: PathBuf, ctx: &egui::Context) {
        self.new_tab();
        self.p0().navigate(dir.clone());
        self.apply_cached_sizes();
        self.status = format!(
            "{} {}",
            crate::lang::tr("新标签页", self.lang),
            dir.display()
        );
        // The dialog may live in the viewer's own OS window: target the root
        // viewport explicitly so the *main* window comes forward.
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
    }
}

// ---------- free helpers ----------

/// Destination for a copy that lands in the folder the original already
/// lives in: `name - 副本.ext`, then `name - 副本 (2).ext`, … (Explorer's
/// "paste here" rule). Without this a same-folder paste is a silent no-op.
fn unique_copy_path(orig: &std::path::Path, lang: crate::lang::Lang) -> std::path::PathBuf {
    let tag = crate::lang::tr("副本", lang);
    let parent = orig.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = orig
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = orig
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty());
    let with = |suffix: &str| match &ext {
        Some(e) => parent.join(format!("{stem}{suffix}.{e}")),
        None => parent.join(format!("{stem}{suffix}")),
    };
    let first = with(&format!(" - {tag}"));
    if !first.exists() {
        return first;
    }
    for n in 2..=999u32 {
        let cand = with(&format!(" - {tag} ({n})"));
        if !cand.exists() {
            return cand;
        }
    }
    orig.to_path_buf()
}

/// `orig` when the path is free, otherwise `name (2).ext`, `name (3).ext`, …
/// — Explorer's collision rule when restoring from the Recycle Bin.
fn unique_dest_path(orig: &std::path::Path) -> std::path::PathBuf {
    if !orig.exists() {
        return orig.to_path_buf();
    }
    let parent = orig.parent().unwrap_or_else(|| std::path::Path::new("."));
    let stem = orig
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = orig
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .filter(|e| !e.is_empty());
    for n in 2..=999u32 {
        let name = match &ext {
            Some(e) => format!("{stem} ({n}).{e}"),
            None => format!("{stem} ({n})"),
        };
        let cand = parent.join(name);
        if !cand.exists() {
            return cand;
        }
    }
    orig.to_path_buf() // 999 clashes: give up, the rename reports the error
}

/// Recursive folder size, scanned with one bulk Win32 enumeration per
/// directory and a hard budget.
///
/// Plain `read_dir` + `metadata` costs one system call *per file*, so a folder
/// with 200k files means 200k round trips into the kernel. `FileIdBothDirectoryInfo`
/// instead returns a whole page of entries — name, attributes and size — in a
/// single call, which is an order of magnitude cheaper.
///
/// Returns `(bytes, complete)`. `complete == false` means the budget ran out
/// mid-scan: the value is a lower bound and is shown with a `~` prefix. It is
/// cached all the same, so a folder that blew the budget once is not rescanned
/// on every visit.
///
/// Use this for *passive* scans (browsing). The manual 重新计算大小 action
/// wants an exact number and uses `scan_dir_size_full` instead — the budget
/// is exactly what turned a 700 GB folder into `~140 GB` there.
pub fn scan_dir_size(src: &std::path::Path) -> (u64, bool) {
    if !src.is_dir() {
        return (fs::metadata(src).map(|m| m.len()).unwrap_or(0), true);
    }
    let mut budget = ScanBudget::new();
    let mut total: u64 = 0;
    // Breadth-first with an explicit queue: deep trees must not blow the
    // thread's stack, and — more importantly — a budget stop must not leave a
    // whole top-level branch unvisited. A stack is depth-first, so the *last*
    // sub-directory is scanned first and the *first* sibling is the one dropped
    // when the 400 ms run out: the parent then reports a number far below its
    // real size, and a 400 MB child can vanish from the total entirely.
    // Level order visits every directory at depth N before any at depth N+1,
    // so a stopped scan is only short in the deep tail, never by a branch.
    let mut queue: std::collections::VecDeque<std::path::PathBuf> =
        std::collections::VecDeque::new();
    queue.push_back(src.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        scan_one_dir(&dir, &mut budget, &mut queue, &mut total);
        if budget.exhausted {
            return (total, false);
        }
    }
    (total, true)
}

/// Full scan: same breadth-first walk, **no** entry-count or time budget.
///
/// Only for the explicit 重新计算大小 action, where the user asked for the
/// real number. A 700 GB folder takes tens of seconds here (I/O bound — there
/// is no faster way to touch every file), so progress is streamed back over
/// `prog` as `(root, bytes so far)` after every directory. The total is never
/// pre-counted: the bytes grow as the walk advances, which is what keeps this
/// from doing an expensive "count everything first" pass.
pub fn scan_dir_size_full(src: &std::path::Path, prog: &Sender<(std::path::PathBuf, u64)>) -> u64 {
    if !src.is_dir() {
        return fs::metadata(src).map(|m| m.len()).unwrap_or(0);
    }
    let mut budget = ScanBudget::unlimited();
    let mut total: u64 = 0;
    let mut queue: std::collections::VecDeque<std::path::PathBuf> =
        std::collections::VecDeque::new();
    queue.push_back(src.to_path_buf());
    while let Some(dir) = queue.pop_front() {
        scan_one_dir(&dir, &mut budget, &mut queue, &mut total);
        // One clone per directory is noise next to the I/O of reading it.
        prog.send((src.to_path_buf(), total)).ok();
    }
    total
}

/// Cost ceiling for a single folder scan.
///
/// Without this, opening a folder that contains `node_modules` or a game
/// library keeps the worker (and the disk) busy for minutes. Stopping early
/// and showing `~` keeps the UI responsive; the number is still useful.
const SCAN_MAX_ENTRIES: u32 = 150_000;
const SCAN_MAX_MS: u64 = 400;
/// How often the clock is consulted (checking on every entry is its own cost).
const SCAN_CHECK_EVERY: u32 = 512;

struct ScanBudget {
    entries: u32,
    deadline: std::time::Instant,
    exhausted: bool,
    limited: bool,
}

impl ScanBudget {
    fn new() -> Self {
        Self {
            entries: 0,
            deadline: std::time::Instant::now() + std::time::Duration::from_millis(SCAN_MAX_MS),
            exhausted: false,
            limited: true,
        }
    }

    /// No ceiling at all — only the manual 重新计算大小 action scans this way.
    fn unlimited() -> Self {
        Self {
            entries: 0,
            deadline: std::time::Instant::now(),
            exhausted: false,
            limited: false,
        }
    }

    /// Charge one directory entry; false once the budget is gone.
    fn spend(&mut self) -> bool {
        if self.exhausted {
            return false;
        }
        if !self.limited {
            return true;
        }
        self.entries += 1;
        if self.entries >= SCAN_MAX_ENTRIES {
            self.exhausted = true;
            return false;
        }
        if self.entries % SCAN_CHECK_EVERY == 0 && std::time::Instant::now() >= self.deadline {
            self.exhausted = true;
            return false;
        }
        true
    }
}

// --- Win32 bulk directory enumeration (hand-rolled FFI, no new dependency) ---

const GENERIC_READ: u32 = 0x8000_0000;
const FILE_SHARE_ALL: u32 = 0x0000_0007; // read | write | delete
const OPEN_EXISTING: u32 = 3;
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
/// Junctions / symlinks: never followed, or a cycle would spin forever and
/// the same files would be counted twice.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// `FILE_INFO_BY_HANDLE_CLASS`: FileIdBothDirectoryInfo.
const FILE_ID_BOTH_DIR_INFO: i32 = 10;

extern "system" {
    fn CreateFileW(
        name: *const u16,
        access: u32,
        share: u32,
        sec: *const (),
        disp: u32,
        flags: u32,
        tmpl: *mut (),
    ) -> isize;
    fn GetFileInformationByHandleEx(file: isize, class: i32, buf: *mut u8, len: u32) -> i32;
    fn CloseHandle(file: isize) -> i32;
}

/// `FILE_ID_BOTH_DIR_INFO` up to (not including) its final `FileName[1]`.
/// `#[repr(C)]` reproduces the C layout, padding included.
#[repr(C)]
struct DirInfoHeader {
    next_entry_offset: u32,
    _file_index: u32,
    _creation_time: i64,
    _last_access_time: i64,
    _last_write_time: i64,
    _change_time: i64,
    end_of_file: i64,
    _allocation_size: i64,
    file_attributes: u32,
    file_name_length: u32,
    _ea_size: u32,
    _short_name_length: i8,
    _short_name: [u16; 12],
    _file_id: i64,
}
/// `FileName` starts right after the header, which is what
/// `offsetof(FILE_ID_BOTH_DIR_INFO, FileName)` is on Windows.
const NAME_OFFSET: usize = std::mem::size_of::<DirInfoHeader>();
const _: () = assert!(std::mem::size_of::<DirInfoHeader>() == 104);

/// Sum one directory's files and queue its sub-directories.
///
/// Unreadable directories (access denied, gone mid-scan) are skipped, not
/// fatal: the caller gets a partial total like any other budget stop.
fn scan_one_dir(
    dir: &std::path::Path,
    budget: &mut ScanBudget,
    pending: &mut std::collections::VecDeque<std::path::PathBuf>,
    total: &mut u64,
) {
    use std::os::windows::ffi::OsStrExt;
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let handle = unsafe {
        CreateFileW(
            wide.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_ALL,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        )
    };
    if handle <= 0 {
        return;
    }
    let mut buf = vec![0u8; 64 * 1024];
    'pages: loop {
        let ok = unsafe {
            GetFileInformationByHandleEx(handle, FILE_ID_BOTH_DIR_INFO, buf.as_mut_ptr(), buf.len() as u32)
        };
        if ok == 0 {
            // No more entries, or an error we cannot do anything about:
            // either way this directory is done.
            break;
        }
        let mut offset = 0usize;
        loop {
            let base = unsafe { buf.as_ptr().add(offset) };
            let info = unsafe { &*(base as *const DirInfoHeader) };
            let chars = info.file_name_length as usize / 2;
            if chars > 0 && offset + NAME_OFFSET + chars * 2 <= buf.len() {
                let name = unsafe {
                    std::slice::from_raw_parts(base.add(NAME_OFFSET) as *const u16, chars)
                };
                let name = String::from_utf16_lossy(name);
                if name != "." && name != ".." {
                    let is_dir = info.file_attributes & FILE_ATTRIBUTE_DIRECTORY != 0;
                    let is_link = info.file_attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0;
                    if is_dir {
                        if !is_link {
                            pending.push_back(dir.join(&name));
                        }
                    } else {
                        *total += info.end_of_file.max(0) as u64;
                    }
                }
            }
            if !budget.spend() {
                break 'pages;
            }
            if info.next_entry_offset == 0 {
                break;
            }
            offset += info.next_entry_offset as usize;
        }
    }
    unsafe {
        CloseHandle(handle);
    }
}

/// Copy a tree with progress reporting.
fn copy_tree(
    src: &std::path::Path,
    dest: &std::path::Path,
    tx: &Sender<JobMsg>,
    total: u64,
) -> Result<(), String> {
    let mut done: u64 = 0;
    copy_walk(src, dest, tx, total, &mut done)?;
    Ok(())
}

fn copy_walk(
    src: &std::path::Path,
    dest: &std::path::Path,
    tx: &Sender<JobMsg>,
    total: u64,
    done: &mut u64,
) -> Result<(), String> {
    if src.is_dir() {
        fs::create_dir_all(dest).map_err(|e| e.to_string())?;
        let rd = fs::read_dir(src).map_err(|e| e.to_string())?;
        for e in rd {
            let e = e.map_err(|e| e.to_string())?;
            let sp = e.path();
            let dp = dest.join(e.file_name());
            copy_walk(&sp, &dp, tx, total, done)?;
        }
    } else {
        let size = fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        fs::copy(src, dest).map_err(|e| e.to_string())?;
        *done = done.saturating_add(size);
        if total > 0 {
            tx.send(JobMsg::Progress(*done as f32 / total as f32)).ok();
        }
    }
    Ok(())
}
