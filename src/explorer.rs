//! `Explorer`: top-level state, filesystem operations and background jobs.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread;
use std::time::Instant;

use crate::app::{Clip, Panel};
use crate::cache::Store;
use crate::fs_util::{self, DriveInfo};

/// Messages from background work.
pub enum JobMsg {
    Progress(f32),
    /// `(key, arg)`: `key` is a translation key (may contain `%s`), `arg` is
    /// substituted for `%s` when non-empty. Lets job threads stay language-agnostic.
    Done(&'static str, String),
    Failed(String),
}

/// A running background job (empty recycle bin, big copy/move).
pub struct Job {
    pub kind: JobKind,
    pub label: String,
    pub progress: f32,
    pub note: Option<(&'static str, String)>,
    pub err: Option<String>,
    pub rx: Receiver<JobMsg>,
}

/// What a `Job` is doing. Only used to trigger the right follow-up work
/// (the Recycle Bin glyph flips full/empty, so its cached icon must be
/// dropped when a job empties the bin).
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    CopyMove,
    EmptyRecycle,
}

impl Job {
    fn finish(&mut self, m: JobMsg) {
        match m {
            JobMsg::Progress(p) => self.progress = p,
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
    // quick access (system jump-list pins + user's own pins)
    pub system_qa: Vec<PathBuf>,
    pub user_pins: Vec<PathBuf>,
    pub qa_rx: Option<Receiver<Vec<PathBuf>>>,
    // folder sizes
    pub store: Store,
    pub size_tx: Sender<PathBuf>,
    pub size_rx: Receiver<(PathBuf, u64, u64)>, // (path, dir_mtime, size)
    pub pending_sizes: HashSet<PathBuf>,
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
    /// Whether the Explorer context-menu entry is installed. Persisted as
    /// `ctx_menu`; the actual registry write happens in the standalone
    /// PowerShell script, never in the app itself.
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

impl Explorer {
    pub fn new(
        store: Store,
        size_tx: Sender<PathBuf>,
        size_rx: Receiver<(PathBuf, u64, u64)>,
    ) -> Self {
        let (rec_tx, rec_rx) = channel::<(PathBuf, Vec<fs_util::RecItem>)>();
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
            .unwrap_or(1.0)
            .clamp(0.8, 1.6);
        // Read-only probe of the actual registry state (the app never writes it).
        let ctx_menu = crate::fs_util::context_menu_installed();

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
            user_pins: store.qa_pins(),
            qa_rx: None,
            store,
            size_tx,
            size_rx,
            pending_sizes: HashSet::new(),
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
        me.restore_layout();
        me.restore_columns();
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
    }

    /// Switch light/dark and persist it.
    pub fn set_dark(&mut self, on: bool) {
        self.dark = on;
        self.store.set_meta("dark", if on { "1" } else { "0" });
        self.apply_frame_theme();
    }

    /// Set the UI font scale and persist it.
    pub fn set_font_scale(&mut self, scale: f32) {
        self.font_scale = scale.clamp(0.8, 1.6);
        self.store.set_meta("font_scale", &format!("{:.2}", self.font_scale));
    }

    /// Open the folder that holds the context-menu install/uninstall scripts.
    ///
    /// The app itself never writes `HKCU\Software\Classes` — an unsigned
    /// binary doing that quietly is a pattern security tools flag. The
    /// registry change is done by the explicit PowerShell script the user
    /// launches from that folder.
    pub fn open_ctx_menu_scripts(&mut self) {
        match crate::fs_util::open_context_menu_script_dir() {
            Some(dir) => {
                self.status = crate::lang::tr("已打开脚本所在目录，右键“右键菜单-安装.ps1”运行", self.lang)
                    .into();
                let _ = dir;
            }
            None => {
                self.status = crate::lang::tr("找不到脚本所在目录", self.lang).into();
            }
        }
        // Re-read the (read-only) registry state so the badge is accurate if
        // the user ran the script in the meantime.
        self.ctx_menu = crate::fs_util::context_menu_installed();
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
            let list = crate::quickaccess::load_windows_quick_access();
            tx.send(list).ok();
        });
    }

    /// Drain the Quick-Access loader channel once its background read finishes.
    /// The (empty) receiver is kept as a "loaded" sentinel so we never re-spawn.
    pub fn poll_quick_access(&mut self) {
        if let Some(rx) = &self.qa_rx {
            if let Ok(list) = rx.try_recv() {
                let n = list.len();
                self.system_qa = list;
                if n > 0 {
                    self.status = format!("{} {n} {}", crate::lang::tr("已加载", self.lang), crate::lang::tr("个系统快速访问文件夹", self.lang));
                }
            }
        }
    }

    /// Combined Quick-Access list: the user's own pins first, then the
    /// system pins (skipping any path the user already pinned).
    pub fn quick_access_list(&self) -> Vec<(PathBuf, bool)> {
        let mut out: Vec<(PathBuf, bool)> = Vec::new();
        for p in &self.user_pins {
            if !out.iter().any(|(q, _)| q == p) {
                out.push((p.clone(), true));
            }
        }
        for p in &self.system_qa {
            if !out.iter().any(|(q, _)| q == p) {
                out.push((p.clone(), false));
            }
        }
        out
    }

    /// Pin the active panel's current directory to the user Quick-Access list.
    pub fn pin_current(&mut self) {
        let cwd = self.active_panel().cwd.clone();
        if cwd.is_dir() && !self.user_pins.iter().any(|p| *p == cwd) {
            self.user_pins.push(cwd.clone());
            self.store.set_qa_pins(&self.user_pins);
            self.status = format!("{}: {}", crate::lang::tr("已固定到快速访问", self.lang), cwd.display());
        } else if self.user_pins.iter().any(|p| *p == cwd) {
            self.status = crate::lang::tr("该目录已在快速访问中", self.lang).into();
        }
    }

    /// Pin an arbitrary folder (used by the folder right-click menu) to the
    /// user Quick-Access list.
    pub fn pin_path(&mut self, path: PathBuf) {
        if path.is_dir() && !self.user_pins.iter().any(|p| *p == path) {
            self.user_pins.push(path.clone());
            self.store.set_qa_pins(&self.user_pins);
            self.status = format!("{}: {}", crate::lang::tr("已固定到快速访问", self.lang), path.display());
        } else if self.user_pins.iter().any(|p| *p == path) {
            self.status = crate::lang::tr("该目录已在快速访问中", self.lang).into();
        }
    }

    /// Remove a path from the user Quick-Access list (system pins stay).
    pub fn unpin(&mut self, path: &std::path::Path) {
        let before = self.user_pins.len();
        self.user_pins.retain(|p| p.as_path() != path);
        if self.user_pins.len() != before {
            self.store.set_qa_pins(&self.user_pins);
            self.status = crate::lang::tr("已取消固定", self.lang).into();
        }
    }

    // ---------- size worker ----------

    /// Ask the background worker to (re)compute sizes for unknown folders.
    pub fn request_folder_sizes(&mut self) {
        if self.pending_sizes.len() >= 64 {
            return;
        }
        // Index-based loop: borrowing `self.tabs` directly (not through a
        // method) lets the other fields be touched inside the loop.
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            for e in panel.entries.iter().filter(|e| e.is_dir && !e.size_known) {
                let p = panel.cwd.join(&e.name);
                let mtime = e.mtime;
                if self.store.size_for(&p, mtime).is_some() {
                    continue; // cache hit, no request needed
                }
                if self.pending_sizes.insert(p.clone()) {
                    self.size_tx.send(p).ok();
                }
            }
        }
    }

    /// Apply a size result coming back from the worker.
    pub fn apply_size_result(&mut self, path: PathBuf, mtime: u64, size: u64) {
        self.pending_sizes.remove(&path);
        self.store.record_size(path.clone(), mtime, size);
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            for e in panel.entries.iter_mut() {
                if e.is_dir && e.mtime == mtime && panel.cwd.join(&e.name) == path {
                    e.size = size;
                    e.size_known = true;
                }
            }
        }
    }

    /// Fill in cached sizes for the current listings (called after loads).
    pub fn apply_cached_sizes(&mut self) {
        for i in 0..2 {
            let panel = &mut self.tabs[self.active_tab].panes[i];
            for e in panel.entries.iter_mut() {
                if e.is_dir && !e.size_known {
                    let p = panel.cwd.join(&e.name);
                    if let Some(sz) = self.store.size_for(&p, e.mtime) {
                        e.size = sz;
                        e.size_known = true;
                    }
                }
            }
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
            crate::lang::tr(if cut { "已准备移动" } else { "已复制" }, self.lang),
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
        let mut ok = 0usize;
        let mut skip = 0usize;
        let mut fail = 0usize;
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
            if src.is_file() {
                let res: std::io::Result<()> = if is_move {
                    match fs::rename(src, &dest) {
                        Ok(()) => Ok(()),
                        Err(_) => fs::copy(src, &dest).and_then(|_| fs::remove_file(src).map(|_| ())),
                    }
                } else {
                    fs::copy(src, &dest).map(|_| ())
                };
                match res {
                    Ok(_) => ok += 1,
                    Err(_) => fail += 1,
                }
            } else {
                dirs.push((src.clone(), dest));
            }
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
            kind: JobKind::CopyMove,
            label,
            progress: 0.0,
            note: None,
            err: None,
            rx,
        });
    }

    /// Drain job messages; returns a final status line when the job finishes.
    pub fn poll_job(&mut self) -> Option<String> {
        let Some(job) = self.job.as_ref() else { return None };
        let kind = job.kind;
        let Some(job) = self.job.as_mut() else { return None };
        let mut finished: Option<String> = None;
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
            }
            if let Some(err) = &job.err {
                finished = Some(format!("{}: {err}", crate::lang::tr("失败", self.lang)));
            }
        }
        if let Some(line) = finished {
            self.job = None;
            if kind == JobKind::EmptyRecycle {
                // The bin just changed full → empty: drop the cached glyph so
                // the left-rail entry re-queries the shell (and re-reads the
                // listing through the normal reload below).
                self.icons.invalidate(&crate::shellicon::Kind::Recycle);
            }
            self.refresh_both();
            Some(line)
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
        if paths.len() == 1 {
            self.delete_path(paths[0].clone(), false);
        } else {
            self.delete_paths(paths, false);
        }
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

    /// Empty the active recycle-bin view in the background (no UI stall).
    pub fn spawn_empty_recycle(&mut self) {
        let dir = self.active_panel().cwd.clone();
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut files: Vec<PathBuf> = vec![];
            if let Ok(rd) = fs::read_dir(&dir) {
                for e in rd.flatten() {
                    let n = e.file_name().to_string_lossy().into_owned();
                    if n.starts_with("$") {
                        files.push(e.path());
                    }
                }
            }
            let total = files.len().max(1) as f32;
            let mut n = 0f32;
            for p in &files {
                // `$R` payloads of deleted *folders* are directories: a plain
                // remove_file fails on them, so fall back to remove_dir_all
                // (otherwise "empty" silently leaves every deleted folder).
                let _ = fs::remove_file(p).or_else(|_| fs::remove_dir_all(p));
                n += 1.0;
                tx.send(JobMsg::Progress(n / total)).ok();
            }
            tx.send(JobMsg::Done("已清空回收站（删除 %s 个文件）", files.len().to_string())).ok();
        });
        self.job = Some(Job {
            kind: JobKind::EmptyRecycle,
            label: crate::lang::tr("清空回收站…", self.lang).into(),
            progress: 0.0,
            note: None,
            err: None,
            rx,
        });
        self.empty_open = false;
        self.status = crate::lang::tr("正在清空回收站…", self.lang).into();
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

    /// Delete a path, either to the Recycle Bin or permanently.
    pub fn delete_path(&mut self, path: PathBuf, permanent: bool) {
        let res: Result<(), String> = if permanent {
            let is_dir = fs::metadata(&path).map(|m| m.is_dir()).unwrap_or(false);
            let r = if is_dir { fs::remove_dir_all(&path) } else { fs::remove_file(&path) };
            r.map_err(|e| e.to_string())
        } else {
            fs_util::move_to_recycle_bin(&path)
        };
        self.status = match res {
            Ok(_) => format!(
                "{} {}",
                crate::lang::tr(if permanent { "已永久删除" } else { "已移到回收站" }, self.lang),
                path.display()
            ),
            Err(e) => format!("{}: {e}", crate::lang::tr("删除失败", self.lang)),
        };
        // Either way the bin's contents (and therefore its full/empty glyph)
        // just changed.
        self.icons.invalidate(&crate::shellicon::Kind::Recycle);
        self.reload_both();
    }

    /// Delete several paths at once (Delete / Shift+Delete with a multi-
    /// selection, or the context menu acting on the whole selection). One bad
    /// item must not abort the rest, so failures are counted, not thrown.
    pub fn delete_paths(&mut self, paths: Vec<PathBuf>, permanent: bool) {
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
    pub fn open_viewer(&mut self, path: PathBuf) {
        // NB: the image list must come from a live directory read, *not* from
        // `panel.entries`. Double-clicks happen inside `render_list` /
        // `render_grid`, which `mem::take` the listing for the duration of
        // the frame — so `entries` is empty right here and the viewer used to
        // open with a single-image list (no arrows, no filmstrip, ←/→ dead).
        // Reading the directory also makes the list independent of the
        // current sort, filter and view mode: it is always every image in the
        // folder.
        let dir = path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut list: Vec<PathBuf> = Vec::new();
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
            if ui.button(crate::lang::tr("固定到快速访问", self.lang)).clicked() {
                self.pin_path(path.clone());
                ui.close_menu();
            }
        } else if ui.button(crate::lang::tr("打开", self.lang)).clicked() {
            fs_util::open_default_file(&path);
            ui.close_menu();
        }
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
            self.open_viewer(path.clone());
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
        if ui.button(crate::lang::tr("固定到快速访问", self.lang)).clicked() {
            self.set_active_pane(is_left);
            self.pin_current();
            ui.close_menu();
        }
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

/// Recursively sum file sizes under `src`.
pub fn total_size(src: &std::path::Path) -> u64 {
    if src.is_dir() {
        fs::read_dir(src)
            .map(|rd| rd.flatten().map(|e| total_size(&e.path())).sum::<u64>())
            .unwrap_or(0)
    } else {
        fs::metadata(src).map(|m| m.len()).unwrap_or(0)
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
