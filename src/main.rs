//! MarinExplorer: a lightweight Windows two-pane file manager (x86_64 + ARM64), built with Rust + egui.
//!
//! Layout:
//!   - Top toolbar : Back / Forward / Up / Refresh / Copy / Move / Paste /
//!                   Delete / New Folder / Go to / Terminal / Hidden
//!   - Left rail   : Recycle-Bin entry + fixed drives (with free/total) + Home
//!   - Center      : two file panes (independent cwd, filter, sort, history)
//!   - Bottom bar  : status, active path, item count, job progress
//!
//! Folder sizes are computed on a background worker and persisted to disk
//! (`%APPDATA%\rust-explorer\store.txt`), so reopening is instant.
//!
//! Keyboard:
//!   ↑/↓ select · Enter open · Home/End · Delete delete ·
//!   Alt+←/→ back/forward · Escape close dialogs.

// A GUI app: without this, Windows allocates a console window that flashes
// open (and swallows panics) on every launch.
#![windows_subsystem = "windows"]

mod app;
mod cache;
mod convert;
mod explorer;
mod fs_util;
mod gif;
mod icons;
mod lang;
mod preview;
mod video;
mod quickaccess;
mod shell;
mod shellicon;
mod single;
mod tray;
mod dragout;

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use app::{
    Clip, SortKey, View, human_clock, human_datetime, human_size, human_size_est, human_time,
    path_segments,
};
use cache::Store;
use explorer::{Explorer, QaKind, RecInfo, TextColor};

// ---- table geometry (list view) ----
// Defaults for the resizable columns (name/type/size/time). The 创建时间
// column is not listed here: it flexes to fill the leftover width.
const DEF_NAME: f32 = 220.0;
const DEF_TYPE: f32 = 80.0;
const DEF_SIZE: f32 = 92.0;
// A full "YYYY-MM-DD HH:MM" needs ~130px at the body font size.
const DEF_TIME: f32 = 132.0;
// Hard floors so a column can never be dragged away entirely.
const MIN_NAME: f32 = 80.0;
const MIN_TYPE: f32 = 48.0;
const MIN_SIZE: f32 = 56.0;
const MIN_TIME: f32 = 96.0;
const MIN_DATE: f32 = 110.0;
const ROW_H: f32 = 20.0;

// ---- icon geometry (one size per context, always centred) ----
/// Leading icon in file-list rows and left-rail rows.
const LIST_ICON: f32 = 16.0;
/// Icon in the thumbnail grid and the preview pane.
const LARGE_ICON: f32 = 40.0;

/// Column geometry shared by the header, the file rows and the recycle rows.
/// One source of truth, so header text and cell text always start at exactly
/// the same x (a padded `Button` in the header used to indent it a few pixels,
/// which is what made the table look ragged).
#[derive(Clone, Copy)]
struct Cols {
    name: f32,
    ty: f32,
    size: f32,
    time: f32,
    date: f32,
}
// ---- grid geometry (thumbnail view) ----
const CELL_W: f32 = 108.0;
const CELL_H: f32 = 134.0;
/// Max cached grid video thumbnails (LRU; oldest evicted first).
const THUMB_VIDEO_CACHE: usize = 48;
/// Max concurrent grid video-poster decodes (each is a worker thread).
const THUMB_VIDEO_INFLIGHT: usize = 4;
/// Longest side of a playback frame. Full-size 1080p RGBA costs ~8 MB per
/// frame to convert, copy and upload; scaling in the worker keeps playback
/// cheap. Raise only if clips look soft on a large display.
const PLAY_MAX_DIM: u32 = 1024;

/// A background video-poster decode job. `rx` yields exactly one result
/// (`None` = undecodable, e.g. MKV without a third-party decoder).
pub struct VideoJob {
    pub path: std::path::PathBuf,
    pub rx: std::sync::mpsc::Receiver<Option<(u32, u32, Vec<u8>)>>,
    pub kind: VideoJobKind,
}

/// Which surface a poster is decoded for — decides the size cap and the cache
/// the finished texture lands in.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VideoJobKind {
    /// Poster for the open viewer (largest, ~1280 px).
    Viewer,
    /// Poster for the side preview pane (~640 px).
    Side,
    /// Thumbnail for the grid view (~256 px).
    Thumb,
}

fn main() -> eframe::Result {
    // Put the UI thread in an STA apartment + initialize OLE up front, so the
    // in-window OLE file drag (DoDragDrop) runs on an STA thread and never
    // hangs. Must happen before any window / GUI is created.
    crate::dragout::init_com_sta();

    // ---- one-shot elevated actions --------------------------------------
    // Emptying *every* account's Recycle Bin needs admin, so it is launched
    // elevated on demand and exits immediately (see the button handler).
    if std::env::args().any(|a| a == "--empty-recycle") {
        let _ = crate::shell::empty_recycle_bin(None);
        return Ok(());
    }

    // ---- single instance (checked BEFORE elevation) ---------------------
    // A plain double-click while we are already open must only bring the
    // window to the front — never a UAC prompt. `EnumWindows` /
    // `SetForegroundWindow` and the file-drop inbox all work from the
    // unelevated launcher against the elevated instance's window (see
    // `single.rs`), so activation needs no elevation. A replace (`--take-over`)
    // needs elevation to terminate the higher-integrity instance.
    let take_over = std::env::args().any(|a| a == "--take-over");
    let path_arg = std::env::args()
        .skip(1)
        .find(|a| a != "--elevated" && a != "--take-over");
    if let Some(found) = crate::single::find_existing() {
        if take_over || !crate::single::same_build(&found) {
            // Replace the running instance. Both normally run at medium
            // integrity now, so `take_over` terminates it directly — no UAC.
            // Only a leftover *elevated* build survives that; then (and only
            // then) we hop up with `runas` to terminate it.
            crate::single::take_over(&found);
            if crate::single::find_existing().is_some() {
                if !crate::shell::is_elevated() {
                    crate::shell::relaunch_elevated_with(&["--take-over"]);
                }
                return Ok(());
            }
            // The old window is gone: fall through to start a fresh one.
        } else {
            // Same binary already running: hand the path over (if any) and
            // show its window. No UAC, no second process.
            if let Some(p) = &path_arg {
                crate::single::write_inbox(p);
            }
            crate::single::activate(found.hwnd);
            return Ok(());
        }
    }

    // ---- run at normal (medium) integrity -------------------------------
    // Do NOT self-elevate. A medium-integrity process shares the Windows
    // clipboard with Explorer / QQ / browsers, which is what lets Ctrl+C here
    // paste *into* them. An elevated window's clipboard is invisible to those
    // programs (UIPI), which broke cross-app paste. The few actions that truly
    // need admin (empty every account's Recycle Bin, replace an elevated
    // leftover) are launched elevated on demand, see `--empty-recycle` above.

    let store = Store::load();

    // Folder-size worker: one persistent thread, unbounded queue.
    let (size_tx, size_rx) = channel::<(PathBuf, bool)>(); // (path, full_scan)
    let (prog_tx, prog_rx) = channel::<(PathBuf, u64)>(); // full-scan progress
    let (back_tx, back_rx) = channel::<(PathBuf, u64, u64, bool, bool)>(); // …, was_full
    std::thread::spawn(move || {
        while let Ok((path, full)) = size_rx.recv() {
            // Passive scans are budgeted: `complete == false` means the number
            // is a lower bound (`~`). The manual recompute asks for `full`:
            // no budget — an exact number is the whole point of the button —
            // with progress streamed back while it runs.
            let (size, complete) = if full {
                (explorer::scan_dir_size_full(&path, &prog_tx), true)
            } else {
                explorer::scan_dir_size(&path)
            };
            let mtime = match std::fs::metadata(&path) {
                Ok(m) => m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };
            back_tx.send((path, mtime, size, complete, full)).ok();
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 640.0])
            .with_title("MarinExplorer")
            .with_icon(app_icon()),
        ..Default::default()
    };

    // UI 字体：运行时从 Windows 字体目录加载系统自带字体（默认黑体，最小）。
    // 不把字体文件打包进 exe，中文观感也与 Windows 原生 UI 一致。
    let boot_lang = crate::lang::Lang::from_id(&store.meta("lang").unwrap_or_default());
    let ui_font = store.meta("ui_font").unwrap_or(crate::DEFAULT_FONT);
    let fonts = font_definitions(ui_font, boot_lang);

    // The native window handle, grabbed once here: it is what lets us repaint
    // the OS title bar to match the in-app dark theme.
    let mut hwnd: Option<isize> = None;

    eframe::run_native(
        "MarinExplorer",
        options,
        Box::new(move |cc| {
            #[cfg(windows)]
            {
                use raw_window_handle::HasWindowHandle as _;
                if let Ok(h) = cc.window_handle() {
                    if let raw_window_handle::RawWindowHandle::Win32(w) = h.as_raw() {
                        hwnd = Some(w.hwnd.get() as isize);
                    }
                }
            }
            cc.egui_ctx.set_fonts(fonts);
            let mut ex = Explorer::new(store, size_tx, prog_rx, back_rx);
            // A path handed over on the command line (context menu, drag onto
            // the exe, `MarinExplorer.exe D:\foo`) opens that folder — or, for
            // a file, its parent with the file selected.
            // Skip the internal markers: they are flags, not paths to open.
            if let Some(arg) = std::env::args()
                .skip(1)
                .find(|a| a != "--elevated" && a != "--take-over")
            {
                ex.open_arg(&arg);
            }
            ex.hwnd = hwnd;
            ex.apply_frame_theme();
            // Hand the OS window handle to the system tray so it can show /
            // restart this process, then spawn the tray thread once.
            if let Some(h) = hwnd {
                crate::tray::set_main_hwnd(h);
                crate::tray::start();
            }
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(Box::new(ex))
        }),
    )
}

/// Default UI font: the smallest system CJK face. We persist the font **file
/// name** (e.g. "simhei.ttf") as the stable key so the picker can list every
/// font Windows ships, not just a hand-picked pool. [`normalize_font_key`]
/// maps the old short keys (pre-v0.1.11) onto these file names on load.
pub const DEFAULT_FONT: &str = "simhei.ttf";

/// `%WINDIR%\Fonts` (falls back to `%SystemRoot%\Fonts` then `C:\Windows`).
fn fonts_dir() -> std::path::PathBuf {
    let windir = std::env::var("WINDIR")
        .or_else(|_| std::env::var("SystemRoot"))
        .unwrap_or_else(|_| "C:\\Windows".to_owned());
    std::path::PathBuf::from(windir).join("Fonts")
}

/// Map a (possibly legacy) font key to a concrete file name in `%WINDIR%\Fonts`.
/// Pre-v0.1.11 the settings stored short keys like "simhei"/"msyh"; those are
/// migrated here so existing users keep their chosen font.
pub fn normalize_font_key(key: &str) -> String {
    match key {
        "simhei" => "simhei.ttf".to_owned(),
        "deng" => "Deng.ttf".to_owned(),
        "kaiti" => "simkai.ttf".to_owned(),
        "simsun" => "simsun.ttc".to_owned(),
        "msyh" => "msyh.ttc".to_owned(),
        "msyhui" => "msyhui.ttc".to_owned(),
        "yugothic" => "YuGothR.ttc".to_owned(),
        "msgothic" => "msgothic.ttc".to_owned(),
        other => other.to_owned(),
    }
}

/// Build egui font definitions. The chosen font is placed **first** in each
/// family so Latin, digits and CJK all share one face — one baseline, so mixed
/// strings (e.g. "steam分销协议", "共 1.9TB") line up. egui's own Ubuntu
/// (Latin) and NotoEmoji (emoji) stay in the family as final fallbacks.
///
/// When the UI language is Japanese we also append a Japanese-capable system
/// font so kana never render as boxes.
pub fn font_definitions(key: &str, lang: crate::lang::Lang) -> egui::FontDefinitions {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(data) = load_cjk_by_key(key) {
        fonts
            .font_data
            .insert("cjk".to_owned(), std::sync::Arc::new(data));
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .insert(0, "cjk".to_owned());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .insert(0, "cjk".to_owned());
    }
    if lang == crate::lang::Lang::Ja {
        if let Some(data) = load_cjk_by_key("YuGothR.ttc").or_else(|| load_cjk_by_key("msgothic.ttc")) {
            fonts
                .font_data
                .insert("jp".to_owned(), std::sync::Arc::new(data));
            fonts
                .families
                .get_mut(&egui::FontFamily::Proportional)
                .unwrap()
                .push("jp".to_owned());
            fonts
                .families
                .get_mut(&egui::FontFamily::Monospace)
                .unwrap()
                .push("jp".to_owned());
        }
    }
    fonts
}

/// Font file bytes that live for the whole process, handed out as
/// `&'static [u8]`.
///
/// Why the leak: `egui::FontData::from_owned` stores the bytes as
/// `Cow::Owned`, and epaint then **clones the whole file** into `ab_glyph`
/// (`FontVec::try_from_vec_and_index(bytes.clone(), ..)`) while its
/// `FontDefinitions` keeps the original too — so a 19 MB `msyh.ttc` costs
/// ~38 MB of heap for the whole session. `FontData::from_static` instead
/// stores `Cow::Borrowed`, and epaint builds a zero-copy `FontRef` from it,
/// so exactly one copy of the font ever lives in the heap.
///
/// The bytes would stay resident for the process lifetime anyway, so leaking
/// them changes nothing in practice. The cache matters for the font toggle:
/// without it, switching 雅黑 → 省内存 → 雅黑 would leak a fresh copy of every
/// font it touches. Keyed by path, each file is leaked at most once.
fn font_bytes(path: &std::path::Path) -> Option<&'static [u8]> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<std::path::PathBuf, &'static [u8]>>,
    > = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    let mut guard = cache.lock().ok()?;
    if let Some(bytes) = guard.get(path) {
        return Some(*bytes);
    }
    let bytes: &'static [u8] = Box::leak(std::fs::read(path).ok()?.into_boxed_slice());
    guard.insert(path.to_path_buf(), bytes);
    Some(bytes)
}

/// Every usable font file in `%WINDIR%\Fonts`, for the settings picker.
/// Enumerates `.ttf/.ttc/.otf/.otc`, reads each file's name table for the real
/// family name (preferring the UI language, then English), and sorts by that
/// name. Called lazily — only the first time the font dropdown is opened.
pub fn available_fonts(lang: crate::lang::Lang) -> Vec<(String, String)> {
    let dir = fonts_dir();
    let mut out: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_font = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| {
                    let e = e.to_ascii_lowercase();
                    e == "ttf" || e == "ttc" || e == "otf" || e == "otc"
                })
                .unwrap_or(false);
            if !is_font {
                continue;
            }
            let filename = match path.file_name().and_then(|f| f.to_str()) {
                Some(f) => f.to_owned(),
                None => continue,
            };
            let display = font_family_name(&path, lang).unwrap_or_else(|| filename.clone());
            out.push((filename, display));
        }
    }
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

/// Family name from a font file's name table. Prefers the requested language,
/// then English, then any available name. Returns `None` if the file can't be
/// parsed or has no name.
///
/// Name IDs: 1 = family, 16 = typographic family. Windows language IDs:
/// 2052 = Chinese (PRC), 1041 = Japanese, 1033 = English.
fn font_family_name(path: &std::path::Path, lang: crate::lang::Lang) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let want = match lang {
        crate::lang::Lang::Zh | crate::lang::Lang::ZhTw => 2052u16,
        crate::lang::Lang::Ja => 1041u16,
        _ => 1033u16,
    };
    let count = ttf_parser::fonts_in_collection(&bytes).unwrap_or(1);
    let mut fallback: Option<String> = None;
    for idx in 0..count {
        let face = match ttf_parser::Face::parse(&bytes, idx) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for name in face.names() {
            let is_family = name.name_id == 1 || name.name_id == 16;
            if !is_family {
                continue;
            }
            if let Some(s) = name.to_string() {
                if name.language_id == want {
                    return Some(s);
                }
                if name.language_id == 1033 && fallback.is_none() {
                    fallback = Some(s);
                }
            }
        }
    }
    fallback
}

/// Display name for a font key (the file name). Parsed from the font file so it
/// matches what the picker shows; falls back to the file name itself.
pub fn font_display_name(key: &str, lang: crate::lang::Lang) -> String {
    let fname = normalize_font_key(key);
    let path = fonts_dir().join(&fname);
    if let Some(name) = font_family_name(&path, lang) {
        return name;
    }
    fname
}

/// Load a system font by file name (e.g. "simhei.ttf"). Validates with
/// `ttf-parser` first (epaint panics on an unparseable file) and keeps the
/// bytes to a single heap copy via [`font_bytes`]. Face index 0 (the regular
/// face) is used. Returns `None` if the file is missing or broken.
///
/// We deliberately do *not* bundle a font file — that would cost ~8 MB of exe
/// size, and the system font also matches the native Windows look. egui ships
/// no CJK glyphs (only Ubuntu-Light / Hack / NotoEmoji), so without this every
/// Chinese character renders as an empty box.
fn load_cjk_by_key(key: &str) -> Option<egui::FontData> {
    let fname = normalize_font_key(key);
    let path = fonts_dir().join(&fname);
    let bytes = font_bytes(&path)?;
    if ttf_parser::Face::parse(bytes, 0).is_err() {
        return None;
    }
    // `from_static` (not `from_owned`) — see `font_bytes`: one heap copy.
    let mut data = egui::FontData::from_static(bytes);
    data.index = 0;
    eprintln!("[font] CJK: {fname}");
    Some(data)
}

/// Window / taskbar icon, decoded straight from the multi-size `logo.ico`
/// embedded next to the binary. We hand egui the largest entry (256×256); the
/// Windows resource embedded by `build.rs` supplies the remaining sizes so the
/// exe's file icon and the taskbar / title-bar glyph stay crisp at every scale.
fn app_icon() -> egui::IconData {
    match image::load_from_memory_with_format(
        include_bytes!("../logo.ico"),
        image::ImageFormat::Ico,
    ) {
        Ok(img) => {
            let rgba = img.to_rgba8();
            let (w, h) = rgba.dimensions();
            egui::IconData {
                rgba: rgba.into_raw(),
                width: w,
                height: h,
            }
        }
        Err(_) => egui::IconData {
            rgba: Vec::new(),
            width: 0,
            height: 0,
        },
    }
}

/// An icon button that opens a popup menu below it (mirrors `Ui::menu_button`
/// for our vector-glyph buttons).
/// Width of a single line of `text` in the current UI's body font. Used to
/// decide whether a right-aligned block still fits in the toolbar.
fn galley_w(ui: &egui::Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    ui.painter()
        .layout_no_wrap(text.to_string(), font_id, egui::Color32::PLACEHOLDER)
        .size()
        .x
}

fn icon_menu<R>(
    ui: &mut egui::Ui,
    ic: &icons::Icon,
    label: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> Option<R> {
    let id = ui.id().with(("icon_menu", ic.0));
    let r = icons::button(ui, ic, label, true, "");
    if r.clicked() {
        ui.memory_mut(|m| m.toggle_popup(id));
    }
    egui::popup::popup_below_widget(
        ui,
        id,
        &r,
        egui::PopupCloseBehavior::CloseOnClickOutside,
        body,
    )
}

impl eframe::App for Explorer {
    /// Called by eframe when the window closes (X button, tray quit, …).
    /// Without this the size cache and any setting changed in the last few
    /// seconds would be lost, because `maybe_flush` only writes every 5s.
    fn save(&mut self, _storage: &mut dyn eframe::Storage) {
        self.flush_now();
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Run a deferred OLE file drag requested during the last frame's
        // interaction. Must run HERE (top of update, outside any widget
        // callback): `DoDragDrop` opens a modal loop and blocks until the mouse
        // is released, so it must never re-enter the UI state.
        if let Some(paths) = self.pending_os_drag.take() {
            crate::dragout::begin_file_drag(&paths);
            return;
        }
        // Light / dark theme. Only re-applied when `dark` changes — rebuilding
        // the whole `Visuals` every frame was pure waste (the toggle is still
        // instant because `set_dark` flips `self.dark` and the next frame
        // re-applies).
        if self.applied_dark != Some(self.dark) {
            let mut visuals = if self.dark {
                egui::Visuals::dark()
            } else {
                egui::Visuals::light()
            };
            if self.dark {
                // Custom main text colour for the active theme. `override_text_color`
                // drives the primary body/label colour across every widget.
                visuals.override_text_color = Some(self.text_color());
                // Brighten links so they stay visible on the dark panels.
                visuals.hyperlink_color = egui::Color32::from_rgb(120, 180, 255);
            } else {
                visuals.override_text_color = Some(self.text_color());
            }
            ctx.set_visuals(visuals);
            self.applied_dark = Some(self.dark);
        }
        // Font-size setting: only re-applied when the scale changes (and reset
        // to the default style when it returns to 1.0, which the old code
        // never did).
        if self.font_scale != self.applied_font_scale {
            if self.font_scale == 1.0 {
                // Reset font sizes to the defaults WITHOUT wiping the custom
                // visuals — `Style::default()` would discard `override_text_color`
                // and revert the configured text colour to egui's stock grey.
                let mut style = (*ctx.style()).clone();
                let base = egui::Style::default().text_styles;
                for (key, base_font) in base.iter() {
                    if let Some(f) = style.text_styles.get_mut(key) {
                        f.size = base_font.size;
                    }
                }
                ctx.set_style(style);
            } else {
                let mut style = (*ctx.style()).clone();
                let base = egui::Style::default().text_styles;
                for (key, base_font) in base.iter() {
                    if let Some(f) = style.text_styles.get_mut(key) {
                        f.size = base_font.size * self.font_scale;
                    }
                }
                ctx.set_style(style);
            }
            self.applied_font_scale = self.font_scale;
        }
        // 次要文字（面板信息栏的"N 项"计数、网格缩略图下方的大小、左栏分组
        // 小标题）一律走 `TextStyle::Body`，与左栏条目主文字同字号，不再用
        // 极小的 `Small`，保证清晰、全界面一致。
        // The UI font (or language) changed: rebuild the font definitions here,
        // where the egui context exists. Dropping the old set frees the font
        // file (both copies of it).
        if self.reload_font {
            self.reload_font = false;
            ctx.set_fonts(crate::font_definitions(&self.ui_font, self.lang));
        }
        self.text_focus = false;
        // Each frame starts fresh: set when the address editor consumes Enter.
        self.enter_consumed = false;
        // ----- live key echo (bottom-left) -------------------------------
        // Mirror the chord the user just pressed: bare Ctrl / Shift / Alt as
        // soon as the modifier goes down, then the full chord once a second
        // key lands (Ctrl+A, Ctrl+C, Del, Enter, …). Purely a visual ack.
        {
            let (prev_c, prev_s, prev_a) = (self.prev_ctrl, self.prev_shift, self.prev_alt);
            let mut echo: Option<String> = None;
            ctx.input(|i| {
                let m = i.modifiers;
                let mut last: Option<String> = None;
                for ev in &i.events {
                    match ev {
                        egui::Event::Key { key, pressed: true, .. } => {
                            last = Some(key_name(*key))
                        }
                        // egui-winit rewrites Ctrl+C/X/V into clipboard
                        // commands and never emits the Key event, so rebuild
                        // the label from the command itself.
                        egui::Event::Copy => last = Some("C".into()),
                        egui::Event::Cut => last = Some("X".into()),
                        egui::Event::Paste(_) => last = Some("V".into()),
                        _ => {}
                    }
                }
                let mut s = String::new();
                if m.ctrl { s.push_str("Ctrl+"); }
                if m.shift { s.push_str("Shift+"); }
                if m.alt { s.push_str("Alt+"); }
                if let Some(k) = last {
                    s.push_str(&k);
                    echo = Some(s);
                } else if (m.ctrl && !prev_c) || (m.shift && !prev_s) || (m.alt && !prev_a) {
                    // Only modifiers down so far: drop the trailing '+'.
                    while s.ends_with('+') { s.pop(); }
                    echo = Some(s);
                }
            });
            if let Some(s) = echo {
                self.key_echo = s;
            }
            let (c, s, a) =
                ctx.input(|i| (i.modifiers.ctrl, i.modifiers.shift, i.modifiers.alt));
            self.prev_ctrl = c;
            self.prev_shift = s;
            self.prev_alt = a;
        }
        // ----- video poster jobs / playback pacing -----
        // Poster jobs and playback advance on background threads, so the UI
        // must keep painting while any is in flight (bounded: only while
        // something is actually pending, never an unconditional repaint).
        self.poll_video_jobs(ctx);
        let perf_ms = self.perf_repaint_ms();
        if !self.video_jobs.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(perf_ms));
        }
        if self.video_playing {
            ctx.request_repaint_after(std::time::Duration::from_millis(perf_ms));
        }
        // ----- drain background channels -----
        let mut sizes_arrived = false;
        // Full-scan progress first: draining it before the results means the
        // last progress message (the final byte count) is still on screen when
        // the result that ends the scan lands in the same frame.
        while let Ok((root, total)) = self.size_prog_rx.try_recv() {
            // Progress only belongs to a live batch. A straggler message can
            // land a frame *after* the batch's last result — the two travel on
            // separate channels — and would otherwise resurrect the finished
            // "正在重新计算大小" line and leave it there forever.
            if self.size_scan_started.is_none() {
                continue;
            }
            let label = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.to_string_lossy().into_owned());
            self.size_scan = Some(explorer::SizeScanUi::Running(label, total));
        }
        while let Ok((p, m, s, complete, was_full)) = self.size_rx.try_recv() {
            if was_full {
                self.size_full_left = self.size_full_left.saturating_sub(1);
                // Volume covered by the manual batch, for the finish line.
                self.size_full_bytes = self.size_full_bytes.saturating_add(s);
            }
            self.apply_size_result(p, m, s, complete);
            sizes_arrived = true;
        }
        if sizes_arrived {
            self.resort_size_sorted_panels();
        }
        // The manual recompute batch just finished: the box keeps its place
        // and switches to the finish line ("计算完成 · 耗时 X"). The status
        // line is deliberately NOT written — it is the operation log, and a
        // minutes-long size scan must not overwrite it. `pending_sizes` must
        // be empty too: the last full result and the last passive straggler
        // can land in either order, and the finish line waits for both.
        if self.size_full_left == 0 && self.pending_sizes.is_empty() {
            if let Some(t0) = self.size_scan_started.take() {
                let took = crate::app::human_duration(t0.elapsed(), self.lang);
                let line = format!(
                    "{} · {} · {}",
                    crate::lang::tr("大小计算完成", self.lang),
                    crate::lang::tr("耗时 %s", self.lang).replace("%s", &took),
                    crate::lang::tr("已扫 %s", self.lang)
                        .replace("%s", &crate::app::human_size(self.size_full_bytes))
                );
                self.size_scan = Some(explorer::SizeScanUi::Done(line));
            }
        }
        // One flag for "a background result landed this frame": it has to be
        // raised outside the borrows above, which hold `&self.drives_rx` etc.
        let mut bg = sizes_arrived;
        if let Some(rx) = &self.drives_rx {
            while let Ok(d) = rx.try_recv() {
                self.drives = d;
                self.drives_at = Some(Instant::now());
                bg = true;
            }
        }
        if let Some(rx) = &self.sid_rx {
            while let Ok(s) = rx.try_recv() {
                if let Some(sid) = s {
                    self.sid = Some(sid.clone());
                    self.store.set_meta("sid", &sid);
                    bg = true;
                }
            }
        }
        if bg {
            self.mark_bg_activity();
        }
        if self.drives.is_empty() {
            self.refresh_drives();
        }
        if self.sid.is_none() {
            self.refresh_sid();
        }
        // Recycle-bin listings are read on worker threads: kick off any that
        // are still loading, then adopt whatever finished.
        self.request_recycle_listings();
        self.poll_recycle();
        // Same dance for the normal directory listings.
        self.poll_listings();
        self.refresh_quick_access();
        self.poll_quick_access();
        if let Some(line) = self.poll_job() {
            self.status = line;
        }
        // Throttled (once a second), not per frame: see the comment there.
        self.refresh_folder_sizes_throttled();
        // Paths a second launch handed over (single instance): throttled to
        // 400ms, each opens its own tab.
        self.poll_inbox_throttled();
        self.maybe_flush();
        // 小 perf mode: no new grid thumbnail decodes (cached ones still show).
        self.previews.set_allow_grid(self.viewer_perf != 0);
        self.previews.poll(ctx);
        // A decode lands on a background thread and produces no input event,
        // so without this the freshly decoded picture would sit in the cache
        // until the user happened to move the mouse.
        if self.previews.busy() {
            ctx.request_repaint_after(std::time::Duration::from_millis(perf_ms));
        }

        // ----- top toolbar -----
        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            let (can_back, can_fwd) = {
                let p = self.active_panel();
                (p.can_back(), p.can_forward())
            };
            let lang = self.lang;
            // One translator for the whole toolbar: binding `lang` by value
            // keeps `self` free for the `&mut self` calls below (a closure
            // borrowing `self` would clash with them).
            let t = move |k: &'static str| crate::lang::tr(k, lang);
            // Wraps to a second row on a narrow window instead of pushing the
            // last buttons off-screen.
            ui.horizontal_wrapped(|ui| {
                let pad = ui.spacing().button_padding;
                ui.spacing_mut().button_padding = egui::vec2(6.0, pad.y);
                ui.spacing_mut().item_spacing.x = 3.0;

                // Top-left settings: theme + language. A dropdown keeps both
                // out of the main toolbar so the chrome stays uncluttered.
                icon_menu(ui, &icons::SLIDERS, &t("设置"), |ui| {
                    if ui.checkbox(&mut self.dark, t("深色模式")).changed() {
                        self.set_dark(self.dark);
                    }
                    ui.horizontal(|ui| {
                        ui.label(t("字体大小"));
                        let fs_label = format!("{:.2}×", self.font_scale);
                        if ui
                            .add(
                                egui::Slider::new(&mut self.font_scale, 0.8..=2.0)
                                    .step_by(0.05)
                                    .text(fs_label),
                            )
                            .changed()
                        {
                            self.set_font_scale(self.font_scale);
                        }
                    });
                    ui.separator();
                    ui.label(t("字体颜色"));
                    ui.collapsing(t("深色模式"), |ui| {
                        ui.horizontal(|ui| {
                            ui.label(t("主文字"));
                            if ui.color_edit_button_srgba(&mut self.text_dark).changed() {
                                self.set_text_color(TextColor::TextDark, self.text_dark);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(t("次要文字"));
                            if ui.color_edit_button_srgba(&mut self.weak_dark).changed() {
                                self.set_text_color(TextColor::WeakDark, self.weak_dark);
                            }
                        });
                    });
                    ui.collapsing(t("浅色模式"), |ui| {
                        ui.horizontal(|ui| {
                            ui.label(t("主文字"));
                            if ui.color_edit_button_srgba(&mut self.text_light).changed() {
                                self.set_text_color(TextColor::TextLight, self.text_light);
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(t("次要文字"));
                            if ui.color_edit_button_srgba(&mut self.weak_light).changed() {
                                self.set_text_color(TextColor::WeakLight, self.weak_light);
                            }
                        });
                    });
                    ui.separator();
                    ui.label(t("集成"));
                    // A real on/off switch: writes HKCU\Software\Classes, which
                    // needs no administrator rights and never prompts UAC.
                    if ui
                        .checkbox(&mut self.ctx_menu, t("通过 MarinExplorer 打开（右键菜单）"))
                        .on_hover_text(t("在文件、文件夹、磁盘和文件夹空白处右键菜单里显示此项；只对当前用户生效，不需要管理员权限"))
                        .clicked()
                    {
                        self.set_ctx_menu(self.ctx_menu);
                    }
                    ui.separator();
                    // Icon source. The two lighter modes never ask the shell,
                    // which keeps its namespace/COM stack (and one texture per
                    // file type) out of the process entirely.
                    ui.label(t("图标"));
                    ui.horizontal_wrapped(|ui| {
                        for (id, name) in [
                            (0u8, "系统图标"),
                            (1u8, "矢量图标"),
                            (2u8, "Emoji"),
                        ] {
                            if ui.selectable_label(self.icon_mode == id, name).clicked()
                                && self.icon_mode != id
                            {
                                self.set_icon_mode(id);
                            }
                        }
                    });
                    ui.label(t("字体"));
                    // Custom dropdown: the candidate list is only scanned the
                    // first time it is opened (`avail_fonts` starts as `None`),
                    // then cached, so we never touch %WINDIR%\Fonts at startup
                    // or on every settings frame.
                    let cur = crate::font_display_name(&self.ui_font, self.lang);
                    if ui
                        .selectable_label(self.font_picker_open, format!("▾ {cur}"))
                        .clicked()
                    {
                        self.font_picker_open = !self.font_picker_open;
                    }
                    if self.font_picker_open {
                        // Populate the lazy cache up front, then drop the
                        // mutable borrow so `set_ui_font` below can run.
                        if self.avail_fonts.is_none() {
                            self.avail_fonts = Some(
                                crate::available_fonts(self.lang)
                                    .into_iter()
                                    .map(|(k, d)| (k.to_owned(), d.to_owned()))
                                    .collect(),
                            );
                        }
                        // Clone the small list into a local so no borrow of
                        // `self` is held while the loop calls `set_ui_font`.
                        let items = self.avail_fonts.clone().unwrap_or_default();
                        ui.indent("font_list", |ui| {
                            for (k, d) in items.iter() {
                                if ui
                                    .selectable_label(
                                        self.ui_font.as_str() == k.as_str(),
                                        d,
                                    )
                                    .clicked()
                                    && self.ui_font.as_str() != k.as_str()
                                {
                                    self.set_ui_font(k.to_owned());
                                    self.font_picker_open = false;
                                }
                            }
                        });
                    }
                    ui.label(t("选择界面字体；默认黑体体积最小，中英文混排已对齐"));
                    ui.separator();
                    ui.label(t("语言"));
                    // Plain buttons, one per language — a ComboBox nested
                    // inside this popup swallowed clicks (and could close the
                    // whole window), so the choice is now a direct hit target.
                    ui.horizontal_wrapped(|ui| {
                        for (l, name) in crate::lang::Lang::names() {
                            if ui
                                .selectable_label(self.lang == l, name)
                                .clicked()
                                && self.lang != l
                            {
                                self.set_lang(l);
                            }
                        }
                    });
                    ui.separator();
                    ui.label(t("性能"));
                    // Direct hit targets (like the language row) — a ComboBox
                    // nested here swallowed clicks. This is the *global* CPU
                    // dial: it scales grid thumbnail decoding, folder-size
                    // scanning, side-pane poster decoding and the main window's
                    // repaint cadence — the viewer's playback pacing too.
                    ui.horizontal_wrapped(|ui| {
                        let opts: [(u8, &str); 3] = [
                            (0, &t("小（最低开销）")),
                            (1, &t("中（均衡）")),
                            (2, &t("大（最流畅）")),
                        ];
                        for (v, name) in opts {
                            if ui
                                .selectable_label(self.viewer_perf == v, name)
                                .clicked()
                                && self.viewer_perf != v
                            {
                                self.viewer_perf = v;
                                self.persist_viewer_opts();
                            }
                        }
                    });
                    ui.label(
                        egui::RichText::new(t("全局：缩略图解码 · 文件夹大小扫描 · 重绘频率"))
                            .small()
                            .weak(),
                    );
                });
                if icons::button(ui, &icons::BACK, &t("后退"), can_back, "").clicked() {
                    self.active_panel().back();
                    self.apply_cached_sizes();
                }
                if icons::button(ui, &icons::FORWARD, &t("前进"), can_fwd, "").clicked() {
                    self.active_panel().forward();
                    self.apply_cached_sizes();
                }
                if icons::button(ui, &icons::UP, &t("上级"), true, "").clicked() {
                    self.active_panel().up();
                    self.apply_cached_sizes();
                }
                if icons::button(ui, &icons::REFRESH, &t("刷新"), true, "").clicked() {
                    let cwd = self.active_panel().cwd.clone();
                    self.active_panel().load(&cwd);
                    self.apply_cached_sizes();
                    self.refresh_drives();
                }
                // Manual override for the size cache: 刷新 re-reads the listing
                // but happily reuses a cached (possibly stale or `~`) size.
                if icons::button(
                    ui,
                    &icons::SIZE,
                    &t("重新计算大小"),
                    true,
                    &t("忽略缓存，重新扫描面板里所有文件夹的大小"),
                )
                .clicked()
                {
                    self.recompute_sizes();
                }
                ui.separator();
                if icons::button(ui, &icons::COPY, &t("复制"), true, "").clicked() { self.do_copy(); }
                if icons::button(ui, &icons::CUT, &t("剪贴"), true, "").clicked() { self.do_move(); }
                if icons::button(ui, &icons::PASTE, &t("粘贴"), true, "").clicked() { self.do_paste(); }
                if icons::button(ui, &icons::TRASH, &t("删除"), true, "").clicked() { self.do_delete(); }
                ui.separator();
                if icons::button(ui, &icons::FOLDER_PLUS, &t("新建"), true, "").clicked() { self.new_folder_open = true; }
                if icons::button(ui, &icons::PLUS, &t("标签页"), true, "Ctrl+T").clicked() { self.new_tab(); }
                if icons::button(ui, &icons::HEART, &t("收藏"), true, &t("把当前目录加入左侧“收藏”")).clicked() { self.favorite_current(); }
                if icons::button(ui, &icons::NAV, &t("跳转"), true, "").clicked() {
                    self.go_path = self.active_panel().cwd.to_string_lossy().into_owned();
                    self.go_open = true;
                }
                if icons::button(ui, &icons::TERMINAL, &t("终端"), true, &t("在当前目录打开终端窗口")).clicked() {
                    let cwd = self.active_panel().cwd.clone();
                    fs_util::open_terminal(&cwd);
                }
                ui.separator();
                ui.checkbox(&mut self.show_hidden, t("隐藏文件"));
                let mut prev = self.show_preview;
                if ui
                    .checkbox(&mut prev, t("预览窗格"))
                    .on_hover_text(t("在右侧显示所选文件的图片预览"))
                    .changed()
                {
                    self.show_preview = prev;
                    self.persist_layout();
                }
                // 分栏：persisted, defaults to off (single pane).
                let mut sp = self.split;
                if ui
                    .checkbox(&mut sp, t("分栏"))
                    .on_hover_text(t("两个文件栏并排（记忆）"))
                    .changed()
                {
                    self.set_split(sp);
                    self.status = if sp { t("已开启分栏") } else { t("已关闭分栏") }.into();
                }
                let in_rec = self.active_panel().in_recycle;
                if in_rec && self.job.is_none() {
                    if icons::button(ui, &icons::TRASH, &t("清空回收站"), true, "").clicked() { self.empty_open = true; }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // The right-hand block only gets what is left of the
                    // toolbar. On a narrow window there is not always enough
                    // room: without this guard the block drew on top of the
                    // left-flowing items ("栏: 单" landed inside 预览窗格).
                    // The status label goes first (right_to_left), the exit
                    // button second, and both are dropped as soon as they
                    // cannot fit.
                    let status = format!(
                        "{} {}",
                        t("栏:"),
                        if self.split {
                            if self.active_pane == 0 { t("左") } else { t("右") }
                        } else {
                            t("单")
                        }
                    );
                    let w = ui.spacing().item_spacing.x + 60.0
                        + ui.spacing().button_padding.x * 2.0
                        + galley_w(ui, &status);
                    if ui.available_width() > w {
                        ui.label(status);
                        if icons::button(ui, &icons::POWER, &t("退出"), true, "").clicked() {
                            self.flush_now();
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    }
                });
            });
        });

        // ----- bottom status bar -----
        egui::TopBottomPanel::bottom("statusbar").show(ctx, |ui| {
            ui.horizontal(|ui| {
                // Bottom-left: echo the key chord just pressed (Ctrl+A, Del, …).
                if !self.key_echo.is_empty() {
                    ui.label(
                        egui::RichText::new(format!("[{}]", self.key_echo)).strong(),
                    );
                    ui.separator();
                }
                ui.strong(&self.status);
                // The folder-size box. It owns its own slot so that neither
                // the running line nor the finish line ever lands in the
                // operation log on the far left. The finish line stays until
                // the next manual recompute — "计算完成 · 耗时 X" is the only
                // warning the user gets that the exact numbers cost real time.
                match &self.size_scan {
                    Some(explorer::SizeScanUi::Running(label, bytes)) => {
                        ui.separator();
                        // Animated bar: no total is known up front, so this is
                        // bytes-counted, never a percentage.
                        ui.add(egui::ProgressBar::new(f32::NAN).desired_width(48.0));
                        ui.label(format!(
                            "{}：{}… 已扫 {}",
                            crate::lang::tr("正在重新计算大小", self.lang),
                            label,
                            crate::app::human_size(*bytes)
                        ));
                    }
                    Some(explorer::SizeScanUi::Done(line)) => {
                        ui.separator();
                        // No bar: a static full bar reads as "still working"
                        // (and a moving one lies). The line itself is the
                        // result — time taken plus the volume scanned.
                        ui.label(line);
                    }
                    None => {}
                }
                // Every other CPU-heavy background task, bottom-left: a
                // copy/delete or a big directory read is just as invisible
                // without this line.
                let tasks = self.running_tasks();
                if !tasks.is_empty() {
                    ui.separator();
                    // Small indeterminate bar: motion for the tasks that have
                    // no numbers of their own (directory reads).
                    ui.add(egui::ProgressBar::new(f32::NAN).desired_width(48.0));
                    ui.label(tasks.join(" · "));
                }
                ui.separator();
                let cwd = self.active_panel().cwd.clone();
                let count = {
                    let p = self.active_panel();
                    if p.in_recycle { p.recycle_items.len() } else { p.entries.len() }
                };
                ui.label(format!("{} · {} {}", cwd.display(), count, crate::lang::tr("项", self.lang)));
                let sel = self.active_panel().selected.clone();
                if let Some(s) = &sel {
                    ui.label(format!("{} {s}", crate::lang::tr("选中:", self.lang)));
                }
                if let Some(job) = &self.job {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if job.indeterminate {
                            // Animated, percentage-free bar: the shell does the
                            // real work and only reports at the end, so a NaN
                            // fill keeps the UI alive without a system dialog.
                            ui.add(egui::ProgressBar::new(f32::NAN).text(job.label.clone()));
                        } else {
                            ui.add(
                                egui::ProgressBar::new(job.progress.clamp(0.0, 1.0))
                                    .text(format!(
                                        "{} 已删除 {} · {:.0}%",
                                        job.label, job.deleted, job.progress * 100.0
                                    )),
                            );
                        }
                    });
                }
            });
        });

        // ----- left rail: recycle bin + drives + home -----
        egui::SidePanel::left("drives")
            .default_width(200.0)
            .width_range(150.0..=360.0)
            .show(ctx, |ui| {
            let lang = self.lang;
            let t = move |k: &'static str| crate::lang::tr(k, lang);
            let row_w = ui.available_width();
            // Everything in the rail scrolls: with 20+ Quick-Access entries and
            // several drives it is taller than the window.
            egui::ScrollArea::vertical()
                .id_salt("rail_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
            let rec = self.recycle_target();
            let rec_glyph = match self.icons.get(ctx, shellicon::Kind::Recycle) {
                Some(id) => icons::Glyph::Tex(id),
                None => icons::Glyph::Svg(&icons::TRASH),
            };
            if icons::entry(ui, rec_glyph, &t("回收站"), false, "", row_w).clicked() {
                if let Some(target) = rec {
                    self.active_panel().navigate(target);
                    self.apply_cached_sizes();
                }
            }
            ui.separator();
            icons::label(ui, &icons::DRIVE, &t("驱动器"));
            let drives = self.drives.clone();
            let texs: Vec<Option<egui::TextureId>> = drives
                .iter()
                .map(|d| {
                    self.icons.get(
                        ctx,
                        shellicon::Kind::Path(PathBuf::from(format!("{}:\\", d.letter))),
                    )
                })
                .collect();
            for (i, d) in drives.iter().enumerate() {
                let root = PathBuf::from(format!("{}:\\", d.letter));
                let cur = self.active_panel().cwd.starts_with(&root)
                    && self.active_panel().cwd.components().count() <= 1;
                let glyph = match texs[i] {
                    Some(id) => icons::Glyph::Tex(id),
                    None => icons::Glyph::Svg(&icons::DRIVE),
                };
                let text = if d.total == 0 {
                    format!("{}:  {}", d.letter, t("容量未知"))
                } else {
                    format!("{}:  {} {}%", d.letter, t("已用"), (d.used_frac() * 100.0) as u32)
                };
                if icons::entry(ui, glyph, &text, cur, "", row_w).clicked() {
                    self.active_panel().navigate(root);
                    self.apply_cached_sizes();
                }
                if d.total != 0 {
                    ui.add_space(3.0);
                    ui.add(
                        egui::ProgressBar::new(d.used_frac())
                            .desired_height(24.0)
                            .corner_radius(egui::CornerRadius::same(8))
                            .text(format!(
                                "{}{}/{}{}",
                                t("共"),
                                human_size(d.total),
                                t("剩"),
                                human_size(d.free)
                            )),
                    );
                    ui.add_space(6.0);
                }
            }
            ui.separator();

            // Quick Access, split the way Windows Explorer splits its own:
            // the folders the user pinned under "已固定", Windows' jump-list
            // entries under "最近使用". Sits below the drives on purpose (the
            // most-used rail entries stay within reach without scrolling).
            icons::label(ui, &icons::STAR, &t("快速访问"));
            let qa = self.quick_access_list();
            {
                // Three groups: Marin's own favourites, then the two kinds
                // Windows hands us. "收藏" is always drawn — it is the group
                // the user fills themselves, so an empty one shows a hint
                // instead of vanishing. The Windows groups are skipped when
                // Explorer has nothing to offer.
                for (heading, ic, kind, always) in [
                    (t("收藏"), &icons::HEART, QaKind::Favorite, true),
                    (t("已固定"), &icons::PIN, QaKind::WinPin, false),
                    (t("最近使用"), &icons::CLOCK, QaKind::Recent, false),
                ] {
                    let paths: Vec<PathBuf> = qa
                        .iter()
                        .filter(|(_, k)| *k == kind)
                        .map(|(p, _)| p.clone())
                        .collect();
                    if paths.is_empty() && !always {
                        continue;
                    }
                    icons::sub_label(ui, ic, &heading, self.weak_color());
                    if paths.is_empty() {
                        weak(ui, t("暂无；右键或工具栏“收藏”添加"));
                        continue;
                    }
                    // Resolve the native icons up front: the borrow of
                    // `self.icons` must not overlap the `&mut self` row calls.
                    // Use `Kind::Path` (the same call the old "常用位置" section
                    // and the drive/home rows use) so known folders such as
                    // Desktop / Downloads / Documents get their *real* shell
                    // glyph instead of the generic folder icon — the pinned
                    // group now matches 常用位置 exactly, after 常用位置 itself
                    // was removed.
                    let texs: Vec<Option<egui::TextureId>> = paths
                        .iter()
                        .map(|p| self.icons.get(ctx, shellicon::Kind::Path(p.clone())))
                        .collect();
                    for (i, path) in paths.iter().enumerate() {
                        let glyph = match texs[i] {
                            Some(id) => icons::Glyph::Tex(id),
                            None => icons::Glyph::Svg(if path.is_dir() {
                                &icons::FOLDER
                            } else {
                                &icons::FILE
                            }),
                        };
                        self.qa_row(ui, path, glyph, row_w, kind);
                    }
                }
            }
            ui.separator();
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."));
            let home_glyph = match self.icons.get(ctx, shellicon::Kind::Path(home.clone())) {
                Some(id) => icons::Glyph::Tex(id),
                None => icons::Glyph::Svg(&icons::HOME),
            };
            if icons::entry(
                ui,
                home_glyph,
                &t("用户目录"),
                self.active_panel().cwd == home,
                &home.to_string_lossy(),
                row_w,
            )
            .clicked()
            {
                self.active_panel().navigate(home);
                self.apply_cached_sizes();
            }
                });
        });

        // ----- right rail: preview pane (optional) -----
        if self.show_preview {
            egui::SidePanel::right("preview_pane")
                .default_width(260.0)
                .width_range(180.0..=560.0)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        crate::icons::label(ui, &crate::icons::EYE, &crate::lang::tr("预览", self.lang));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::icons::small_button(ui, &crate::icons::X, &crate::lang::tr("关闭预览窗格", self.lang)).clicked() {
                                self.show_preview = false;
                                self.persist_layout();
                            }
                        });
                    });
                    ui.separator();
                    self.render_preview_pane(ui);
                });
        }

        // ----- center: tab strip + one or two file panes -----
        egui::CentralPanel::default().show(ctx, |ui| {
            self.render_tab_strip(ui);

            let avail = ui.available_size_before_wrap();
            let h = avail.y;
            if self.split {
                // NB: the two panes are laid out by hand instead of inside a
                // `ui.horizontal`. `set_min_size(w)` children plus a
                // `ui.separator()` add up to more than the available width
                // (item_spacing on both sides of the separator, the line
                // itself), so the right pane was pushed past the central
                // panel's right edge — onto the preview SidePanel — and its
                // clip rect (built from the already-shifted cursor) hid the
                // overflow. Exact rects + one 1px divider line have no such
                // slack.
                let sep = 1.0f32;
                let span = (avail.x - sep).max(1.0);
                let min_w = 60.0f32;
                let frac = self.split_frac.clamp(min_w / span, 1.0 - min_w / span);
                let w = span * frac;
                let top = ui.cursor().min;
                let r1 = egui::Rect::from_min_size(top, egui::vec2(w, h));
                let r2 = egui::Rect::from_min_size(
                    egui::pos2(top.x + w + sep, top.y),
                    egui::vec2(span - w, h),
                );
                // Reserve the full strip so the cursor advances past both
                // panes (the children below paint, they do not lay out).
                let (_r, _) = ui.allocate_exact_size(egui::vec2(avail.x, h), egui::Sense::hover());
                // NB: must use a *vertical* child here. `ui.scope` would inherit
                // this horizontal layout, which makes every top-level widget in
                // the panel (breadcrumb, filter, and the file list) flow into a
                // single row — the "all files on one line / garbled" bug.
                //
                // Each pane also gets its own clip rectangle and its own id
                // salt: a child Ui otherwise inherits the central panel's clip
                // rect (over-wide content then bleeds into the neighbour) and
                // its parent's id (every widget in the two panes would collide).
                for (rect, is_left) in [(r1, true), (r2, false)] {
                    let mut child = ui.new_child(
                        egui::UiBuilder::new()
                            .id_salt(if is_left { "pane_l" } else { "pane_r" })
                            .max_rect(rect)
                            .layout(egui::Layout::top_down(egui::Align::Min)),
                    );
                    child.set_clip_rect(rect);
                    self.render_panel(&mut child, is_left);
                }
                // The divider: a 1px line plus an invisible 7px grip over it.
                // Registered *after* the panes on purpose — egui breaks
                // pointer-hit ties by registration order ("in tie, pick
                // last = topmost"), so the grip wins against the rows that
                // reach into its edges and can be grabbed anywhere on it.
                let grip = egui::Rect::from_center_size(
                    egui::pos2(r1.max.x + sep * 0.5, r1.center().y),
                    egui::vec2(7.0, h),
                );
                let g = ui
                    .interact(
                        grip,
                        egui::Id::new("split_grip"),
                        egui::Sense::click_and_drag(),
                    )
                    .on_hover_cursor(egui::CursorIcon::ResizeHorizontal)
                    .on_hover_text(crate::lang::tr("拖动调整栏宽；双击恢复等宽", self.lang));
                let line =
                    egui::Rect::from_min_size(egui::pos2(r1.max.x, r1.min.y), egui::vec2(sep, h));
                let line_color = if g.dragged() {
                    ui.visuals().selection.bg_fill
                } else if g.hovered() {
                    ui.visuals().widgets.hovered.bg_stroke.color
                } else {
                    ui.visuals().widgets.noninteractive.bg_stroke.color
                };
                ui.painter()
                    .rect_filled(line, egui::CornerRadius::ZERO, line_color);
                if g.double_clicked() {
                    self.split_frac = 0.5;
                    self.persist_layout();
                } else if g.dragged() {
                    if let Some(pos) = g.interact_pointer_pos() {
                        let f =
                            ((pos.x - top.x) / span).clamp(min_w / span, 1.0 - min_w / span);
                        if (f - self.split_frac).abs() > 1e-4 {
                            self.split_frac = f;
                        }
                    }
                }
                // Persist on release, not every drag frame (same rule as the
                // column widths).
                if g.drag_stopped() {
                    self.persist_layout();
                }
            } else {
                // NB: same vertical-child rule as above.
                let pane = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(avail.x, h));
                ui.vertical(|ui| {
                    ui.set_min_size(egui::vec2(avail.x, h));
                    ui.set_clip_rect(pane);
                    self.render_panel(ui, true);
                });
            }
        });

        // ----- dialogs -----
        if self.delete_open {
            let msg = if let Some(suffix) = self.delete_rec_suffix.clone() {
                let name = self
                    .active_panel()
                    .recycle_items
                    .iter()
                    .find(|r| r.suffix == suffix.as_str())
                    .map(|r| r.name.clone())
                    .unwrap_or_default();
                crate::lang::tr("彻底删除 “%s”？\n（直接删除回收站文件，不可恢复）", self.lang).replace("%s", &name)
            } else if !self.delete_targets.is_empty() {
                let n = self.delete_targets.len();
                let perm = self.delete_permanent;
                let tail = crate::lang::tr(
                    if perm {
                        "项？\n（永久删除，不进回收站）"
                    } else {
                        "项？\n（移到回收站，可还原）"
                    },
                    self.lang,
                );
                if n == 1 {
                    let target = self
                        .delete_targets
                        .first()
                        .map(|p| p.display().to_string())
                        .unwrap_or_default();
                    crate::lang::tr(
                        if perm {
                            "删除 “%s”？\n（永久删除，不进回收站）"
                        } else {
                            "删除 “%s”？\n（移到回收站，可还原）"
                        },
                        self.lang,
                    )
                    .replace("%s", &target)
                } else {
                    format!(
                        "{} {} {}",
                        crate::lang::tr("删除选中的", self.lang),
                        n,
                        tail
                    )
                }
            } else {
                String::new()
            };
            let mut open = true;
            egui::Window::new(crate::lang::tr("确认删除", self.lang))
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(&msg);
                    ui.horizontal(|ui| {
                        if ui.button(crate::lang::tr("删除", self.lang)).clicked() { self.confirm_delete(); }
                        if ui.button(crate::lang::tr("取消", self.lang)).clicked() {
                            self.delete_open = false;
                            self.delete_targets.clear();
                            self.delete_rec_suffix = None;
                        }
                    });
                });
            self.delete_open = self.delete_open && open;
        }

        if self.empty_open {
            let count = self.active_panel().recycle_items.len();
            let mut open = true;
            egui::Window::new(crate::lang::tr("清空回收站", self.lang))
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("{}{}{}", crate::lang::tr("确定清空回收站吗？共", self.lang), count, crate::lang::tr("项。", self.lang)));
                    ui.label(crate::lang::tr("后台逐个删除 $R/$I 文件，界面不卡顿。", self.lang));
                    ui.horizontal(|ui| {
                        if ui.button(crate::lang::tr("清空", self.lang)).clicked() {
                            self.empty_open = false;
                            // Emptying the whole bin can touch other accounts'
                            // items, which needs admin — launch one elevated
                            // instance just for this, then it exits. If UAC is
                            // declined, fall back to the in-process empty
                            // (current user only).
                            if crate::shell::relaunch_elevated_with(&["--empty-recycle"]) {
                                self.status =
                                    crate::lang::tr("已请求提权清空回收站…", self.lang).into();
                            } else {
                                self.spawn_empty_recycle();
                            }
                        }
                        if ui.button(crate::lang::tr("取消", self.lang)).clicked() { self.empty_open = false; }
                    });
                });
            self.empty_open = self.empty_open && open;
        }

        if self.new_folder_open {
            let mut open = true;
            egui::Window::new(crate::lang::tr("新建文件夹", self.lang))
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(crate::lang::tr("名称:", self.lang));
                    ui.text_edit_singleline(&mut self.new_folder_name);
                    ui.horizontal(|ui| {
                        if ui.button(crate::lang::tr("创建", self.lang)).clicked() { self.create_folder(); }
                        if ui.button(crate::lang::tr("取消", self.lang)).clicked() { self.new_folder_open = false; }
                    });
                });
            self.new_folder_open = self.new_folder_open && open;
        }

        if self.go_open {
            let mut open = true;
            egui::Window::new(crate::lang::tr("跳转到路径", self.lang))
                .open(&mut open)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(crate::lang::tr("路径:", self.lang));
                    ui.text_edit_singleline(&mut self.go_path);
                    ui.horizontal(|ui| {
                        ui.label(crate::lang::tr("应用到:", self.lang));
                        ui.radio_value(&mut self.go_target, false, crate::lang::tr("左栏", self.lang));
                        ui.radio_value(&mut self.go_target, true, crate::lang::tr("右栏", self.lang));
                        if ui.button(crate::lang::tr("跳转", self.lang)).clicked() { self.go_to(); }
                        if ui.button(crate::lang::tr("取消", self.lang)).clicked() { self.go_open = false; }
                    });
                });
            self.go_open = self.go_open && open;
        }

        if self.rename_open {
            let mut open = true;
            let mut go = false;
            egui::Window::new(crate::lang::tr("重命名", self.lang))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(crate::lang::tr("新名称:", self.lang));
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.rename_name).desired_width(280.0),
                    );
                    self.text_focus |= resp.has_focus();
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        go = true;
                    }
                    ui.horizontal(|ui| {
                        if ui.button(crate::lang::tr("确定", self.lang)).clicked() {
                            go = true;
                        }
                        if ui.button(crate::lang::tr("取消", self.lang)).clicked() {
                            self.rename_open = false;
                        }
                    });
                });
            if go {
                self.confirm_rename();
            } else {
                self.rename_open = self.rename_open && open;
            }
        }

        if let Some(p) = self.props.clone() {
            let mut open = true;
            egui::Window::new(crate::lang::tr("属性", self.lang))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("{}{}", crate::lang::tr("名称:", self.lang), p.name));
                    ui.label(format!("{}{}", crate::lang::tr("路径:", self.lang), p.path.display()));
                    ui.label(format!(
                        "{}{}",
                        crate::lang::tr("类型:", self.lang),
                        if p.is_dir { crate::lang::tr("文件夹", self.lang) } else { crate::lang::tr("文件", self.lang) }
                    ));
                    if p.is_dir {
                        if let Some(n) = p.entries {
                            ui.label(format!("{}{}{}", crate::lang::tr("包含:", self.lang), n, crate::lang::tr("项", self.lang)));
                        }
                        ui.label(format!("{}{}", crate::lang::tr("大小:", self.lang), human_size(p.size)));
                    } else {
                        ui.label(format!(
                            "{}{} （{}{}）",
                            crate::lang::tr("大小:", self.lang),
                            human_size(p.size),
                            p.size,
                            crate::lang::tr("字节", self.lang)
                        ));
                    }
                    ui.label(format!(
                        "{}{}",
                        crate::lang::tr("创建时间:", self.lang),
                        human_datetime(p.ctime)
                    ));
                    ui.label(format!(
                        "{}{}",
                        crate::lang::tr("修改时间:", self.lang),
                        human_datetime(p.mtime)
                    ));
                    ui.horizontal(|ui| {
                        if ui.button(crate::lang::tr("复制路径", self.lang)).clicked() {
                            ui.ctx().copy_text(p.path.to_string_lossy().into_owned());
                        }
                        if ui.button(crate::lang::tr("关闭", self.lang)).clicked() {
                            self.props = None;
                        }
                    });
                });
            if !open {
                self.props = None;
            }
        }

        // ----- built-in image viewer (its own OS window) -----
        if let Some(vpath) = self.viewer.clone() {
            // An *immediate viewport*, not an in-app `egui::Window`: the
            // picture gets a real OS window of its own, so it can move to a
            // second monitor and never hides behind the panes. Its key and
            // close events never reach the main window, so the X button and
            // Escape are handled on the viewport's own context below.
            let title = format!(
                "{}{}",
                crate::lang::tr("预览 - ", self.lang),
                vpath
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            );
            let n_imgs = self.viewer_list.len();
            let vp = egui::ViewportId::from_hash_of("image_viewer");
            let builder = egui::ViewportBuilder::default()
                .with_title(title.clone())
                .with_inner_size([900.0, 700.0])
                .with_min_inner_size([320.0, 240.0]);
            ctx.show_viewport_immediate(vp, builder, |vctx, class| {
                if class == egui::ViewportClass::Embedded {
                    // Backend without multi-window support: fall back to the
                    // old in-app window so the picture still shows up.
                    let mut open = true;
                    egui::Window::new(&title)
                        .open(&mut open)
                        .default_size([900.0, 700.0])
                        .resizable(true)
                        .show(vctx, |ui| self.viewer_body(ui, &vpath, n_imgs));
                    if !open {
                        self.viewer = None;
                        self.reset_video_state();
                    }
                    return;
                }
                // The OS window's own close button, and Escape. The frame
                // still renders once so the window never flashes black on
                // its way out; the next pass skips this block and eframe
                // garbage-collects the window.
                if vctx.input(|i| i.viewport().close_requested())
                    || vctx.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    self.viewer = None;
                    self.reset_video_state();
                }
                egui::CentralPanel::default()
                    .show(vctx, |ui| self.viewer_body(ui, &vpath, n_imgs));
                // This OS window has its own repaint schedule: the main
                // window's request never reaches it, and the decode finishing
                // in the background raises no input event. Without this the
                // viewer would sit on "加载中…" until the mouse moved.
                // GIF playback is driven here too: its clip decodes on a
                // worker thread and the control bar must keep repainting to
                // pick the result up and advance frames.
                //
                // CRUCIAL: the viewport stops processing input the moment it
                // stops repainting. A paused clip (video_playing == false,
                // gif_playing == false) would otherwise ignore its own
                // play/pause click and feel "stuck". So we ALWAYS request a
                // repaint while the viewer is open — fast while something is
                // animating, slow-but-alive otherwise.
                let animating = self.video_playing
                    || self.gif_playing
                    || self.previews.busy()
                    || !self.video_jobs.is_empty()
                    || self.gif_rx.is_some()
                    || self.side_gif_rx.is_some();
                // Perf mode sets the baseline repaint cadence while a clip plays.
                // 小 = 50 ms (lowest overhead), 中 = 33 ms, 大 = 16 ms (smoothest).
                // Media code (e.g. GIF) overrides this with a precise per-frame
                // hint so we only wake when the next frame is actually due.
                let anim_base: u64 = match self.viewer_perf {
                    0 => 50,
                    2 => 16,
                    _ => 33,
                };
                let ms = if animating {
                    self.repaint_hint_ms.unwrap_or(anim_base)
                } else {
                    200
                };
                vctx.request_repaint_after(std::time::Duration::from_millis(ms));
                // Dialogs reached from the viewer's right-click menu show up
                // in *this* window — the main window may sit behind it.
                if self.convert.is_some() {
                    self.render_convert_dialog(vctx);
                }
            });
        }

        // ----- image → ICO / PNG conversion dialog -----
        if self.viewer.is_none() {
            self.render_convert_dialog(ctx);
        }

        if let Some(info) = self.rec_info.clone() {
            let mut open = true;
            egui::Window::new(crate::lang::tr("回收站项目", self.lang))
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(format!("{}{}", crate::lang::tr("名称:", self.lang), info.name));
                    if !info.orig_path.is_empty() {
                        ui.label(format!("{}{}", crate::lang::tr("原位置:", self.lang), info.orig_path));
                    }
                    ui.label(format!(
                        "{}{}",
                        crate::lang::tr("类型:", self.lang),
                        if info.is_dir { crate::lang::tr("文件夹", self.lang) } else { crate::lang::tr("文件", self.lang) }
                    ));
                    if !info.is_dir {
                        ui.label(format!("{}{}", crate::lang::tr("大小:", self.lang), human_size(info.size)));
                    }
                    if info.deleted_at == 0 {
                        ui.label(crate::lang::tr("删除时间: 未知", self.lang));
                    } else {
                        ui.label(format!(
                            "{}{}",
                            crate::lang::tr("删除时间:", self.lang),
                            human_datetime(info.deleted_at)
                        ));
                    }
                    ui.label(format!("{}{}", crate::lang::tr("流后缀:", self.lang), info.suffix));
                    ui.label(crate::lang::tr("回收站项目可以“还原”回原位置，或“彻底删除”。", self.lang));
                    ui.horizontal(|ui| {
                        if ui.button(crate::lang::tr("彻底删除", self.lang)).clicked() {
                            let suffix = info.suffix.clone();
                            self.rec_info = None;
                            self.delete_rec_suffix = Some(suffix);
                            self.delete_open = true;
                        }
                        if ui.button(crate::lang::tr("关闭", self.lang)).clicked() { self.rec_info = None; }
                    });
                });
            if !open {
                self.rec_info = None;
            }
        }

        // ----- keyboard -----
        let text_edit_focus = self.text_focus;
        let any_dialog = self.delete_open
            || self.empty_open
            || self.new_folder_open
            || self.go_open
            || self.rec_info.is_some()
            || self.rename_open
            || self.props.is_some()
            || self.addr_edit.is_some()
            || self.convert.is_some();
        let key = |k: egui::Key| ctx.input(|i| i.key_pressed(k));
        if key(egui::Key::Escape) {
            self.addr_edit = None;
            self.delete_open = false;
            self.delete_targets.clear();
            self.delete_rec_suffix = None;
            self.empty_open = false;
            self.new_folder_open = false;
            self.go_open = false;
            self.rec_info = None;
            self.rename_open = false;
            self.props = None;
            self.convert = None;
            // Esc also drops the multi-selection, like Explorer.
            let is_left = self.active_pane == 0;
            self.pane_of(is_left).multi.clear();
        }
        if !text_edit_focus {
            if key(egui::Key::Delete) {
                if ctx.input(|i| i.modifiers.shift) {
                    self.do_delete_permanent();
                } else {
                    self.do_delete();
                }
            }
            // A confirmation dialog is open: Enter confirms it. Esc is handled
            // by the global handler above (it cancels every dialog), so only
            // the confirm side is wired here. The destructive one is delete;
            // Enter on it commits the deletion the user just asked for.
            if key(egui::Key::Enter) {
                if self.delete_open {
                    self.confirm_delete();
                }
            }
            if !any_dialog && !self.enter_consumed && key(egui::Key::Enter) { self.open_selected(); }
            if !any_dialog {
                if key(egui::Key::ArrowUp) { self.move_selection(-1, false, false); }
                if key(egui::Key::ArrowDown) { self.move_selection(1, false, false); }
                if key(egui::Key::Home) { self.move_selection(0, true, false); }
                if key(egui::Key::End) { self.move_selection(0, false, true); }
            }
        }
        if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowLeft)) {
            self.active_panel().back();
            self.apply_cached_sizes();
        }
        if ctx.input(|i| i.modifiers.alt && i.key_pressed(egui::Key::ArrowRight)) {
            self.active_panel().forward();
            self.apply_cached_sizes();
        }
        if !text_edit_focus {
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::T)) {
                self.new_tab();
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::W)) {
                let idx = self.active_tab;
                self.close_tab(idx);
            }
            // Selection / clipboard shortcuts — the ones a file manager is
            // expected to have. Skipped while the image viewer is open (its
            // own window owns the keyboard then).
            if self.viewer.is_none() {
                if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::A)) {
                    self.select_all();
                }
                // egui-winit rewrites Ctrl+C / Ctrl+X / Ctrl+V into clipboard
                // *commands* (`Event::Copy` / `Cut` / `Paste`) and never emits
                // a `Key::C` / `X` / `V` event, so `key_pressed()` above can
                // never see them. Drive the file clipboard off those commands.
                let (copy_ev, cut_ev, paste_ev) = ctx.input(|i| {
                    let (mut c, mut x, mut v) = (false, false, false);
                    for e in &i.events {
                        match e {
                            egui::Event::Copy => c = true,
                            egui::Event::Cut => x = true,
                            egui::Event::Paste(_) => v = true,
                            _ => {}
                        }
                    }
                    (c, x, v)
                });
                if copy_ev { self.do_copy(); }
                if cut_ev { self.do_move(); }
                if paste_ev { self.do_paste(); }
            }
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Tab)) {
                let next = (self.active_tab + 1) % self.tabs.len();
                self.active_tab = next;
                self.persist_layout();
            }
            if ctx.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(egui::Key::Tab)) {
                let prev = if self.active_tab == 0 { self.tabs.len() - 1 } else { self.active_tab - 1 };
                self.active_tab = prev;
                self.persist_layout();
            }
        }
        // An action like Ctrl+C has no visual result of its own, so remember
        // when the status line last changed: the active pane's status band
        // draws it highlighted for a few seconds (see `draw_pane_status`).
        if self.status != self.status_prev {
            self.status_prev = self.status.clone();
            self.status_at = Instant::now();
        }
        self.status_fresh =
            !self.status.is_empty() && self.status_at.elapsed() < Duration::from_secs(8);
        // Only repaint while something actually changes. Unconditional
        // repainting keeps egui in continuous mode and burns a whole core on
        // an idle window. Perf mode scales the polling cadence: even when
        // something is in flight, 小 polls slower (fewer wakeups), 大 faster.
        if self.needs_repaint() {
            let ms = match self.viewer_perf {
                0 => 200, // 小: 5 wakeups/s while a scan job runs
                2 => 60,  // 大: ~16 wakeups/s, snappier progress
                _ => 100, // 中: 10 fps is plenty for a progress bar
            };
            ctx.request_repaint_after(Duration::from_millis(ms));
        }
    }
}

/// Human-friendly label for a key in the bottom-left echo. Letters/digits
/// fall through to `Debug` (which already gives `A`, `Z`, …); the ones a user
/// actually reaches for get short names (`Del`, `Esc`, arrows).
fn key_name(k: egui::Key) -> String {
    use egui::Key::*;
    match k {
        Delete => "Del".into(),
        Enter => "Enter".into(),
        Escape => "Esc".into(),
        Space => "Space".into(),
        Backspace => "Backspace".into(),
        Tab => "Tab".into(),
        Insert => "Ins".into(),
        ArrowUp => "↑".into(),
        ArrowDown => "↓".into(),
        ArrowLeft => "←".into(),
        ArrowRight => "→".into(),
        Home => "Home".into(),
        End => "End".into(),
        PageUp => "PgUp".into(),
        PageDown => "PgDn".into(),
        Num0 => "0".into(),
        Num1 => "1".into(),
        Num2 => "2".into(),
        Num3 => "3".into(),
        Num4 => "4".into(),
        Num5 => "5".into(),
        Num6 => "6".into(),
        Num7 => "7".into(),
        Num8 => "8".into(),
        Num9 => "9".into(),
        _ => format!("{k:?}"),
    }
}

impl Explorer {
    /// One Quick-Access row in the left rail: click or double-click navigates,
    /// right-click opens the folder menu (open / new tab / VS Code / terminal /
    /// reveal / copy path / properties / favourite), and the favourites also
    /// get a small × to drop them without opening the menu.
    ///
    /// `glyph` is resolved by the caller: `self.icons` must be borrowed *before*
    /// this runs, otherwise it collides with the `&mut self` navigation below.
    fn qa_row(
        &mut self,
        ui: &mut egui::Ui,
        path: &std::path::Path,
        glyph: icons::Glyph,
        row_w: f32,
        kind: QaKind,
    ) {
        // Only Marin's own favourites are ours to remove: Windows' pins live in
        // the jump list, and dropping them here would do nothing.
        let mine = kind == QaKind::Favorite;
        let label = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        // Three kinds share the rail, so the hover line says which one this row
        // came from — and for the read-only ones, why there is no ×.
        let hint = match kind {
            QaKind::Favorite => format!(
                "{}\n{}",
                path.to_string_lossy(),
                crate::lang::tr("已收藏", self.lang)
            ),
            QaKind::WinPin => format!(
                "{}\n{}",
                path.to_string_lossy(),
                crate::lang::tr("Windows 已固定此文件夹", self.lang)
            ),
            QaKind::Recent => path.to_string_lossy().into_owned(),
        };
        ui.horizontal(|ui| {
            let r = icons::entry(ui, glyph, &label, false, &hint, row_w);
            // Single click already navigates (the rail is a jump target, not a
            // selection list); double-click does the same so the row also
            // answers the "open it" reflex.
            if r.clicked() || r.double_clicked() {
                if path.is_dir() {
                    self.active_panel().navigate(path.to_path_buf());
                    self.apply_cached_sizes();
                } else {
                    crate::fs_util::open_default_file(&path);
                }
            }
            let _ = r.context_menu(|ui| {
                self.qa_context_menu(ui, path);
            });
            if mine {
                if icons::small_button(ui, &icons::X, &crate::lang::tr("取消收藏", self.lang))
                    .clicked()
                {
                    self.unfavorite(path);
                }
            }
        });
    }

    // ----- performance baseline -----

    /// Repaint cadence (ms) for the main window while background work is in
    /// flight: thumbnail decodes, folder-size scans, side-pane posters. This is
    /// the global throttle behind the 小 / 中 / 大 setting — it governs the
    /// whole file manager, not just the viewer:
    ///   小 (0) = 120 ms  → fewest wakeups, lowest CPU (icons instead of grid
    ///                       thumbnails, lighter folder-size scans)
    ///   中 (1) = 60 ms   → balanced (default)
    ///   大 (2) = 30 ms   → smooth, most work per second
    ///
    /// Media in the viewer viewport keeps its own precise per-frame hint; this
    /// only bounds *polling* while something is decoding in the background.
    pub fn perf_repaint_ms(&self) -> u64 {
        match self.viewer_perf {
            0 => 120,
            2 => 30,
            _ => 60,
        }
    }

    // ----- image conversion dialog (ICO / PNG) -----

    /// Window shell for the convert dialog. The state is taken out of
    /// `self.convert` for the frame, so the body can borrow the state and
    /// `self` (for `lang`) at the same time.
    /// Body of the built-in image viewer: the toolbar, the picture
    /// itself with the prev / next overlay arrows, and the bottom
    /// filmstrip. Rendered into whatever surface the caller hands us —
    /// the viewer's own OS window (a viewport `CentralPanel`) or a plain
    /// in-app `Window` on backends without multi-window support.
    fn viewer_body(&mut self, ui: &mut egui::Ui, vpath: &std::path::Path, n_imgs: usize) {
        // Reset the per-frame repaint hint; media code re-sets it if it has a
        // precise wake time (GIF). Video leaves it None and the viewport falls
        // back to the perf-mode base cadence.
        self.repaint_hint_ms = None;
        // One viewer for both media kinds: stills and clips share the toolbar,
        // the prev / next navigation and the filmstrip; only the content area
        // differs (decoded picture vs. video surface).
        if self.is_video_viewer(vpath) {
            self.viewer_toolbar(ui, vpath, n_imgs, true);
            ui.separator();
            self.viewer_key_nav(ui, n_imgs);
            self.render_video_surface(ui, vpath, n_imgs);
            if n_imgs > 1 {
                self.viewer_filmstrip(ui, n_imgs);
            }
            return;
        }
        self.viewer_toolbar(ui, vpath, n_imgs, false);
        ui.separator();
        self.viewer_key_nav(ui, n_imgs);
        // A GIF shows a play/pause + frame-scrubber bar below the picture, so
        // reserve its height up front or the bar gets pushed off-screen.
        let is_gif = self.is_gif_viewer(vpath);
        // Main image: centred in the space left above the filmstrip, drag to
        // pan when zoomed, and left/right overlay arrows at the window edges.
        let film_h = if n_imgs > 1 { 84.0 } else { 0.0 };
        let gifbar_h = if is_gif { 26.0 } else { 0.0 };
        let area_w = ui.available_width().max(40.0);
        let area_h = (ui.available_height() - film_h - gifbar_h).max(60.0);
        let area = egui::Rect::from_min_size(
        ui.cursor().min,
        egui::vec2(area_w, area_h),
        );
        // One interaction for the whole image area: drag pans a zoomed image,
        // right-click opens the viewer menu. Allocated before the image is
        // painted so the menu also works while the decode is still running.
        let area_resp = ui.interact(
        area,
        egui::Id::new("viewer_area"),
        egui::Sense::click_and_drag(),
        );
        let _ = area_resp.context_menu(|ui| self.viewer_context_menu(ui, &vpath));
        if area_resp.dragged() {
        self.viewer_pan += area_resp.drag_delta();
        }
        // ---- animated GIF? ----
        // A GIF is decoded as a whole clip (background thread) and animated
        // frame by frame here; everything else goes through the still path.
        if is_gif {
            self.ensure_gif_clip(vpath);
            let mut landed: Option<(std::path::PathBuf, Option<crate::gif::GifClip>)> = None;
            let mut disconnected = false;
            if let Some((p, rx)) = self.gif_rx.as_ref() {
                match rx.try_recv() {
                    Ok(clip) => landed = Some((p.clone(), clip)),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        // Worker panicked (or file vanished): never hangs.
                        disconnected = true;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {}
                }
            }
            if let Some((p, clip)) = landed {
                self.gif_rx = None;
                match clip {
                    Some(c) => {
                        self.gif_clip = Some((p, c));
                        self.gif_idx = 0;
                        self.gif_due = None;
                        self.gif_playing = true;
                    }
                    None => {
                        self.gif_failed.insert(p);
                    }
                }
            } else if disconnected {
                if let Some((p, _)) = self.gif_rx.take() {
                    self.gif_failed.insert(p);
                }
            }
            // GIF: animate and show the play/pause + scrubber bar. The bar is
            // drawn on every frame (even while the clip is still decoding) so
            // the user always sees controls instead of a silent static image.
            if self.gif_clip.is_none() {
                // Clip still decoding: show the cached first frame at once
                // (poster, like a video) instead of a blank "准备中" screen.
                self.paint_gif_poster_still(ui, area, vpath);
            }
            let gif_finished = self.step_gif(ui, area);
            self.viewer_edge_arrows(ui, area, &vpath, n_imgs);
            // Centre play/pause badge — mirrors the video viewer. Clicking it
            // toggles the animation; it shows ⏸ while playing and ▶ when
            // Centre click toggles play/pause (like the video surface) but we
            // draw no button — the user wanted the visible badge gone, yet
            // clicking the middle should still pause/resume. An invisible
            // interaction over the centre handles that.
            if self.gif_clip.is_some() {
                let badge =
                    egui::Rect::from_center_size(area.center(), egui::vec2(96.0, 96.0));
                let badge_resp = ui.interact(
                    badge,
                    egui::Id::new("gif_center_badge"),
                    egui::Sense::click(),
                );
                if badge_resp.clicked() {
                    self.gif_playing = !self.gif_playing;
                    self.viewer_play_on = self.gif_playing;
                    if self.gif_playing {
                        self.gif_due = None;
                    }
                }
            }
            ui.advance_cursor_after_rect(area);
            self.gif_play_controls(ui, n_imgs);
            if n_imgs > 1 {
                self.viewer_filmstrip(ui, n_imgs);
            }
            if gif_finished && self.viewer_auto_next && n_imgs > 1 {
                self.viewer_nav(1);
            }
            return;
        } else if self.gif_clip.is_some() || self.gif_rx.is_some() {
            // Left a GIF behind: free the frames.
            self.reset_gif_state();
        }

        // Main image. While the full-size decode is still running, any smaller
        // size already in the cache (the grid thumbnail, usually) is stretched
        // into place first, so stepping through a folder never shows a blank
        // frame. `ready` says whether what we painted is the real thing.
        let weak_col = self.weak_color();
        let loading_txt = crate::lang::tr("加载中…", self.lang);
        let (tex, ready) = self
            .previews
            .get_or_smaller(&vpath, preview::FULL, preview::Priority::Urgent);
        if let Some(t) = tex {
        let (mut w, mut h) = (t.w as f32, t.h as f32);
        if self.viewer_zoom <= 0.0 {
        let s = (area_w / w).min(area_h / h).min(1.0);
        w *= s;
        h *= s;
        } else {
        w *= self.viewer_zoom;
        h *= self.viewer_zoom;
        }
        let dest = egui::Rect::from_center_size(area.center() + self.viewer_pan, egui::vec2(w, h));
        ui.painter().image(
        t.tex.id(),
        dest,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
        );
        // Left / right overlay arrows, pinned to the window edges. Allocated
        // after the image so, on a pointer-hit tie, the arrow wins over the
        // drag area underneath it. A dark translucent pill keeps the glyph
        // readable over any photo.
        self.viewer_edge_arrows(ui, area, &vpath, n_imgs);
        if !ready {
            // A smaller cached size is on screen; say so while the sharp one
            // decodes instead of pretending this is final.
            ui.painter().text(
                area.left_bottom() + egui::vec2(10.0, -10.0),
                egui::Align2::LEFT_BOTTOM,
                &loading_txt,
                egui::FontId::proportional(13.0),
                weak_col,
            );
        }
        } else {
        ui.painter().text(
        area.center(),
        egui::Align2::CENTER_CENTER,
        &loading_txt,
        egui::FontId::proportional(18.0),
        weak_col,
        );
        }
        // The main image + its interaction area are painted directly onto the
        // painter (they don't consume layout space), so the cursor is still at
        // the top. Advance it past the image region now so the filmstrip below
        // actually lands at the bottom instead of overlapping the top of the
        // image. The reserved `film_h` slice is exactly what's left.
        ui.advance_cursor_after_rect(area);
        if n_imgs > 1 {
            self.viewer_filmstrip(ui, n_imgs);
        }
        // 自动下一页 on a still: hold it for `viewer_slide_secs`, then step on.
        if self.tick_slide(vpath, n_imgs) {
            self.viewer_nav(1);
        } else if self.viewer_auto_next && n_imgs > 1 {
            // Wake up in time to make the step, without repainting at full
            // frame rate for the whole wait.
            let left = self
                .slide_at
                .map(|t| t.elapsed().as_secs())
                .map(|e| self.viewer_slide_secs.max(1) as u64 - e)
                .unwrap_or(1);
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(left.min(500) * 1000));
        }
    }


    /// Toolbar shared by both media kinds: prev / next, the per-kind controls
    /// (zoom for images, play-pause for clips), the file actions and the
    /// position inside the folder.
    fn viewer_toolbar(
        &mut self,
        ui: &mut egui::Ui,
        vpath: &std::path::Path,
        n_imgs: usize,
        is_video: bool,
    ) {
        ui.horizontal_wrapped(|ui| {
            // Greyed out when the folder holds a single item: the buttons must
            // not look alive and do nothing.
            ui.add_enabled_ui(n_imgs > 1, |ui| {
                if icons::small_button(ui, &icons::CHEV_LEFT, &crate::lang::tr("上一张", self.lang))
                    .clicked()
                {
                    self.viewer_nav(-1);
                }
                if icons::small_button(ui, &icons::CHEV_RIGHT, &crate::lang::tr("下一张", self.lang))
                    .clicked()
                {
                    self.viewer_nav(1);
                }
            });
            ui.separator();
            if is_video {
                self.video_play_controls(ui, vpath);
            } else {
                self.image_zoom_controls(ui);
            }
            ui.separator();
            if ui.button(crate::lang::tr("用默认程序打开", self.lang)).clicked() {
                fs_util::open_default_file(&vpath);
            }
            if ui.button(crate::lang::tr("在资源管理器中显示", self.lang)).clicked() {
                fs_util::reveal_in_explorer(&vpath);
            }
            ui.separator();
            if ui.button(crate::lang::tr("复制文件", self.lang)).clicked() {
                // Put the *file* on the clipboard (CF_HDROP) so it can be pasted
                // into QQ / Explorer / an upload box. For an animated GIF or a
                // video this copies the real file, not a single still frame.
                fs_util::set_clipboard_files(std::slice::from_ref(&vpath.to_path_buf()));
                self.status = crate::lang::tr("已复制文件", self.lang).into();
            }
            ui.separator();
            // Slideshow options, shared by stills and clips and remembered
            // across restarts (written to the store the moment they change).
            let mut lp = self.viewer_loop;
            if ui
                .checkbox(&mut lp, crate::lang::tr("循环播放", self.lang))
                .changed()
            {
                self.viewer_loop = lp;
                self.persist_viewer_opts();
            }
            let mut an = self.viewer_auto_next;
            if ui
                .checkbox(&mut an, crate::lang::tr("自动下一页", self.lang))
                .changed()
            {
                self.viewer_auto_next = an;
                self.persist_viewer_opts();
            }
            if self.viewer_auto_next {
                let mut secs = self.viewer_slide_secs as i32;
                if ui
                    .add(
                        egui::DragValue::new(&mut secs)
                            .range(1..=3600)
                            .suffix(crate::lang::tr(" 秒", self.lang)),
                    )
                    .changed()
                {
                    self.viewer_slide_secs = secs.clamp(1, 3600) as u32;
                    self.persist_viewer_opts();
                }
            }
            ui.separator();
            if n_imgs > 1 {
                weak(ui, format!("({}/{})", self.viewer_index + 1, n_imgs));
            }
        });
    }

    /// Zoom buttons plus the current-zoom readout (still images only).
    fn image_zoom_controls(&mut self, ui: &mut egui::Ui) {
        if ui.button(crate::lang::tr("适应窗口", self.lang)).clicked() {
            self.viewer_zoom = 0.0;
            self.viewer_pan = egui::Vec2::ZERO;
        }
        if ui.button("1:1").clicked() {
            self.viewer_zoom = 1.0;
            self.viewer_pan = egui::Vec2::ZERO;
        }
        if icons::small_button(ui, &icons::ZOOM_IN, &crate::lang::tr("放大", self.lang)).clicked() {
            let z = if self.viewer_zoom <= 0.0 { 1.0 } else { self.viewer_zoom };
            self.viewer_zoom = (z * 1.25).min(8.0);
            self.viewer_pan = egui::Vec2::ZERO;
        }
        if icons::small_button(ui, &icons::ZOOM_OUT, &crate::lang::tr("缩小", self.lang)).clicked() {
            let z = if self.viewer_zoom <= 0.0 { 1.0 } else { self.viewer_zoom };
            self.viewer_zoom = (z / 1.25).max(0.05);
            self.viewer_pan = egui::Vec2::ZERO;
        }
        if self.viewer_zoom <= 0.0 {
            weak(ui, crate::lang::tr("缩放: 适应窗口", self.lang));
        } else {
            weak(
                ui,
                format!(
                    "{} {:.0}%",
                    crate::lang::tr("缩放", self.lang),
                    self.viewer_zoom * 100.0
                ),
            );
        }
    }

    /// Arrow keys and the mouse wheel step through the folder, for images and
    /// clips alike. One step per wheel notch (throttled to 100 ms).
    fn viewer_key_nav(&mut self, ui: &mut egui::Ui, n_imgs: usize) {
        if n_imgs <= 1 {
            return;
        }
        // The viewer lives in its own OS window, so these events arrive on the
        // viewport's context.
        let nav = ui.input(|i| {
            if i.key_pressed(egui::Key::ArrowLeft) {
                Some(-1)
            } else if i.key_pressed(egui::Key::ArrowRight) {
                Some(1)
            } else {
                None
            }
        });
        if let Some(d) = nav {
            self.viewer_nav(d);
        }
        let wheel_dy = ui.input(|i| i.raw_scroll_delta.y);
        if wheel_dy.abs() > 0.01 {
            let ok = self
                .viewer_wheel_at
                .map(|t| t.elapsed().as_millis() >= 100)
                .unwrap_or(true);
            if ok {
                self.viewer_wheel_at = Some(std::time::Instant::now());
                // Scroll up (negative delta.y) → previous image, like a film
                // strip being wound back. Scroll down → next.
                self.viewer_nav(if wheel_dy > 0.0 { -1 } else { 1 });
            }
        }
    }

    /// Prev / next pills pinned to the left and right edges of the content
    /// area. Allocated after the picture so, on a pointer-hit tie, the arrow
    /// wins over the surface underneath it.
    fn viewer_edge_arrows(
        &mut self,
        ui: &mut egui::Ui,
        area: egui::Rect,
        vpath: &std::path::Path,
        n_imgs: usize,
    ) {
        if n_imgs <= 1 {
            return;
        }
        let pill = egui::vec2(34.0, 54.0);
        for (side, delta) in [(-1.0f32, -1isize), (1.0, 1)] {
            let glyph = if side < 0.0 {
                &icons::CHEV_LEFT
            } else {
                &icons::CHEV_RIGHT
            };
            let cx = if side < 0.0 {
                area.min.x + 6.0
            } else {
                area.max.x - pill.x - 6.0
            };
            let brect = egui::Rect::from_center_size(
                egui::pos2(cx + pill.x * 0.5, area.center().y),
                pill,
            );
            let r = ui.interact(
                brect,
                egui::Id::new(("viewer_arrow", side < 0.0)),
                egui::Sense::click(),
            );
            if r.clicked() {
                self.viewer_nav(delta);
            }
            let _ = r.context_menu(|ui| self.viewer_context_menu(ui, vpath));
            ui.painter().rect_filled(
                brect,
                egui::CornerRadius { nw: 6, ne: 6, se: 6, sw: 6 },
                egui::Color32::from_black_alpha(90),
            );
            let col = ui.visuals().text_color();
            icons::paint(ui.painter(), glyph.0, brect.shrink(9.0), col, 2.0);
        }
    }

    /// Filmstrip: pinned to the very bottom by the vertical flow above. When
    /// every thumbnail fits we centre the row; when they would overflow we fall
    /// back to a sideways scroll.
    fn viewer_filmstrip(&mut self, ui: &mut egui::Ui, n_imgs: usize) {
        ui.separator();
        let avail_w = ui.available_width();
        let start = self.viewer_index.saturating_sub(5);
        let end = (self.viewer_index + 5).min(n_imgs - 1);
        let cnt = end.saturating_sub(start) + 1;
        let total = cnt as f32 * 64.0 + (cnt.saturating_sub(1) as f32) * 6.0;
        if total <= avail_w {
            ui.horizontal(|ui| {
                ui.add_space((avail_w - total) / 2.0);
                self.draw_film_thumbs(ui, n_imgs);
            });
        } else {
            egui::ScrollArea::horizontal()
                .id_salt("filmstrip")
                .auto_shrink([false, true])
                .show(ui, |ui| self.draw_film_thumbs(ui, n_imgs));
        }
    }

    /// `true` when the viewer's current file is a video. Caches the decision per
    /// path and resets all per-clip video state whenever the clip changes.
    fn is_video_viewer(&mut self, vpath: &std::path::Path) -> bool {
        if self.viewer_kind_path.as_deref() != Some(vpath) {
            let name = vpath
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            self.viewer_is_video = crate::preview::is_video(&name, vpath);
            self.viewer_is_gif = crate::preview::is_gif(&name, vpath);
            self.viewer_kind_path = Some(vpath.to_path_buf());
            // New clip: drop any in-flight worker, cached frames and playback
            // state. A failed poster decode is simply absent — the job is gone,
            // so nothing retries it until the clip is reopened.
            self.reset_video_state();
            // …and whatever the previous file animated: GIF frames and the
            // slide timer both belong to the file we just left.
            self.reset_gif_state();
            if self.viewer_is_video {
                self.spawn_video_job(vpath, VideoJobKind::Viewer);
            }
        }
        self.viewer_is_video
    }

    /// `true` when the viewer's current file is an animated GIF. Shares the
    /// per-path cache in [`Self::is_video_viewer`], which must run first.
    fn is_gif_viewer(&mut self, vpath: &std::path::Path) -> bool {
        let _ = self.is_video_viewer(vpath);
        self.viewer_is_gif
    }

    /// Drop all per-clip video state (worker, textures, playback position).
    fn reset_video_state(&mut self) {
        self.video_playing = false;
        self.video_playback = None;
        self.video_poster = None;
        self.video_frame = None;
        self.video_pos_ns = 0;
        self.video_dur_ns = None;
        self.video_eos = false;
        self.video_seek_frac = None;
    }

    /// Drop the decoded GIF for the file we just left. Called whenever the
    /// viewer moves to another file (or closes).
    fn reset_gif_state(&mut self) {
        self.gif_rx = None;
        self.gif_clip = None;
        self.gif_idx = 0;
        self.gif_due = None;
        self.gif_playing = true;
        self.gif_tex = None;
        self.gif_painted_idx = usize::MAX;
        self.repaint_hint_ms = None;
        self.slide_path = None;
        self.slide_at = None;
    }

    /// Make sure a whole-clip GIF decode is running for `vpath` (once), and
    /// pick it up when it lands.
    fn ensure_gif_clip(&mut self, vpath: &std::path::Path) {
        let clip_current = self.gif_clip.as_ref().map(|(p, _)| p.as_path()) == Some(vpath);
        if clip_current {
            return;
        }
        let job_current = self.gif_rx.as_ref().map(|(p, _)| p.as_path()) == Some(vpath);
        // Only clear when the in-flight / cached clip belongs to a *different*
        // file. While the same file is still decoding we must NOT reset it on
        // every repaint, or the worker would be killed before it finishes and
        // the viewer would hang on "正在准备 GIF…".
        if !job_current {
            self.reset_gif_state();
        }
        if self.gif_failed.contains(vpath) || self.gif_rx.is_some() {
            return;
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = vpath.to_path_buf();
        std::thread::spawn(move || {
            let clip = crate::gif::decode(&path);
            let _ = tx.send(clip);
        });
        self.gif_rx = Some((vpath.to_path_buf(), rx));
    }

    /// Draw the current GIF frame into `area`, fitted, using one reused
    /// texture (a new `TextureHandle` per frame would leak GPU memory).
    fn paint_gif(&mut self, ui: &mut egui::Ui, area: egui::Rect) {
        let Some((_, clip)) = self.gif_clip.as_ref() else {
            return;
        };
        let Some(frame) = clip.frames.get(self.gif_idx) else {
            return;
        };
        // Upload to the GPU only when the visible frame index changed (or the
        // texture was dropped for a new clip). Re-painting the same frame is a
        // cheap texture blit; the old code rebuilt a ~1 MB RGBA buffer and
        // re-uploaded it every frame, which is what burned CPU.
        if self.gif_painted_idx != self.gif_idx || self.gif_tex.is_none() {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [clip.w as usize, clip.h as usize],
                &frame.rgba,
            );
            match self.gif_tex.as_mut() {
                Some(t) => t.set(img, egui::TextureOptions::LINEAR),
                None => {
                    self.gif_tex = Some(
                        ui.ctx()
                            .load_texture("gif:frame", img, egui::TextureOptions::LINEAR),
                    );
                }
            }
            self.gif_painted_idx = self.gif_idx;
        }
        let Some(tex) = self.gif_tex.as_ref() else {
            return;
        };
        let ar = (clip.w as f32 / clip.h as f32).max(0.01);
        let mut w = area.width();
        let mut h = w / ar;
        if h > area.height() {
            h = area.height();
            w = h * ar;
        }
        let dest = egui::Rect::from_center_size(area.center(), egui::vec2(w, h));
        ui.painter().image(
            tex.id(),
            dest,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    /// While the full clip is still decoding, show the already-cached still
    /// first frame (the grid thumbnail is usually present) so the viewer feels
    /// instant — exactly like a video poster — instead of sitting on a blank
    /// "正在准备 GIF…" for the ~1.7 s the whole 121-frame decode takes.
    fn paint_gif_poster_still(&mut self, ui: &mut egui::Ui, area: egui::Rect, vpath: &std::path::Path) {
        let (tex, _ready) = self
            .previews
            .get_or_smaller(vpath, preview::FULL, preview::Priority::Urgent);
        let Some(t) = tex else {
            return;
        };
        let (mut w, mut h) = (t.w as f32, t.h as f32);
        let s = (area.width() / w).min(area.height() / h).min(1.0);
        w *= s;
        h *= s;
        let dest = egui::Rect::from_center_size(area.center(), egui::vec2(w, h));
        ui.painter().image(
            t.tex.id(),
            dest,
            egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            egui::Color32::WHITE,
        );
    }

    /// Advance the GIF frame when its time is up and repaint it. Returns `true`
    /// once a full pass has finished — the caller turns that into 循环播放
    /// (already handled here) or 自动下一页.
    fn step_gif(&mut self, ui: &mut egui::Ui, area: egui::Rect) -> bool {
        let n = self.gif_clip.as_ref().map(|(_, c)| c.frames.len()).unwrap_or(0);
        if n == 0 {
            return false;
        }
        // Paused: hold the current frame, never advance, never finish. The
        // viewport falls back to its 200 ms idle repaint, which keeps input
        // (the play button) alive without burning CPU.
        if !self.gif_playing {
            self.repaint_hint_ms = None;
            self.paint_gif(ui, area);
            return false;
        }
        let now = std::time::Instant::now();
        if let Some(due) = self.gif_due {
            if now < due {
                // Wait for the next frame: hold the current one on screen and
                // wake exactly when it is due — no 60 fps redraws in between.
                let remaining = (due - now).as_millis() as u64;
                self.repaint_hint_ms = Some(remaining.max(8));
                self.paint_gif(ui, area);
                return false;
            }
        }
        // 循环播放 wins over 自动下一页: looping restarts from frame 0 and
        // never reports "finished", so it stays put instead of paging on.
        let looping = self.viewer_loop;
        if self.gif_idx + 1 >= n {
            if looping {
                self.gif_idx = 0;
            } else if self.gif_idx == n - 1 {
                // Parked on the last frame: keep showing it, no timer.
                self.gif_due = None;
                self.repaint_hint_ms = None;
                self.paint_gif(ui, area);
                return true;
            } else {
                self.gif_idx = n - 1;
            }
        } else {
            self.gif_idx += 1;
        }
        let delay = self
            .gif_clip
            .as_ref()
            .and_then(|(_, c)| c.frames.get(self.gif_idx))
            .map(|f| f.delay_ms)
            .unwrap_or(100);
        self.gif_due = Some(now + std::time::Duration::from_millis(delay as u64));
        // Wake when this frame's delay elapses — the next step_gif advances it.
        self.repaint_hint_ms = Some((delay as u64).max(8));
        self.paint_gif(ui, area);
        false
    }

    /// Play / pause toggle plus a frame scrubber for the current GIF, drawn in
    /// the viewer's bottom bar (mirrors the video seek bar). Pausing holds the
    /// current frame; the scrubber jumps to any frame and resumes from there.
    fn gif_play_controls(&mut self, ui: &mut egui::Ui, n_imgs: usize) {
        let n = self.gif_clip.as_ref().map(|(_, c)| c.frames.len()).unwrap_or(0);
        ui.horizontal(|ui| {
            // While the clip is still decoding there is no play state yet: say
            // so instead of showing a blank bar.
            if n == 0 {
                weak(ui, crate::lang::tr("正在准备 GIF…", self.lang));
                let _ = n_imgs;
                return;
            }
            let label = if self.gif_playing {
                crate::lang::tr("暂停", self.lang)
            } else {
                crate::lang::tr("播放", self.lang)
            };
            if ui.button(label).clicked() {
                self.gif_playing = !self.gif_playing;
                // Remember the intent so stepping to the next file keeps the
                // animation going (or stays paused, if the user paused).
                self.viewer_play_on = self.gif_playing;
                // Resume from the current frame's own timing.
                if self.gif_playing {
                    self.gif_due = None;
                }
            }
            let total = n.max(1);
            let frac = if total <= 1 {
                1.0
            } else {
                (self.gif_idx as f32 / (total - 1) as f32).clamp(0.0, 1.0)
            };
            let bar_w = (ui.available_width() - 72.0).max(40.0);
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(bar_w, 16.0),
                egui::Sense::click_and_drag(),
            );
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(8),
                ui.visuals().widgets.inactive.bg_fill,
            );
            if frac > 0.0 {
                let fill = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(rect.width() * frac, rect.height()),
                );
                painter.rect_filled(fill, egui::CornerRadius::same(8), ui.visuals().selection.bg_fill);
            }
            let kx = rect.min.x + rect.width() * frac;
            painter.circle_filled(
                egui::pos2(kx, rect.center().y),
                6.0,
                ui.visuals().selection.bg_fill,
            );
            if (resp.drag_started() || resp.dragged() || resp.clicked()) && rect.width() > 0.0 {
                if let Some(p) = resp.interact_pointer_pos() {
                    let f = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                    let idx = if total <= 1 {
                        0
                    } else {
                        (f * (total - 1) as f32).round() as usize
                    };
                    self.gif_idx = idx.min(total - 1);
                    // Recompute the delay on the next step from the new frame.
                    self.gif_due = None;
                }
            }
            weak(ui, format!("{}/{}", self.gif_idx + 1, total));
            let _ = n_imgs;
        });
    }

    /// Start (or restart) the still-image slide timer for `vpath`.
    fn tick_slide(&mut self, vpath: &std::path::Path, n_imgs: usize) -> bool {
        if !self.viewer_auto_next || n_imgs <= 1 {
            return false;
        }
        if self.slide_path.as_deref() != Some(vpath) {
            self.slide_path = Some(vpath.to_path_buf());
            self.slide_at = Some(std::time::Instant::now());
            return false;
        }
        let secs = self.viewer_slide_secs.max(1) as u64;
        let elapsed = self.slide_at.map(|t| t.elapsed().as_secs()).unwrap_or(0);
        elapsed >= secs
    }

    /// Spawn a background poster-decode job for `path` unless one is already
    /// running. Viewer/side keep a single slot (switching replaces the old
    /// job); grid thumbnails cap concurrency and skip already-cached paths.
    fn spawn_video_job(&mut self, path: &std::path::Path, kind: VideoJobKind) {
        // A clip that already failed to decode is never retried: the job is
        // dropped on completion, so without this every repaint would spawn a
        // new (doomed) worker and pin a core.
        if self.video_poster_failed.contains(path) {
            return;
        }
        if self.video_jobs.iter().any(|j| j.path == path && j.kind == kind) {
            return;
        }
        match kind {
            VideoJobKind::Viewer | VideoJobKind::Side => {
                self.video_jobs.retain(|j| j.kind != kind);
            }
            VideoJobKind::Thumb => {
                if self
                    .video_jobs
                    .iter()
                    .filter(|j| j.kind == VideoJobKind::Thumb)
                    .count()
                    >= THUMB_VIDEO_INFLIGHT
                {
                    return;
                }
                if self.thumb_video_cache.iter().any(|(p, _)| p == path) {
                    return;
                }
            }
        }
        let max_dim = match kind {
            VideoJobKind::Viewer => 1280,
            VideoJobKind::Side => 640,
            VideoJobKind::Thumb => 256,
        };
        self.video_jobs.push(VideoJob {
            path: path.to_path_buf(),
            rx: crate::video::poster_rgba_scaled_async(path, max_dim),
            kind,
        });
    }

    /// Collect finished poster jobs and upload their frames as textures.
    /// Called once per frame before the panels render.
    fn poll_video_jobs(&mut self, ctx: &egui::Context) {
        let mut i = 0;
        while i < self.video_jobs.len() {
            let done = match self.video_jobs[i].rx.try_recv() {
                Ok(res) => Some(res),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                // Worker died without a result (should not happen).
                Err(_) => Some(None),
            };
            let Some(res) = done else {
                i += 1;
                continue;
            };
            let job = self.video_jobs.remove(i);
            let Some((w, h, rgba)) = res else {
                // Decode failed (or the worker died): remember it so the pane
                // stops asking for another poster.
                self.video_poster_failed.insert(job.path.clone());
                continue;
            };
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            let name = match job.kind {
                VideoJobKind::Viewer => format!("video:poster:{}", job.path.display()),
                VideoJobKind::Side => format!("video:side:{}", job.path.display()),
                VideoJobKind::Thumb => format!("video:thumb:{}", job.path.display()),
            };
            let tex = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
            match job.kind {
                VideoJobKind::Viewer => {
                    // Only if the viewer still shows this clip.
                    if self.viewer.as_deref() == Some(job.path.as_path()) {
                        self.video_poster = Some(tex);
                    }
                }
                VideoJobKind::Side => {
                    self.side_video_tex = Some((job.path, tex));
                }
                VideoJobKind::Thumb => {
                    self.thumb_video_cache.push((job.path, tex));
                    while self.thumb_video_cache.len() > THUMB_VIDEO_CACHE {
                        // Evicting frees the texture (TextureHandle::drop).
                        let _ = self.thumb_video_cache.remove(0);
                    }
                }
            }
        }
    }

    /// Toggle play/pause for the viewer's current clip. Starts playback on a
    /// fresh clip (or replays after the end of stream), resumes a paused
    /// worker, pauses while playing. No-op when the clip cannot be decoded.
    fn toggle_video_play(&mut self, vpath: &std::path::Path) {
        let has_mf = self.video_poster.is_some() || self.video_playback.is_some();
        if self.video_playing {
            // Pause keeps the worker alive (it just stops reading samples), so
            // 播放 resumes at the current position.
            if let Some(pb) = self.video_playback.as_ref() {
                pb.set_paused(true);
            }
            self.video_playing = false;
            // User paused: don't keep playing on the next file.
            self.viewer_play_on = false;
        } else if has_mf {
            match self.video_playback.as_ref() {
                // Paused mid-clip: resume where we left off.
                Some(pb) if !self.video_eos => {
                    pb.set_paused(false);
                    self.video_playing = true;
                    self.viewer_play_on = true;
                }
                // Fresh clip, or replay after the end of stream.
                _ => {
                    if let Some(pb) = crate::video::start_playback(vpath, PLAY_MAX_DIM) {
                        self.video_playback = Some(pb);
                        self.video_playing = true;
                        self.video_eos = false;
                        self.video_pos_ns = 0;
                        self.viewer_play_on = true;
                    }
                }
            }
        }
    }

    /// Render the video viewer: poster frame, play/pause, a seek bar with
    /// position / duration at the bottom, and the live playback surface.
    /// Frames are decoded on a background thread and uploaded as egui
    /// textures — the same path the image viewer uses.
    /// Play / pause button and decode status for the clip in the viewer.
    /// Rendered inside the shared [`Self::viewer_toolbar`].
    fn video_play_controls(&mut self, ui: &mut egui::Ui, vpath: &std::path::Path) {
        let has_mf = self.video_poster.is_some() || self.video_playback.is_some();
        let label = if self.video_playing {
            crate::lang::tr("暂停", self.lang)
        } else {
            crate::lang::tr("播放", self.lang)
        };
        if ui
            .add_enabled(has_mf || self.video_playing, egui::Button::new(label))
            .clicked()
        {
            self.toggle_video_play(vpath);
        }
        // Sound: a muted/unmuted toggle, persisted. Default is muted.
        let sound_label = if self.viewer_muted {
            crate::lang::tr("取消静音", self.lang)
        } else {
            crate::lang::tr("静音", self.lang)
        };
        if ui.button(sound_label).clicked() {
            self.viewer_muted = !self.viewer_muted;
            self.persist_viewer_opts();
        }
        if !has_mf && self.video_jobs.is_empty() {
            weak(
                ui,
                crate::lang::tr(
                    "无法解码此视频（格式不受 Media Foundation 支持，如 MKV/WebM/FLV）",
                    self.lang,
                ),
            );
        } else if !has_mf {
            weak(ui, crate::lang::tr("正在准备视频…", self.lang));
        }
    }

    /// The clip surface (poster while idle, live frames while playing) plus the
    /// seek bar below it. The surface reserves its rect, so the bars that
    /// follow land at the bottom instead of overlapping the picture.
    fn render_video_surface(&mut self, ui: &mut egui::Ui, vpath: &std::path::Path, n_imgs: usize) {
        // The seek bar lives at the bottom of the viewer: reserve its height
        // before laying out the video surface so the picture never overlaps it.
        let bar_h = if self.video_playback.is_some() || self.video_eos {
            26.0
        } else {
            0.0
        };

        // The caller draws the filmstrip after this: keep its band clear too.
        let film_h = if n_imgs > 1 { 84.0 } else { 0.0 };
        let area_w = ui.available_width().max(40.0);
        let area_h = (ui.available_height() - bar_h - film_h).max(60.0);
        let area = egui::Rect::from_min_size(ui.cursor().min, egui::vec2(area_w, area_h));

        // Drain the playback worker into locals first (the borrow of
        // `video_playback` must end before the rest of `self` is touched).
        let mut info_dur: Option<u64> = None;
        let mut latest: Option<(u32, u32, Vec<u8>)> = None;
        let mut latest_ts: Option<u64> = None;
        let mut eos = false;
        // End of stream: step to the next file, or start over with no gap.
        let mut pending_next = false;
        let mut pending_restart = false;
        if let Some(pb) = self.video_playback.as_mut() {
            loop {
                match pb.rx.try_recv() {
                    Ok(crate::video::PbMsg::Info { dur_ns }) => info_dur = Some(dur_ns),
                    Ok(crate::video::PbMsg::Frame { ts, w, h, rgba }) => {
                        latest_ts = Some(ts.max(0) as u64);
                        latest = Some((w, h, rgba));
                    }
                    Ok(crate::video::PbMsg::Eos) => {
                        eos = true;
                        break;
                    }
                    Err(_) => break,
                }
            }
        }
        if let Some(d) = info_dur {
            self.video_dur_ns = Some(d);
        }
        // Adopt the worker's latest frame timestamp for the playhead — but
        // while a seek is pinned we hold the playhead at the target until the
        // worker actually emits a frame there, so the bar never snaps back to
        // a stale in-flight frame.
        if let Some(t) = latest_ts {
            if let Some(frac) = self.video_seek_frac {
                let target = self
                    .video_dur_ns
                    .map(|d| (frac as f64 * d as f64))
                    .unwrap_or(0.0);
                let eps = (self.video_dur_ns.unwrap_or(0) as f64) * 0.02;
                if (t as f64) >= target - eps {
                    self.video_seek_frac = None;
                    self.video_pos_ns = t;
                }
                // else: keep video_pos_ns pinned at the target (set on seek)
            } else {
                self.video_pos_ns = t;
            }
        }
        if eos {
            // Keep the last frame on screen; 播放 replays from the start.
            self.video_eos = true;
            self.video_playing = false;
            self.video_seek_frac = None;
            if let Some(d) = self.video_dur_ns {
                self.video_pos_ns = d;
            }
            // What happens next is decided once the borrow above has ended.
            if self.viewer_auto_next && n_imgs > 1 {
                pending_next = true;
            } else if self.viewer_loop {
                pending_restart = true;
            }
        }
        // 自动下一页：进入视频即自动开播，播完由上面的 EOS 分支翻到下一张。
        // 也支持「用户点过播放」：只要 viewer_play_on 为真（手动点过播放，或
        // 自动下一页触发），切到新视频就自动接着播。仅在解码就绪（海报已出）
        // 后触发；首帧海报未到时不动作，下一帧再判。
        if (self.viewer_auto_next || self.viewer_play_on)
            && !self.video_playing
            && !self.video_eos
        {
            let has_mf = self.video_poster.is_some() || self.video_playback.is_some();
            if has_mf {
                self.toggle_video_play(vpath);
            }
        }
        if let Some((w, h, rgba)) = latest {
            let img = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
            if self.video_frame.is_none() {
                let tex = ui.ctx().load_texture(
                    format!("video:frame:{}", vpath.display()),
                    img,
                    egui::TextureOptions::LINEAR,
                );
                self.video_frame = Some(tex);
            } else if let Some(tex) = self.video_frame.as_mut() {
                tex.set(img, egui::TextureOptions::LINEAR);
            }
        }

        // Live frame while a worker exists (playing, paused or ended); the
        // poster before the first play.
        let mut surface_clicked = false;
        let tex = self.video_frame.as_ref().or(self.video_poster.as_ref());
        match tex {
            Some(tex) => {
                let (tw, th) = (tex.size()[0] as f32, tex.size()[1] as f32);
                let ar = (tw / th).max(0.01);
                let mut w = area_w.min(area_h * ar);
                let mut h = w / ar;
                if h > area_h {
                    h = area_h;
                    w = h * ar;
                }
                let dest = egui::Rect::from_center_size(area.center(), egui::vec2(w, h));
                // One click anywhere on the surface toggles play/pause. The ▶
                // badge in the middle is only painted — this rect is what
                // actually makes the whole picture clickable.
                let hot = ui.interact(
                    area,
                    egui::Id::new("video_surface"),
                    egui::Sense::click(),
                );
                surface_clicked = hot.clicked();
                ui.painter().image(
                    tex.id(),
                    dest,
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    egui::Color32::WHITE,
                );
                if !self.video_playing {
                    // ▶ overlay hint in the centre.
                    let r = egui::Rect::from_center_size(area.center(), egui::vec2(64.0, 64.0));
                    ui.painter().circle_filled(
                        r.center(),
                        32.0,
                        egui::Color32::from_black_alpha(120),
                    );
                    ui.painter().text(
                        area.center() + egui::vec2(2.0, 0.0),
                        egui::Align2::CENTER_CENTER,
                        "▶",
                        egui::FontId::proportional(34.0),
                        egui::Color32::WHITE,
                    );
                }
            }
            None => {
                ui.painter().text(
                    area.center(),
                    egui::Align2::CENTER_CENTER,
                    crate::lang::tr("无法解码此视频（格式不受 Media Foundation 支持，如 MKV/WebM/FLV）", self.lang),
                    egui::FontId::proportional(15.0),
                    self.weak_color(),
                );
            }
        }
        if surface_clicked {
            self.toggle_video_play(vpath);
        }
        // Same prev / next pills the image viewer shows, so a clip can be
        // stepped through without hunting for the toolbar buttons.
        self.viewer_edge_arrows(ui, area, vpath, n_imgs);
        // The surface is painted straight onto the painter and consumes no
        // layout space: advance past it so the seek bar (and the filmstrip the
        // caller adds) land below the picture instead of on top of it.
        ui.advance_cursor_after_rect(area);

        // ---- bottom: seek bar (position / duration) ----
        // Shown once playback has started: the duration arrives from the
        // worker as its first message.
        let mut want_seek: Option<f32> = None;
        if self.video_playback.is_some() || self.video_eos {
            ui.horizontal(|ui| {
                weak(ui, crate::video::fmt_time(self.video_pos_ns));
                let bar_w = (ui.available_width() - 78.0).max(60.0);
                let (rect, resp) = ui
                    .allocate_exact_size(egui::vec2(bar_w, 16.0), egui::Sense::click_and_drag());
                let frac = match self.video_dur_ns {
                    Some(d) if d > 0 => (self.video_pos_ns as f64 / d as f64).clamp(0.0, 1.0) as f32,
                    _ => 0.0,
                };
                let shown = self.video_seek_frac.unwrap_or(frac);
                let painter = ui.painter();
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::same(8),
                    ui.visuals().widgets.inactive.bg_fill,
                );
                if shown > 0.0 {
                    let fill =
                        egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * shown, rect.height()));
                    painter.rect_filled(fill, egui::CornerRadius::same(8), ui.visuals().selection.bg_fill);
                }
                let kx = rect.min.x + rect.width() * shown;
                painter.circle_filled(
                    egui::pos2(kx, rect.center().y),
                    6.0,
                    ui.visuals().selection.bg_fill,
                );
                if (resp.drag_started() || resp.dragged() || resp.clicked())
                    && rect.width() > 0.0
                {
                    if let Some(p) = resp.interact_pointer_pos() {
                        let f = ((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0);
                        self.video_seek_frac = Some(f);
                    }
                }
                if resp.drag_stopped() || resp.clicked() {
                    // Keep the fraction set (don't take it) so the bar stays
                    // pinned on the target until the worker confirms the seek.
                    let f = self.video_seek_frac;
                    if let (Some(f), Some(d)) = (f, self.video_dur_ns) {
                        if d > 0 {
                            want_seek = Some(f);
                        }
                    }
                }
                weak(
                    ui,
                    match self.video_dur_ns {
                        Some(d) => crate::video::fmt_time(d),
                        None => "--:--".to_string(),
                    },
                );
            });
        }
        // A committed drag: jump the worker, and resume if it was paused.
        if let Some(f) = want_seek {
            if let (Some(pb), Some(d)) = (self.video_playback.as_ref(), self.video_dur_ns) {
                let target = (f as f64 * d as f64) as u64;
                pb.seek(target);
                self.video_pos_ns = target;
                // Pin the playhead on the target until the worker emits a frame
                // there; see the latest_ts handling above.
                self.video_seek_frac = Some(f);
                self.video_eos = false;
                if !self.video_playing {
                    pb.set_paused(false);
                    self.video_playing = true;
                }
            }
        }
        // ---- end of stream: auto-next wins, otherwise loop with no gap ----
        if pending_next {
            self.viewer_nav(1);
        } else if pending_restart {
            // Restarted in the same frame the old stream ended, so there is no
            // black frame between the last and the first.
            self.toggle_video_play(vpath);
        }
    }


    /// Draw the +/-5 thumbnails of the folder around the current image, in a
    /// single horizontal row. Called inside either a centred `ui.horizontal`
    /// or a sideways `ScrollArea`, so it just lays out 64x64 cells left to
    /// right and does not manage its own scroll.
    fn draw_film_thumbs(&mut self, ui: &mut egui::Ui, n_imgs: usize) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let accent = ui.visuals().selection.bg_fill;
        for o in -5..=5i32 {
        let i = self.viewer_index as i32 + o;
        if i < 0 || i as usize >= n_imgs {
        continue;
        }
        let i = i as usize;
        // Owned clone: the filmstrip spawns decode jobs for clips, which needs
        // `&mut self` while the path is still in use below.
        let p = self.viewer_list[i].clone();
        let is_cur = i == self.viewer_index;
        let thumb_rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(64.0, 64.0));
        let resp = ui.allocate_rect(thumb_rect, egui::Sense::click());
        let inner = resp.rect.shrink(4.0);
        // Images come from the thumbnail cache; clips have no image thumbnail,
        // so fall back to a decoded poster (spawning its job when missing).
        let cell: Option<(egui::TextureId, f32)> = self
        .previews
        .get(&p, preview::THUMB, preview::Priority::Film)
        .map(|t| (t.tex.id(), t.aspect().max(0.01)))
        .or_else(|| {
        self.thumb_video_cache
        .iter()
        .find(|(cp, _)| *cp == p)
        .map(|(_, tex)| {
        let s = tex.size();
        (tex.id(), s[0] as f32 / s[1].max(1) as f32)
        })
        });
        if let Some((tid, ar)) = cell {
        let mut w = inner.width();
        let mut h = w / ar;
        if h > inner.height() {
        h = inner.height();
        w = h * ar;
        }
        let dest = egui::Rect::from_center_size(inner.center(), egui::vec2(w, h));
        ui.painter().image(
        tid,
        dest,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
        );
        } else {
        let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
        if !self.video_poster_failed.contains(&p) && crate::preview::is_video(&name, &p) {
        self.spawn_video_job(&p, VideoJobKind::Thumb);
        }
        ui.painter().text(
        inner.center(),
        egui::Align2::CENTER_CENTER,
        "…",
        egui::FontId::proportional(20.0),
        self.weak_color(),
        );
        }
        if is_cur {
        ui.painter().rect_stroke(
        resp.rect,
        egui::CornerRadius { nw: 3, ne: 3, se: 3, sw: 3 },
        egui::Stroke::new(2.0_f32, accent),
        egui::StrokeKind::Outside,
        );
        }
        if resp.clicked() {
        self.viewer_index = i;
        self.viewer = Some(p.clone());
        self.viewer_zoom = 0.0;
        self.viewer_pan = egui::Vec2::ZERO;
        }
        }
    }

    fn render_convert_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut cv) = self.convert.take() else {
            return;
        };
        let mut open = true;
        let fname = cv
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let title = match cv.mode {
            convert::ConvertMode::Ico => crate::lang::tr("转换为 ICO", self.lang),
            convert::ConvertMode::Png => crate::lang::tr("转换为 PNG", self.lang),
        };
        let resp = egui::Window::new(format!("{title} - {fname}"))
            .open(&mut open)
            .default_size([470.0, 660.0])
            .resizable(false)
            .show(ctx, |ui| self.render_convert_body(ui, &mut cv));
        // `false` when the body closed the dialog (取消); the window's own
        // close button sets `open = false` instead. (`Window::show` wraps
        // the closure result in `Option`: it is `None` when the window did
        // not render at all.)
        let keep: bool = resp.and_then(|r| r.inner).unwrap_or(false);
        if open && keep {
            self.convert = Some(cv);
        }
    }

    fn render_convert_body(&mut self, ui: &mut egui::Ui, cv: &mut convert::ConvertState) -> bool {
        let lang = self.lang;
        let (iw, ih) = cv
            .img
            .as_ref()
            .map(|i| (i.width(), i.height()))
            .unwrap_or((0, 0));
        weak(
            ui,
            format!("{} {iw}×{ih}", crate::lang::tr("图片尺寸", lang)),
        );

        // ---- preview box: the image plus a square-crop drag ----
        let box_h = 250.0;
        let (brect, _) = ui.allocate_exact_size(
            egui::vec2(ui.available_width().max(140.0), box_h),
            egui::Sense::hover(),
        );
        ui.painter().rect_filled(
            brect,
            egui::CornerRadius::same(4),
            ui.visuals().extreme_bg_color,
        );
        ui.painter().rect_stroke(
            brect,
            egui::CornerRadius::same(4),
            egui::Stroke::new(1.0_f32, ui.visuals().widgets.noninteractive.bg_stroke.color),
            egui::StrokeKind::Outside,
        );
        if let (Some(tex), Some(_img)) = (cv.tex.as_ref(), cv.img.as_ref()) {
            let inner = brect.shrink(8.0);
            let s = (inner.width() / iw as f32)
                .min(inner.height() / ih as f32)
                .max(0.001);
            let dest = egui::Rect::from_center_size(
                inner.center(),
                egui::vec2(iw as f32 * s, ih as f32 * s),
            );
            ui.painter().image(
                tex.id(),
                dest,
                egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                egui::Color32::WHITE,
            );
            // Drag a square region out of the picture. The region is kept in
            // image pixels, so zooming the window never rescales a crop.
            if iw > 0 && ih > 0 {
                let resp = ui.interact(
                    dest,
                    egui::Id::new("conv_crop"),
                    egui::Sense::click_and_drag(),
                );
                let to_img = |p: egui::Pos2| -> [u32; 2] {
                    let x = ((p.x - dest.min.x) / dest.width() * iw as f32).floor() as i32;
                    let y = ((p.y - dest.min.y) / dest.height() * ih as f32).floor() as i32;
                    [
                        x.clamp(0, iw as i32 - 1) as u32,
                        y.clamp(0, ih as i32 - 1) as u32,
                    ]
                };
                if resp.drag_started() {
                    if let Some(p) = resp.interact_pointer_pos() {
                        cv.drag_from = Some(to_img(p));
                    }
                }
                if let Some(from) = cv.drag_from {
                    if let Some(p) = resp.interact_pointer_pos() {
                        cv.crop = Some(convert::square_between(from, to_img(p), iw, ih));
                    }
                }
                if resp.drag_stopped() {
                    cv.drag_from = None;
                    // A click (not a drag) clears the crop again.
                    if let Some(r) = cv.crop {
                        if r[2] < 4 || r[3] < 4 {
                            cv.crop = None;
                        }
                    }
                }
                if let Some(r) = cv.crop {
                    let sr = egui::Rect::from_min_size(
                        dest.min + egui::vec2(r[0] as f32 * s, r[1] as f32 * s),
                        egui::vec2(r[2] as f32 * s, r[3] as f32 * s),
                    );
                    let dim = egui::Color32::from_black_alpha(120);
                    let p = ui.painter();
                    p.rect_filled(
                        egui::Rect::from_min_max(dest.min, egui::pos2(sr.max.x, sr.min.y)),
                        0.0,
                        dim,
                    );
                    p.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(sr.min.x, sr.max.y), dest.max),
                        0.0,
                        dim,
                    );
                    p.rect_filled(
                        egui::Rect::from_min_max(dest.min, egui::pos2(sr.min.x, dest.max.y)),
                        0.0,
                        dim,
                    );
                    p.rect_filled(
                        egui::Rect::from_min_max(egui::pos2(sr.max.x, dest.min.y), dest.max),
                        0.0,
                        dim,
                    );
                    p.rect_stroke(
                        sr,
                        egui::CornerRadius::same(1),
                        egui::Stroke::new(1.5_f32, egui::Color32::WHITE),
                        egui::StrokeKind::Outside,
                    );
                    p.text(
                        sr.min - egui::vec2(0.0, 3.0),
                        egui::Align2::LEFT_BOTTOM,
                        format!("{}×{}", r[2], r[3]),
                        egui::FontId::proportional(12.0),
                        egui::Color32::WHITE,
                    );
                }
            }
        }
        ui.add_space(2.0);
        ui.horizontal(|ui| {
            let hint = match cv.mode {
                convert::ConvertMode::Ico => {
                    crate::lang::tr("在图片上拖动选方形区域；不选则取中间方形", lang)
                }
                convert::ConvertMode::Png => {
                    crate::lang::tr("在图片上拖动可选裁剪区域；不选则导出整张", lang)
                }
            };
            weak(ui, hint);
            if cv.crop.is_some() && ui.button(crate::lang::tr("重置裁剪", lang)).clicked() {
                cv.crop = None;
            }
        });
        ui.separator();

        // ---- mode-specific controls ----
        match cv.mode {
            convert::ConvertMode::Ico => {
                ui.label(crate::lang::tr("尺寸（多选）", lang));
                ui.horizontal_wrapped(|ui| {
                    for (i, &sz) in convert::ICO_SIZES.iter().enumerate() {
                        let mut on = cv.sizes[i];
                        if ui.checkbox(&mut on, format!("{sz}")).changed() {
                            cv.sizes[i] = on;
                        }
                    }
                });
                weak(
                    ui,
                    crate::lang::tr("所有勾选的尺寸合并为一个 xxx.ico", lang),
                );
            }
            convert::ConvertMode::Png => {
                ui.horizontal(|ui| {
                    ui.label(crate::lang::tr("压缩率", lang));
                    let mut lvl = cv.level as i32;
                    if ui
                        .add(egui::Slider::new(&mut lvl, 0..=9).show_value(true))
                        .changed()
                    {
                        cv.level = lvl.clamp(0, 9) as u8;
                    }
                });
                weak(
                    ui,
                    crate::lang::tr("0=最快/最大，9=最慢/最小（PNG 无损，只影响文件大小）", lang),
                );
            }
        }
        ui.separator();

        // ---- output folder (PNG only: ICO goes next to the source) ----
        if cv.mode == convert::ConvertMode::Png {
            ui.horizontal(|ui| {
                ui.label(crate::lang::tr("导出文件夹", lang));
                let fr = ui.add(egui::TextEdit::singleline(&mut cv.folder).desired_width(200.0));
                self.text_focus |= fr.has_focus();
            });
            weak(
                ui,
                crate::lang::tr("在图片所在文件夹中新建，已存在则直接写入", lang),
            );
        }
        ui.add_space(4.0);

        // ---- buttons + status ----
        let mut keep = true;
        ui.horizontal(|ui| {
            if ui.button(crate::lang::tr("导出", lang)).clicked() {
                cv.run_export(lang);
            }
            if ui.button(crate::lang::tr("取消", lang)).clicked() {
                keep = false;
            }
            if let Some(d) = &cv.out_dir {
                if ui
                    .button(crate::lang::tr("打开输出文件夹", lang))
                    .on_hover_text(crate::lang::tr("在本资源管理器中新建标签页打开", lang))
                    .clicked()
                {
                    let d = d.clone();
                    self.open_dir_in_new_tab(d, ui.ctx());
                }
                if ui
                    .button(crate::lang::tr("打开原生输出文件夹", lang))
                    .on_hover_text(crate::lang::tr("用 Windows 资源管理器打开", lang))
                    .clicked()
                {
                    crate::fs_util::open_native_folder(d);
                }
            }
        });
        if !cv.status.is_empty() {
            let color = if cv.status_err {
                ui.visuals().error_fg_color
            } else {
                ui.visuals().text_color()
            };
            ui.colored_label(color, &cv.status);
        }
        keep
    }

    fn render_panel(&mut self, ui: &mut egui::Ui, is_left: bool) {
        let cwd = get_panel(self, is_left).cwd.clone();
        let in_recycle = get_panel(self, is_left).in_recycle;

        // Whole-pane click sensor: clicking *anywhere* inside a pane — a row,
        // the empty space below the rows, the address row, even the filter
        // box — makes it the pane that receives the keyboard and the toolbar
        // actions. It is checked against the raw pointer position *after* the
        // contents are drawn (see the bottom of this function), not with a
        // `ui.interact` here: egui reports `clicked()` for one widget only,
        // the topmost one, so a pane-wide sensor would swallow every row's
        // own click.
        let pane_rect = ui.available_rect_before_wrap();

        let lang = self.lang;
        // One translator for the whole panel: capturing `lang` by value keeps
        // `self` available for the `&mut self` calls below.
        let t = move |k: &'static str| crate::lang::tr(k, lang);
        let editing = self.addr_edit == Some(is_left);
        let show_hidden = self.show_hidden;
        let dialog_open = self.delete_open
            || self.empty_open
            || self.new_folder_open
            || self.go_open
            || self.rec_info.is_some()
            || self.rename_open
            || self.props.is_some()
            || self.addr_edit.is_some()
            || self.convert.is_some();

        // Header: breadcrumb, or an inline path editor.
        // No "左栏/右栏" tag here on purpose: it sat right next to the
        // breadcrumb and broke double-click-to-edit-the-path on it. Which
        // pane is active is already shown in the bottom status bar.
        let addr_row = ui.horizontal(|ui| {
            if editing {
                let w = (ui.available_width() - 52.0).max(120.0);
                let id = egui::Id::new(("addr_edit", is_left));
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.addr_text)
                        .id(id)
                        .desired_width(w)
                        .font(egui::TextStyle::Body)
                        .hint_text(t("输入路径后按 Enter 跳转")),
                );
                self.text_focus |= resp.has_focus();
                if self.addr_fresh {
                    ui.memory_mut(|m| m.request_focus(id));
                    self.addr_fresh = false;
                }
                // egui 0.31: a *singleline* TextEdit surrenders focus the
                // moment Enter is pressed, so `resp.has_focus()` is already
                // false by the time we look — checking it made Enter do
                // nothing at all. The documented pattern (and the one the
                // rename dialog uses) is `lost_focus() && key_pressed(Enter)`.
                if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                    // Swallow the same key press in the global handler below,
                    // so it does not ALSO open the selected row this frame.
                    self.enter_consumed = true;
                    self.commit_addr(is_left);
                } else if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    self.addr_edit = None;
                }
                if icons::small_button(ui, &icons::NAV, &t("跳转")).clicked() {
                    self.commit_addr(is_left);
                }
            } else {
                if in_recycle {
                    ui.label(format!(" · {}（{}）", cwd.display(), t("回收站")));
                } else {
                    let segs = path_segments(&cwd);
                    for (i, (label, prefix)) in segs.iter().enumerate() {
                        // NB: use `link` (plain clickable text), NOT `hyperlink`.
                        // `Ui::hyperlink(url)` treats its argument as a URL and
                        // launches the browser on click — that's why clicking the
                        // address used to open a web page instead of navigating.
                        if ui.link(label.as_str()).clicked() {
                            self.go_breadcrumb(prefix.clone(), is_left);
                        }
                        if i + 1 < segs.len() {
                            ui.label(" / ");
                        }
                    }
                }
                // The leftover strip of the address row is a double-click
                // target for typing a path (Explorer does the same).
                let rest = ui.available_rect_before_wrap();
                if rest.width() > 8.0 {
                    let r = ui.interact(
                        rest,
                        egui::Id::new(("addr_blank", is_left)),
                        egui::Sense::click(),
                    );
                    if r.double_clicked() {
                        self.begin_addr_edit(is_left);
                    }
                    r.on_hover_text(t("双击可直接输入路径（回车跳转）"));
                }
            }
        });
        let addr_row_rect = addr_row.response.rect;
        ui.separator();

        // Capacity banner when the panel is sitting on a drive root.
        if !in_recycle {
            if let Some(d) = self.drives.iter().find(|d| {
                cwd.to_string_lossy()
                    .eq_ignore_ascii_case(&format!("{}:\\", d.letter))
            }) {
                if d.total > 0 {
                    let pct = (d.used_frac() * 100.0) as u32;
                    ui.horizontal(|ui| {
                        ui.strong(format!("{}: {}", d.letter, crate::lang::tr("磁盘容量", self.lang)));
                        ui.add(
                            egui::ProgressBar::new(d.used_frac())
                                .desired_width(180.0)
                                .desired_height(10.0)
                                .text(format!("{} {}%", crate::lang::tr("已用", self.lang), pct)),
                        );
                        ui.label(format!(
                            "{}{}/{}{}/{}{}",
                            crate::lang::tr("共", self.lang),
                            human_size(d.total),
                            crate::lang::tr("已用", self.lang),
                            human_size(d.used()),
                            crate::lang::tr("剩", self.lang),
                            human_size(d.free)
                        ));
                    });
                    ui.separator();
                }
            }
        }

        // Filter row + per-pane view switcher + address editor
        let filter_txt = get_panel(self, is_left).filter.clone();
        let filter_rect = ui
            .horizontal(|ui| {
                // Read the label first: the TextEdit below holds a `&mut self` borrow.
                let hint = t("过滤文件名…");
                let tedit = egui::TextEdit::singleline(&mut get_panel(self, is_left).filter)
                    .desired_width(160.0)
                    .hint_text(hint);
                let resp = ui.add(tedit);
                self.text_focus |= resp.has_focus();
                let fr = resp.rect;
                let view = get_panel(self, is_left).view;
                if icons::toggle_button(ui, &icons::LIST, view == View::List, &t("列表")).clicked()
                    && view != View::List
                {
                    self.set_view(is_left, View::List);
                }
                if icons::toggle_button(ui, &icons::GRID, view == View::Grid, &t("缩略图")).clicked()
                    && view != View::Grid
                {
                    self.set_view(is_left, View::Grid);
                }
                if icons::small_button(ui, &icons::PENCIL, &t("编辑地址")).clicked() {
                    self.begin_addr_edit(is_left);
                }
                fr
            })
            .inner;

        // Per-pane status strip: which pane this is (highlighted when it owns
        // the keyboard), how many entries pass the filter, and what is
        // selected right now.
        //
        // It is drawn as its own band — filled background, a hairline on top and
        // a few pixels of clear space above it — so it reads as pane chrome
        // instead of sitting *on* the file listing. The space is reserved here
        // (by shortening the cursor) because the listing is a `ScrollArea`
        // that claims every remaining pixel of the pane; the band itself is
        // painted at the bottom of this function, so it lands on top and the
        // two can never overlap.
        let strip_h = 24.0f32;
        let strip_gap = 5.0f32;
        let strip_rect = egui::Rect::from_min_size(
            egui::pos2(
                pane_rect.min.x,
                (pane_rect.max.y - strip_h).max(pane_rect.min.y),
            ),
            egui::vec2(pane_rect.width(), strip_h),
        );
        // The listing gets the rect between the header rows and the band.
        // Its top is *the current cursor*, not `pane_rect.min`: the address
        // row, the capacity banner and the filter row have already been laid
        // out above, and starting the listing at the pane's top edge would
        // draw it over them (the "bottom is fixed, top overflows" bug).
        //
        // Letting it take "whatever is left" does not work either: the
        // listing is a `ScrollArea`, which claims every remaining pixel of
        // the pane, so the band would be pushed off-screen or drawn over the
        // last rows.
        let list_top = ui.cursor().min.y;
        let list_rect = egui::Rect::from_min_max(
            egui::pos2(pane_rect.min.x, list_top),
            egui::pos2(
                pane_rect.max.x,
                (strip_rect.min.y - strip_gap).max(list_top),
            ),
        );
        {
            // NB: a *vertical* child. `ui.scope` would inherit the pane's
            // layout and lay every section out on a single row.
            let mut list_ui = ui.new_child(
                egui::UiBuilder::new()
                    .id_salt(("pane_list", is_left))
                    .max_rect(list_rect)
                    .layout(egui::Layout::top_down(egui::Align::Min)),
            );
            list_ui.set_clip_rect(list_rect);
            match get_panel(self, is_left).view {
                View::List => self.render_list(&mut list_ui, is_left),
                View::Grid => self.render_grid(&mut list_ui, is_left),
            }
        }

        // The band itself, painted last: opaque so a row's highlight cannot
        // bleed through it, with a hairline separating it from the listing.
        let strip_fill = ui.visuals().extreme_bg_color;
        let strip_line = ui.visuals().widgets.noninteractive.bg_stroke;
        ui.painter()
            .rect_filled(strip_rect, egui::CornerRadius::ZERO, strip_fill);
        ui.painter().hline(strip_rect.x_range(), strip_rect.min.y, strip_line);
        let mut s_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("pane_status", is_left))
                .max_rect(strip_rect.shrink2(egui::vec2(6.0, 0.0)))
                .layout(egui::Layout::left_to_right(egui::Align::Center)),
        );
        s_ui.set_clip_rect(strip_rect.shrink2(egui::vec2(6.0, 0.0)));
        self.draw_pane_status(&mut s_ui, is_left, show_hidden, &filter_txt, &t);

        // …and the whole-pane activation promised at the top: a click released
        // anywhere inside the pane's own rectangle activates it, whatever (or
        // nothing) it landed on. It also closes an open address editor unless
        // the click landed on the editor row itself.
        if ui.input(|i| i.pointer.primary_clicked()) {
            let pos = ui.input(|i| i.pointer.interact_pos());
            if let Some(p) = pos {
                if pane_rect.contains(p) {
                    self.set_active_pane(is_left);
                    // `editing` was captured before the contents were drawn, so
                    // the pencil button that *opens* the editor this very frame
                    // isn't immediately undone by its own click.
                    if editing && !addr_row_rect.contains(p) {
                        self.addr_edit = None;
                    }
                    // Clicking the listing hands the keyboard back to the
                    // pane. Without this, a pane-wide text field (the filter
                    // box) keeps egui's focus after the user typed in it, and
                    // the global shortcuts below are skipped because a text
                    // edit owns the keyboard — Ctrl+C / Ctrl+V then look
                    // completely dead.
                    if !dialog_open && !addr_row_rect.contains(p) && !filter_rect.contains(p) {
                        if let Some(id) = ui.memory(|m| m.focused()) {
                            ui.memory_mut(|m| m.surrender_focus(id));
                        }
                    }
                }
            }
        }
    }

    /// The little status band at the bottom-left of a pane: 左栏/右栏 tag
    /// (bright when this pane owns the keyboard), the **last action** (the
    /// only feedback a Ctrl+C / Ctrl+V gets), the pending clipboard, the
    /// entry count and the active filter.
    ///
    /// Text goes straight onto the painter (see [`cell_text`]) so the band is
    /// purely decorative — a `Label` would become a clickable widget and steal
    /// clicks from the listing underneath.
    fn draw_pane_status(
        &mut self,
        ui: &mut egui::Ui,
        is_left: bool,
        show_hidden: bool,
        filter_txt: &str,
        t: &impl Fn(&'static str) -> &'static str,
    ) {
        let max = ui.max_rect();
        let mut x = max.min.x;
        let cy = max.center().y;
        let weak = self.weak_color();

        // 1) pane tag — only meaningful while split.
        let is_active_pane = !self.split
            || ((is_left && self.active_pane == 0) || (!is_left && self.active_pane == 1));
        // Read everything we need from `self` *before* borrowing the panel:
        // `get_panel` hands out a `&mut` to the whole struct.
        let last_action: Option<String> = if is_active_pane && !self.status.is_empty() {
            Some(if self.status_fresh {
                format!("● {}", self.status)
            } else {
                self.status.clone()
            })
        } else {
            None
        };
        let clip_txt: Option<String> = match &self.clip {
            Some(Clip::Copy(v)) => Some(format!(
                "{}: {} {} ({})",
                t("剪贴板"),
                v.len(),
                t("项"),
                t("复制")
            )),
            Some(Clip::Move(v)) => Some(format!(
                "{}: {} {} ({})",
                t("剪贴板"),
                v.len(),
                t("项"),
                t("剪切")
            )),
            None => None,
        };
        if self.split {
            let tag = t(if is_left { "左栏" } else { "右栏" });
            let color = if is_active_pane {
                ui.visuals().selection.bg_fill
            } else {
                weak
            };
            let font = egui::TextStyle::Body.resolve(ui.style());
            let galley = ui.painter().layout_no_wrap(tag.to_string(), font, color);
            let w = galley.size().x;
            let r = egui::Rect::from_min_size(
                egui::pos2(x, cy - galley.size().y * 0.5),
                galley.size(),
            );
            ui.painter().galley(r.min, galley, color);
            x += w + 6.0;
            // hairline after the tag
            ui.painter().vline(
                x,
                (max.min.y + 3.0)..=(max.max.y - 3.0),
                ui.visuals().widgets.noninteractive.bg_stroke,
            );
            x += 8.0;
        }

        // 2) the last action. Copy / paste / delete leave no other trace, so
        // this is the pane's way of confirming that the keypress was seen.
        // Fresh lines go brighter for a moment (`status_fresh`).
        if let Some(text) = last_action {
            let color = if self.status_fresh {
                ui.visuals().strong_text_color()
            } else {
                weak
            };
            x = status_cell(ui, x, cy, max.max.x, text, color);
        }

        // 3) counts + clipboard + filter hint, each in its own painted cell.
        let p = get_panel(self, is_left);
        let n_vis = if p.in_recycle {
            p.recycle_items.len()
        } else {
            p.visible(show_hidden).len()
        };
        let n_sel = p.multi.len();
        let mut parts: Vec<String> = Vec::new();
        parts.push(if n_sel > 1 {
            format!("{} / {} {}", n_sel, n_vis, t("项"))
        } else {
            format!("{} {}", n_vis, t("项"))
        });
        if let Some(c) = clip_txt {
            parts.push(c);
        }
        if !filter_txt.is_empty() {
            parts.push(format!("{}: {}", t("过滤"), filter_txt));
        }
        if p.in_recycle {
            parts.push(t("回收站").to_string());
        }
        for part in parts {
            let font = egui::TextStyle::Body.resolve(ui.style());
            let galley = ui.painter().layout_no_wrap(part.clone(), font, weak);
            let w = galley.size().x;
            if x + w > max.max.x {
                break; // no room left: drop the rest rather than overlap
            }
            let r = egui::Rect::from_min_size(
                egui::pos2(x, cy - galley.size().y * 0.5),
                galley.size(),
            );
            ui.painter().galley(r.min, galley, weak);
            x += w + 10.0;
        }
    }

    /// Browser-style tab strip. The tab list is persisted, so the next launch
    /// reopens exactly the tabs the user left open.
    fn render_tab_strip(&mut self, ui: &mut egui::Ui) {
        let snap: Vec<(usize, u32, PathBuf)> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(i, t)| (i, t.id, t.panes[0].cwd.clone()))
            .collect();
        let count = snap.len();
        ui.horizontal(|ui| {
            ui.set_min_height(26.0);
            for (idx, _id, cwd) in snap {
                let selected = idx == self.active_tab;
                let label = tab_label(&cwd);
                let hover = cwd.to_string_lossy().to_string();
                // One frame per tab: the name and the × closer sit inside a
                // single rounded background, so the two read as one unit
                // (browser tabs work the same way). Everything is placed by
                // hand — a `selectable_label` plus a separate button would
                // each paint their own background and look like two unrelated
                // widgets.
                let text_color = if selected {
                    ui.visuals().strong_text_color()
                } else {
                    ui.visuals().text_color()
                };
                let font_id = egui::TextStyle::Button.resolve(ui.style());
                let galley = ui.painter().layout_no_wrap(label.clone(), font_id, text_color);
                let btn = ui.spacing().icon_width.max(16.0);
                let pad = 5.0;
                let gap = 4.0;
                let closable = count > 1;
                let w =
                    pad * 2.0 + galley.size().x + if closable { gap + btn } else { 0.0 };
                let h = (btn + 6.0).max(22.0);
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
                let resp = resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_text(hover);
                let bg = if selected {
                    ui.visuals().selection.bg_fill
                } else if resp.hovered() {
                    ui.visuals().widgets.hovered.weak_bg_fill
                } else {
                    egui::Color32::TRANSPARENT
                };
                if ui.is_rect_visible(rect) {
                    if bg != egui::Color32::TRANSPARENT {
                        ui.painter()
                            .rect_filled(rect, egui::CornerRadius::same(4), bg);
                    }
                    ui.painter().galley(
                        egui::pos2(rect.min.x + pad, rect.center().y - galley.size().y * 0.5),
                        galley,
                        text_color,
                    );
                }
                if closable {
                    // Registered *after* the tab rect on purpose: egui breaks
                    // pointer-hit ties by registration order ("in tie, pick
                    // last = topmost" — hit_test.rs), so a click on the ×
                    // never also activates the tab.
                    let x_rect = egui::Rect::from_center_size(
                        egui::pos2(rect.max.x - pad - btn * 0.5, rect.center().y),
                        egui::vec2(btn, btn),
                    );
                    let x_resp = ui
                        .interact(
                            x_rect,
                            egui::Id::new(("tab_close", idx)),
                            egui::Sense::click(),
                        )
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .on_hover_text(crate::lang::tr("关闭标签页", self.lang));
                    let v = ui.style().visuals.widgets.style(&x_resp);
                    if ui.is_rect_visible(x_rect) && v.weak_bg_fill != egui::Color32::TRANSPARENT {
                        ui.painter().rect_filled(x_rect, v.corner_radius, v.weak_bg_fill);
                    }
                    icons::paint(ui.painter(), icons::X.0, x_rect, v.text_color(), 2.0);
                    if x_resp.clicked() {
                        self.close_tab(idx);
                        break;
                    }
                }
                if resp.clicked() {
                    self.active_tab = idx;
                    // Keep whichever pane the user had selected: the active
                    // pane survives tab switches, so addresses keep opening
                    // in it.
                    self.persist_layout();
                }
                // Right-click a tab: open its folder in the right-hand pane,
                // plus the usual close family. `menu_stop` leaves the loop
                // after a close: `snap` is stale once tabs are removed.
                let mut menu_stop = false;
                let _ = resp.context_menu(|ui| {
                    if ui
                        .button(crate::lang::tr("从右栏打开", self.lang))
                        .clicked()
                    {
                        self.open_tab_in_right_pane(idx);
                        ui.close_menu();
                    }
                    ui.separator();
                    // Favourite / unfavourite the tab's folder, the same
                    // affordance as the folder menu and the toolbar button.
                    self.favorite_menu_item(ui, &cwd);
                    if ui
                        .button(crate::lang::tr("关闭标签页", self.lang))
                        .clicked()
                    {
                        self.close_tab(idx);
                        menu_stop = true;
                        ui.close_menu();
                    }
                    if count > 1
                        && ui
                            .button(crate::lang::tr("关闭其他标签页", self.lang))
                            .clicked()
                    {
                        self.close_other_tabs(idx);
                        menu_stop = true;
                        ui.close_menu();
                    }
                    if idx + 1 < count
                        && ui
                            .button(crate::lang::tr("关闭右侧标签页", self.lang))
                            .clicked()
                    {
                        self.close_tabs_to_right(idx);
                        menu_stop = true;
                        ui.close_menu();
                    }
                });
                if menu_stop {
                    break;
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if icons::button(
                    ui,
                    &icons::PLUS,
                    &crate::lang::tr("标签页", self.lang),
                    true,
                    "Ctrl+T",
                )
                .clicked()
                {
                    self.new_tab();
                }
            });
        });
        ui.separator();
    }

    /// Column widths for the table views: the four left columns are
    /// user-resizable (persisted), the 修改日期 column absorbs the leftover
    /// width so the table always fills the pane exactly — no overflow, no gap.
    fn list_cols(&self, avail_w: f32) -> Cols {
        let name = self.col_name.max(MIN_NAME);
        let ty = self.col_type.max(MIN_TYPE);
        let size = self.col_size.max(MIN_SIZE);
        let time = self.col_time.max(MIN_TIME);
        let date = (avail_w - (name + ty + size + time)).max(MIN_DATE);
        let mut c = Cols { name, ty, size, time, date };
        // The five columns have fixed widths, so on a narrow pane (the preview
        // pane steals 260px, or the window is just small) their sum can exceed
        // the pane itself. The rows live in a ScrollArea that clips at the pane
        // edge, so the last column was pushed out of sight entirely — a blank
        // hole at the pane's top-right — while the header row (not clipped)
        // bled across the split into the neighbouring pane. Scale every column
        // by the same factor instead: the table then always ends exactly at
        // the pane's right edge, and text just truncates a little earlier.
        let total = c.name + c.ty + c.size + c.time + c.date;
        if total > avail_w && avail_w > 0.0 {
            let k = avail_w / total;
            c.name *= k;
            c.ty *= k;
            c.size *= k;
            c.time *= k;
            c.date *= k;
        }
        // 创建时间 column floor: a full "YYYY-MM-DD HH:MM" needs ~128px to be
        // readable, and even the short "MM-DD HH:MM" needs ~92px. On a narrow
        // pane (split view with the preview pane open) borrow the difference
        // from the name column — a truncated name is still recognisable, a
        // truncated timestamp is not.
        let date_floor = 92.0;
        if c.date < date_floor {
            let borrow = (date_floor - c.date).min((c.name - MIN_NAME).max(0.0));
            c.name -= borrow;
            c.date += borrow;
        }
        c
    }

    fn render_list(&mut self, ui: &mut egui::Ui, is_left: bool) {
        let cols = self.list_cols(ui.available_width());

        // Drop the gap between columns; separation is done by the divider line
        // drawn at each column's right edge (and you can drag that line).
        let old_spacing = ui.style().spacing.item_spacing.x;
        ui.style_mut().spacing.item_spacing.x = 0.0;

        ui.horizontal(|ui| {
            self.sort_header(ui, is_left, SortKey::Name, 0, cols.name, true);
            self.sort_header(ui, is_left, SortKey::Type, 1, cols.ty, true);
            self.sort_header(ui, is_left, SortKey::Size, 2, cols.size, true);
            self.sort_header(ui, is_left, SortKey::Time, 3, cols.time, true);
            self.sort_header(ui, is_left, SortKey::Ctime, 4, cols.date, false);
        });
        ui.separator();

        if get_panel(self, is_left).in_recycle {
            self.render_recycle(ui, is_left, cols);
            ui.style_mut().spacing.item_spacing.x = old_spacing;
            return;
        }

        let show_hidden = self.show_hidden;
        let loading = get_panel(self, is_left).loading;
        let vis = get_panel(self, is_left).visible(show_hidden);
        // Multi-selection membership is read per row via `multi.contains`
        // (below) instead of cloning the whole set every frame — a 10k-item
        // Ctrl+A would otherwise deep-copy 10k strings each repaint.
        // Borrow the listing for the duration of the frame instead of cloning
        // it: a 5000-entry folder would otherwise deep-clone every frame.
        let generation = get_panel(self, is_left).generation;
        // Invalidate the per-row text cache (size / type / dates) when the
        // listing, UI language, or column widths change — all three alter the
        // formatted strings. Keyed so a mid-generation size update (folder
        // scan completing) still refreshes the row via `(index, size, est)`.
        let lang_id = self.lang.num();
        let sig = generation
            ^ (lang_id << 40)
            ^ ((cols.time as u64) << 20)
            ^ (cols.date as u64);
        {
            let p = get_panel(self, is_left);
            if p.fmt_sig != sig {
                p.fmt_cache.clear();
                p.fmt_sig = sig;
            }
        }
        let entries = std::mem::take(&mut get_panel(self, is_left).entries);

        // Icons are resolved per visible row inside the loop (below), not for
        // all `vis.len()` rows up front — a 50k-entry folder must not pay 50k
        // shell/COM lookups every single frame.
        let emoji_mode = self.icons.mode() == shellicon::IconMode::Emoji;

        egui::ScrollArea::vertical()
            .id_salt(if is_left { "filelist_left" } else { "filelist_right" })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Rows sit flush: with no vertical gap the column lines join
                // into one continuous divider per column (like a real table)
                // instead of a dashed stack of 20px stubs.
                ui.spacing_mut().item_spacing.y = 0.0;
                if vis.is_empty() {
                    if loading {
                        weak(ui, crate::lang::tr("加载中…", self.lang));
                    } else {
                        weak(ui, crate::lang::tr("空目录", self.lang));
                    }
                }
                for &i in vis.iter() {
                    // Reserve exactly one row of space for every entry so the
                    // scrollbar length stays correct, but only actually build
                    // the (expensive) Frame + cells + shell icon when the row
                    // is on screen. This is what keeps a 50k-file folder from
                    // freezing the whole window every frame.
                    let (row_rect, _) =
                        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), egui::Sense::hover());
                    if !ui.is_rect_visible(row_rect) {
                        continue;
                    }
                    let mut row_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(row_rect)
                            .layout(egui::Layout::top_down(egui::Align::LEFT)),
                    );
                    let ui = &mut row_ui;
                        let e = &entries[i];
                        let name = e.name.clone();
                        let is_dir = e.is_dir;
                        let is_sel = get_panel(self, is_left).multi.contains(&name);
                        // Icon resolved lazily, only for this visible row.
                        let tex = self
                            .icons
                            .get(&ui.ctx().clone(), shellicon::Kind::for_entry(e.is_dir, &e.ext));
                        // "" unless the emoji mode is on: then every row wears a
                        // font glyph instead of a shell texture.
                        let emoji = if emoji_mode {
                            emoji_for(is_dir, &e.ext)
                        } else {
                            ""
                        };
                        // Per-row text is cached by `(index, size, est)`: re-reading
                        // `entries` on the worker replaces them, and a size result
                        // mid-scan bumps `e.size` so the row refreshes. The cache is
                        // cleared wholesale on nav / language / width change above.
                        let (size_txt, type_txt, mtime_txt, ctime_txt): (&str, &str, &str, &str) = {
                            let key = (i, e.size, e.size_est as u8);
                            let lang = self.lang;
                            let mtime_w = cols.time;
                            let ctime_w = cols.date;
                            let fmt = get_panel(self, is_left)
                                .fmt_cache
                                .entry(key)
                                .or_insert_with(|| {
                                    let size_txt = if is_dir {
                                        if e.size_known {
                                            human_size_est(e.size, e.size_est)
                                        } else {
                                            "…".to_string()
                                        }
                                    } else {
                                        human_size(e.size)
                                    };
                                    let type_txt = e.type_label(lang);
                                    let mtime_txt = col_datetime(e.mtime, mtime_w);
                                    let ctime_txt = col_datetime(e.ctime, ctime_w);
                                    (size_txt, type_txt, mtime_txt, ctime_txt)
                                });
                            (fmt.0.as_str(), fmt.1.as_str(), fmt.2.as_str(), fmt.3.as_str())
                        };
                        let fill = if is_sel {
                            ui.visuals().selection.bg_fill
                        } else {
                            egui::Color32::TRANSPARENT
                        };

                        let row = egui::Frame::NONE.fill(fill).show(ui, |ui| {
                            ui.horizontal(|ui| {
                                name_cell(
                                    ui,
                                    cols.name,
                                    name.clone(),
                                    if is_dir { IconKind::Folder } else { IconKind::File },
                                    tex,
                                    emoji,
                                );
                                table_cell(ui, cols.ty, true, type_txt);
                                table_cell(ui, cols.size, true, size_txt);
                                table_cell(ui, cols.time, true, mtime_txt);
                                table_cell(ui, cols.date, false, ctime_txt);
                            });
                        });
                        let resp = row.response.interact(egui::Sense::click_and_drag());
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                        // Drag the row's file(s) out of the window (to QQ, an
                        // upload box, Explorer, …) via a Windows OLE drag.
                        let mut drag_handled = false;
                        if resp.drag_started() {
                            drag_handled = self.try_start_os_drag(is_left, &name);
                        }

                        // Double-click must win over single-click: test it first,
                        // otherwise every double-click also fires `clicked()` and
                        // only ever selects (the old bug — folders never opened).
                        if !drag_handled && resp.double_clicked() {
                            self.open_row(is_left, &name, is_dir, &entries);
                            ui.ctx().request_repaint();
                        } else if !drag_handled && resp.clicked() {
                            // Explorer-style modifier clicks: Ctrl toggles one row,
                            // Shift extends from the anchor, a plain click starts
                            // a fresh single selection.
                            let (ctrl, shift) =
                                ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
                            let vis_names: Vec<String> =
                                vis.iter().map(|&j| entries[j].name.clone()).collect();
                            self.set_active_pane(is_left);
                            get_panel(self, is_left)
                                .click_select(&name, ctrl, shift, &vis_names);
                            // Clicking a file row must hand the keyboard back to
                            // the pane too (same fix as the background click above):
                            // otherwise the always-present filter box keeps egui
                            // focus and Ctrl+C / Ctrl+V / Delete stay suppressed.
                            if let Some(id) = ui.memory(|m| m.focused()) {
                                ui.memory_mut(|m| m.surrender_focus(id));
                            }
                        }

                        let path = get_panel(self, is_left).cwd.join(&name);
                        let _ = resp.context_menu(|ui| {
                            self.set_selection(is_left, &name);
                            self.file_context_menu(ui, &name, is_dir, &path);
                        });
                }
                self.pane_bg_menu(ui, is_left, None);
            });
        self.restore_entries(is_left, generation, entries);
        ui.style_mut().spacing.item_spacing.x = old_spacing;
    }

    /// Recycle-bin listing: exactly the same five columns as the file list, so
    /// switching between a folder and the bin doesn't change the layout.
    fn render_recycle(&mut self, ui: &mut egui::Ui, is_left: bool, cols: Cols) {
        // Take the list for the duration of the frame instead of cloning it:
        // a bin with thousands of items would otherwise deep-clone every
        // string on every repaint.
        let generation = get_panel(self, is_left).generation;
        let loading = get_panel(self, is_left).rec_loading;
        let rec_items = std::mem::take(&mut get_panel(self, is_left).recycle_items);
        let sel = get_panel(self, is_left).selected.clone();
        let emoji_mode = self.icons.mode() == shellicon::IconMode::Emoji;
        // Native icons resolved up front (the borrow of `self.icons` must not
        // overlap the `&mut self` work done per row).
        let ctx = ui.ctx().clone();
        let texs: Vec<Option<egui::TextureId>> = rec_items
            .iter()
            .map(|it| {
                let ext = std::path::Path::new(&it.name)
                    .extension()
                    .map(|x| x.to_string_lossy().to_ascii_lowercase())
                    .unwrap_or_default();
                self.icons.get(&ctx, shellicon::Kind::for_entry(it.is_dir, &ext))
            })
            .collect();
        egui::ScrollArea::vertical()
            .id_salt(if is_left { "filelist_left" } else { "filelist_right" })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                if rec_items.is_empty() {
                    if loading {
                        weak(ui, crate::lang::tr("回收站加载中…", self.lang));
                    } else {
                        weak(ui, crate::lang::tr("回收站为空", self.lang));
                    }
                }
                for (k, it) in rec_items.iter().enumerate() {
                    let is_sel = sel.as_deref() == Some(it.suffix.as_str());
                    let fill = if is_sel {
                        ui.visuals().selection.bg_fill
                    } else {
                        egui::Color32::TRANSPARENT
                    };
                    let type_txt = if it.is_dir {
                        crate::lang::tr("文件夹", self.lang).to_string()
                    } else {
                        crate::lang::tr("文件", self.lang).to_string()
                    };
                    let size_txt = if it.is_dir {
                        "—".to_string()
                    } else {
                        human_size(it.size)
                    };
                    // Recycle rows carry no creation time; the deletion time
                    // goes in the 修改时间 column, 创建时间 stays blank.
                    let clock = col_datetime(it.deleted_at, cols.time);

                    // Emoji mode: the bin's own glyph for the deleted item's
                    // type, so a deleted photo still reads as a photo.
                    let emoji = if emoji_mode {
                        let ext = std::path::Path::new(&it.name)
                            .extension()
                            .map(|x| x.to_string_lossy().to_ascii_lowercase())
                            .unwrap_or_default();
                        emoji_for(it.is_dir, &ext)
                    } else {
                        ""
                    };
                    let row = egui::Frame::NONE.fill(fill).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            name_cell(
                                ui,
                                cols.name,
                                it.name.clone(),
                                IconKind::Recycle,
                                texs[k],
                                emoji,
                            );
                            table_cell(ui, cols.ty, true, type_txt);
                            table_cell(ui, cols.size, true, size_txt);
                            table_cell(ui, cols.time, true, clock);
                            table_cell(ui, cols.date, false, "—");
                        });
                    });
                    let resp = row.response.interact(egui::Sense::click());
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    // `on_hover_text` consumes `resp`, so re-bind the result
                    // (conditionally) instead of dropping it and losing `resp`.
                    let resp = if !it.orig_path.is_empty() {
                        resp.on_hover_text(&it.orig_path)
                    } else {
                        resp
                    };
                    if resp.double_clicked() {
                        self.rec_info = Some(RecInfo {
                            name: it.name.clone(),
                            orig_path: it.orig_path.clone(),
                            is_dir: it.is_dir,
                            size: it.size,
                            deleted_at: it.deleted_at,
                            suffix: it.suffix.clone(),
                        });
                    } else if resp.clicked() {
                        self.set_selection(is_left, &it.suffix);
                    }
                    // Right-click: 还原 / 彻底删除 / 原位置 / 属性 — the row
                    // becomes the selection first, like the file list does.
                    let _ = resp.context_menu(|ui| {
                        self.set_selection(is_left, &it.suffix);
                        self.rec_context_menu(ui, is_left, it);
                    });
                }
                // Empty space under the rows: 清空回收站 / 还原选中的 / 刷新.
                // The selected item is resolved from the local `rec_items`
                // snapshot: `sel_rec()` filters the panel field, which is
                // `mem::take`n for the duration of this closure and would
                // always come back empty inside a menu callback.
                let sel_item = sel
                    .as_deref()
                    .and_then(|sfx| rec_items.iter().find(|r| r.suffix == *sfx));
                self.pane_bg_menu(ui, is_left, sel_item);
            });
        let p = get_panel(self, is_left);
        if p.generation == generation {
            p.recycle_items = rec_items;
        }
    }

    // ---------------- thumbnail grid ----------------

    fn render_grid(&mut self, ui: &mut egui::Ui, is_left: bool) {
        let show_hidden = self.show_hidden;
        let loading = get_panel(self, is_left).loading;
        let vis = get_panel(self, is_left).visible(show_hidden);
        // Multi-selection membership is read per row (see render_list); no
        // per-frame clone of the whole set.
        // Borrow the listing for the duration of the frame instead of cloning
        // it: a 5000-entry folder would otherwise deep-clone every frame.
        let generation = get_panel(self, is_left).generation;
        // Per-row text cache (grid only renders a size string) cleared on nav /
        // language change — matches the list view's signature scheme minus the
        // width bits, which the grid does not use.
        let lang_id = self.lang.num();
        let sig = generation ^ (lang_id << 40);
        {
            let p = get_panel(self, is_left);
            if p.fmt_sig != sig {
                p.fmt_cache.clear();
                p.fmt_sig = sig;
            }
        }
        let entries = std::mem::take(&mut get_panel(self, is_left).entries);
        let sp = ui.spacing().item_spacing.x;
        let cols = (((ui.available_width() + sp) / (CELL_W + sp)).floor() as usize).max(1);
        // Row height is fixed (`CELL_H`), so off-screen rows are skipped
        // entirely: a huge folder no longer builds 50k cells (and pays 50k
        // shell icon lookups) every frame. `vis_names` is no longer built up
        // front — `grid_cell` assembles it only when a row is actually clicked.
        let row_w = (cols as f32) * (CELL_W + sp) - sp;

        egui::ScrollArea::vertical()
            .id_salt(if is_left { "grid_left" } else { "grid_right" })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = sp;
                if vis.is_empty() {
                    if loading {
                        weak(ui, crate::lang::tr("加载中…", self.lang));
                    } else {
                        weak(ui, crate::lang::tr("空目录", self.lang));
                    }
                }
                let mut start = 0usize;
                while start < vis.len() {
                    let end = (start + cols).min(vis.len());
                    // Reserve the row; skip building its cells when it is
                    // scrolled out of view.
                    let (row_rect, _) =
                        ui.allocate_exact_size(egui::vec2(row_w, CELL_H), egui::Sense::hover());
                    if !ui.is_rect_visible(row_rect) {
                        start = end;
                        continue;
                    }
                    let mut row_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(row_rect)
                            .layout(egui::Layout::top_down(egui::Align::LEFT)),
                    );
                    let ui = &mut row_ui;
                        ui.horizontal(|ui| {
                            for &i in &vis[start..end] {
                                let e = &entries[i];
                                let is_sel = get_panel(self, is_left).multi.contains(&e.name);
                                self.grid_cell(ui, is_left, e, is_sel, &entries, &vis, i);
                            }
                        });
                    start = end;
                }
                self.pane_bg_menu(ui, is_left, None);
            });
        self.restore_entries(is_left, generation, entries);
    }

    fn grid_cell(
        &mut self,
        ui: &mut egui::Ui,
        is_left: bool,
        e: &app::Entry,
        is_sel: bool,
        entries: &[app::Entry],
        vis: &[usize],
        idx: usize,
    ) {
        let name = e.name.clone();
        let is_dir = e.is_dir;
        let path = get_panel(self, is_left).cwd.join(&name);
        let fill = if is_sel {
            ui.visuals().selection.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };
        // Native (full-colour) icon for this cell, at grid size; `None` in both
        // lighter icon modes, which never ask the shell.
        let tex = self
            .icons
            .get(ui.ctx(), shellicon::Kind::for_entry(is_dir, &e.ext));
        let emoji = if self.icons.mode() == shellicon::IconMode::Emoji {
            emoji_for(is_dir, &e.ext)
        } else {
            ""
        };

        let cell = egui::Frame::NONE
            .fill(fill)
            .corner_radius(egui::CornerRadius::same(4))
            .inner_margin(egui::Margin::same(3))
            .show(ui, |ui| {
                ui.allocate_ui_with_layout(
                    egui::vec2(CELL_W - 6.0, CELL_H - 6.0),
                    egui::Layout::top_down(egui::Align::Center),
                    |ui| {
                        let (rect, _) = ui.allocate_exact_size(
                            egui::vec2(CELL_W - 18.0, CELL_H - 46.0),
                            egui::Sense::hover(),
                        );
                        if is_dir {
                            draw_row_icon(
                                ui.painter(),
                                tex,
                                emoji,
                                IconKind::Folder,
                                rect,
                                LARGE_ICON,
                                egui::Color32::from_rgb(225, 180, 70),
                            );
                        } else if crate::preview::is_image(&name, &path) {
                            let mut painted = false;
                            // Only request a thumbnail for on-screen cells; off-
                            // screen cells in a huge folder must not flood the
                            // decode queue (they are lowest priority anyway).
                            // 小 perf mode stops *new* grid decodes (see
                            // Previews::set_allow_grid); cached thumbnails still
                            // show. Off-screen cells never enqueue either way.
                            if ui.is_rect_visible(rect) {
                                if let Some(t) = self.previews.get(&path, preview::THUMB, preview::Priority::Grid) {
                                    let ar = t.aspect().max(0.01);
                                    let mut w = rect.width();
                                    let mut h = w / ar;
                                    if h > rect.height() {
                                        h = rect.height();
                                        w = h * ar;
                                    }
                                    let dest = egui::Rect::from_center_size(
                                        rect.center(),
                                        egui::vec2(w, h),
                                    );
                                    ui.painter().image(
                                        t.tex.id(),
                                        dest,
                                        egui::Rect::from_min_max(
                                            egui::pos2(0.0, 0.0),
                                            egui::pos2(1.0, 1.0),
                                        ),
                                        egui::Color32::WHITE,
                                    );
                                    painted = true;
                                }
                            }
                            if !painted {
                                // Thumbnail still decoding: show the shell's own
                                // icon for this image type instead of a stub.
                                draw_row_icon(
                                    ui.painter(),
                                    tex,
                                    emoji,
                                    IconKind::Image,
                                    rect,
                                    LARGE_ICON,
                                    self.weak_color(),
                                );
                            }
                        } else if crate::preview::is_video(&name, &path) {
                            // Video thumbnail: the decoded first frame, cached
                            // per path. Only visible cells request a decode
                            // (off-screen cells in a huge folder must not flood
                            // the job queue).
                            let mut painted = false;
                            if ui.is_rect_visible(rect) {
                                let cached = self
                                    .thumb_video_cache
                                    .iter()
                                    .find(|(p, _)| p == &path)
                                    .map(|(_, t)| t.clone());
                                match cached {
                                    Some(t) => {
                                        let ar = t.aspect_ratio().max(0.01);
                                        let mut w = rect.width();
                                        let mut h = w / ar;
                                        if h > rect.height() {
                                            h = rect.height();
                                            w = h * ar;
                                        }
                                        let dest = egui::Rect::from_center_size(
                                            rect.center(),
                                            egui::vec2(w, h),
                                        );
                                        ui.painter().image(
                                            t.id(),
                                            dest,
                                            egui::Rect::from_min_max(
                                                egui::pos2(0.0, 0.0),
                                                egui::pos2(1.0, 1.0),
                                            ),
                                            egui::Color32::WHITE,
                                        );
                                        painted = true;
                                    }
                                    None => {
                                        // 小 perf mode skips decoding video
                                        // posters in the grid: the icon stands
                                        // in, and no Media Foundation session is
                                        // opened per cell.
                                        if self.viewer_perf != 0 {
                                            self.spawn_video_job(&path, VideoJobKind::Thumb);
                                        }
                                    }
                                }
                            }
                            if !painted {
                                // Still decoding (or undecodable): the shell's
                                // own video icon stands in.
                                draw_row_icon(
                                    ui.painter(),
                                    tex,
                                    emoji,
                                    IconKind::File,
                                    rect,
                                    LARGE_ICON,
                                    self.weak_color(),
                                );
                            }
                        } else {
                            draw_row_icon(
                                ui.painter(),
                                tex,
                                emoji,
                                IconKind::File,
                                rect,
                                LARGE_ICON,
                                ui.visuals().text_color(),
                            );
                        }
                        // Painter-drawn caption instead of a `Label`: a selectable
                        // label would be an interactive widget stealing the
                        // click/double-click on the thumbnail name.
                        let cap_rect = ui
                            .allocate_exact_size(
                                egui::vec2(CELL_W - 18.0, 40.0),
                                egui::Sense::hover(),
                            )
                            .0;
                        let font_id = egui::TextStyle::Body.resolve(ui.style());
                        let color = ui.visuals().text_color();
                        // Name on the first line, size on the second — folders
                        // show their computed total once the size worker has
                        // it ("…" while it is still being walked). The size
                        // string is cached by `(index, size, est)`.
                        let name_rect =
                            egui::Rect::from_min_size(cap_rect.min, egui::vec2(cap_rect.width(), 20.0));
                        cell_text_with(ui, name_rect, name.clone(), font_id.clone(), color);
                        // Clone the cached string out so the `&mut self` borrow
                        // held by `fmt_cache` ends here; `self.weak_color()`
                        // below then borrows `self` immutably without a clash.
                        let size_txt: String = {
                            let key = (idx, e.size, e.size_est as u8);
                            let fmt = get_panel(self, is_left)
                                .fmt_cache
                                .entry(key)
                                .or_insert_with(|| {
                                    let s = if e.is_dir {
                                        if e.size_known {
                                            human_size_est(e.size, e.size_est)
                                        } else {
                                            "…".to_string()
                                        }
                                    } else {
                                        human_size(e.size)
                                    };
                                    (s, String::new(), String::new(), String::new())
                                });
                            fmt.0.clone()
                        };
                        let size_rect = egui::Rect::from_min_size(
                            cap_rect.min + egui::vec2(0.0, 20.0),
                            egui::vec2(cap_rect.width(), 18.0),
                        );
                        cell_text_with(
                            ui,
                            size_rect,
                            size_txt,
                            font_id,
                            self.weak_color(),
                        );
                    },
                );
            });
        let resp = cell.response.interact(egui::Sense::click_and_drag());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        // Drag the thumbnail's file(s) out of the window (QQ / upload / Explorer).
        let mut drag_handled = false;
        if resp.drag_started() {
            drag_handled = self.try_start_os_drag(is_left, &name);
        }
        if !drag_handled && resp.double_clicked() {
            self.open_row(is_left, &name, is_dir, entries);
        } else if !drag_handled && resp.clicked() {
            // Same modifier rules as the list view (Ctrl / Shift / plain).
            let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
            self.set_active_pane(is_left);
            // Build the visible-name list only now (on click), not every frame.
            let vis_names: Vec<String> =
                vis.iter().map(|&j| entries[j].name.clone()).collect();
            get_panel(self, is_left).click_select(&name, ctrl, shift, &vis_names);
            // Hand the keyboard back to the pane: the filter box must not keep
            // egui focus after a row click, or the Ctrl+C / Ctrl+V / Delete
            // shortcuts are suppressed (see the list view for the same fix).
            if let Some(id) = ui.memory(|m| m.focused()) {
                ui.memory_mut(|m| m.surrender_focus(id));
            }
        }
        let _ = resp.context_menu(|ui| {
            self.set_selection(is_left, &name);
            self.file_context_menu(ui, &name, is_dir, &path);
        });
    }

    // ---------------- shared bits ----------------

    /// Start a Windows OLE drag of the given file(s) so they can be dropped
    /// into any other program — QQ, an upload box, Explorer, … . Returns true
    /// if a drag actually began. We drag the whole selection when the dragged
    /// item is part of a multi-selection, otherwise just that one file.
    ///
    /// `DoDragDrop` runs a modal message loop and blocks until the drop (or
    /// Escape) — the same behaviour as Explorer, and fine on the UI thread.
    fn try_start_os_drag(&mut self, is_left: bool, name: &str) -> bool {
        let panel = get_panel(self, is_left);
        let cwd = panel.cwd.clone();
        let paths: Vec<std::path::PathBuf> = if panel.multi.contains(name) && panel.multi.len() > 1 {
            panel.multi.iter().map(|n| cwd.join(n)).collect()
        } else {
            vec![cwd.join(name)]
        };
        if paths.is_empty() {
            return false;
        }
        // Defer the actual OLE drag to the top of the next `update` so
        // `DoDragDrop`'s modal loop runs outside any widget callback (no
        // re-entrant UI access). It blocks until the mouse is released.
        self.pending_os_drag = Some(paths);
        true
    }

    /// Double-click / Enter on a row.
    fn open_row(&mut self, is_left: bool, name: &str, is_dir: bool, listing: &[app::Entry]) {
        self.set_active_pane(is_left);
        let path = get_panel(self, is_left).cwd.join(name);
        if is_dir {
            get_panel(self, is_left).navigate(path);
            self.apply_cached_sizes();
        } else if crate::preview::is_previewable(name, &path) {
            self.open_viewer(listing, path);
        } else {
            fs_util::open_default_file(&path);
        }
    }

    /// Put back the listing borrowed by `render_list` / `render_grid`.
    /// Skipped when a navigation/refresh happened during the frame — the panel
    /// already holds the fresh listing then.
    fn restore_entries(&mut self, is_left: bool, generation: u64, entries: Vec<app::Entry>) {
        let p = get_panel(self, is_left);
        if p.generation == generation {
            p.entries = entries;
        }
    }

    /// The empty area under the rows: right-click for the pane menu, and a
    /// double-click to type a path (Explorer opens the address bar the same
    /// way). A click there also focuses this pane, so the two never fight.
    fn pane_bg_menu(&mut self, ui: &mut egui::Ui, is_left: bool, sel_rec: Option<&crate::fs_util::RecItem>) {
        let rest = ui.available_rect_before_wrap();
        if rest.height() < 6.0 {
            return;
        }
        let resp = ui.interact(
            rest,
            egui::Id::new(("pane_bg", is_left)),
            egui::Sense::click(),
        );
        if resp.double_clicked() {
            self.begin_addr_edit(is_left);
        } else if resp.clicked() {
            self.set_active_pane(is_left);
        }
        let _ = resp.context_menu(|ui| self.pane_context_menu(ui, is_left, sel_rec));
    }

    /// Clickable column header that sorts the panel. Every column is left
    /// aligned and drawn with a separator line on its right edge, so each type
    /// (名称/类型/大小/修改时间/修改日期) reads as its own block. When
    /// `resizable`, the right-edge divider becomes a drag handle that resizes
    /// the column (the "修改日期" column is the only non-resizable one because
    /// it flexes to fill the remaining width).
    fn sort_header(
        &mut self,
        ui: &mut egui::Ui,
        is_left: bool,
        key: SortKey,
        idx: usize,
        width: f32,
        resizable: bool,
    ) {
        let (active_key, dir) = {
            let p = get_panel(self, is_left);
            (p.sort_key, p.sort_dir)
        };
        let mut label = key.label(self.lang).to_string();
        if active_key == key {
            label.push(' ');
            label.push_str(dir.arrow());
        }
        // Reserve the whole column rect and draw the text with the painter, so
        // there is no inner widget that could intercept clicks — the entire
        // header cell is a single click target. A `Label` is *selectable* by
        // default, which turns it into a `Sense::click_and_drag()` widget: it
        // swallowed clicks that landed on the glyphs (only blank space sorted)
        // and showed a text "I-beam" cursor, as if the header were editable.
        // The text sits at the same x as the values underneath, keeping the
        // grid aligned.
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::click());
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(rect.min.x + CELL_PAD, rect.min.y),
            egui::pos2(rect.max.x - CELL_PAD, rect.max.y),
        );
        cell_text(ui, text_rect, label, true);
        if resp.clicked() {
            self.sort_by(is_left, key);
        }
        resp.on_hover_cursor(egui::CursorIcon::PointingHand)
            .on_hover_text(crate::lang::tr("点击表头排序", self.lang));

        // Draggable divider on the right edge: drag to resize this column.
        if resizable {
            let handle = egui::Rect::from_min_max(
                egui::pos2(rect.right() - 4.0, rect.top()),
                egui::pos2(rect.right() + 4.0, rect.bottom()),
            );
            let h = ui.interact(
                handle,
                egui::Id::new(("col_resize", is_left, idx)),
                egui::Sense::drag(),
            );
            if h.dragged() {
                let d = h.drag_delta().x;
                let (field, floor) = match idx {
                    0 => (&mut self.col_name, MIN_NAME),
                    1 => (&mut self.col_type, MIN_TYPE),
                    2 => (&mut self.col_size, MIN_SIZE),
                    3 => (&mut self.col_time, MIN_TIME),
                    _ => (&mut self.col_name, MIN_NAME),
                };
                *field = (*field + d).max(floor);
            }
            if h.drag_stopped() {
                self.persist_columns();
            }
            if h.hovered() || h.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }
        }
        col_divider(ui, rect);
    }

    /// Right rail: preview of the active panel's selection.
    fn render_preview_pane(&mut self, ui: &mut egui::Ui) {
        let path = if self.active_pane == 0 {
            self.p0().sel_path()
        } else {
            self.p1().sel_path()
        };
        let Some(path) = path else {
            weak(ui, crate::lang::tr("未选中文件", self.lang));
            return;
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let is_dir = path.is_dir();
        let md = std::fs::metadata(&path).ok();

        ui.add(egui::Label::new(egui::RichText::new(&name).strong()).truncate());
        // The thing's own (full-colour) icon, drawn at preview size.
        let ext = path
            .extension()
            .map(|x| x.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if let Some(id) = self.icons.get(ui.ctx(), shellicon::Kind::for_entry(is_dir, &ext)) {
            let (r, _) = ui.allocate_exact_size(egui::vec2(LARGE_ICON, LARGE_ICON), egui::Sense::hover());
            draw_tex(ui.painter(), id, r, LARGE_ICON);
        }
        if let Some(m) = &md {
            if is_dir {
                // Folders show their computed total too (from the size worker),
                // so the preview pane agrees with the 大小 column.
                let sz = self
                    .active_panel()
                    .sel_entry()
                    .filter(|e| e.size_known)
                    .map(|e| human_size(e.size));
                match sz {
                    Some(s) => weak(
                        ui,
                        format!(
                            "{} · {} {s}",
                            crate::lang::tr("文件夹", self.lang),
                            crate::lang::tr("大小", self.lang)
                        ),
                    ),
                    None => weak(ui, crate::lang::tr("文件夹", self.lang)),
                };
            } else {
                weak(ui, format!("{} {}", crate::lang::tr("大小", self.lang), human_size(m.len())));
            }
            if let Ok(t) = m.modified() {
                if let Ok(d) = t.duration_since(std::time::UNIX_EPOCH) {
                    weak(ui, format!(
                        "{} {} {}",
                        crate::lang::tr("修改", self.lang),
                        human_time(d.as_secs()),
                        human_clock(d.as_secs())
                    ));
                }
            }
        }
        ui.separator();

        if !is_dir
            && (crate::preview::is_image(&name, &path) || crate::preview::is_video(&name, &path))
        {
            if crate::preview::is_image(&name, &path) {
                // Animated GIFs play inline in the pane (with a play/pause
                // key); everything else is a still decode.
                if crate::preview::is_gif(&name, &path) {
                    self.render_side_gif(ui, &path);
                } else {
                // Same progressive trick as the viewer: the grid thumbnail
                // shows up immediately while the 720 px decode runs.
                let (t, ready) = self
                    .previews
                    .get_or_smaller(&path, preview::PREVIEW, preview::Priority::Urgent);
                if let Some(t) = t {
                    let ar = t.aspect().max(0.01);
                    let mut w = ui.available_width().max(40.0);
                    let mut h = w / ar;
                    let max_h = (ui.available_height() - 40.0).max(80.0);
                    if h > max_h {
                        h = max_h;
                        w = h * ar;
                    }
                    let tex = egui::load::SizedTexture::new(t.tex.id(), egui::vec2(w, h));
                    ui.add(egui::Image::new(tex));
                    if !ready {
                        weak(ui, crate::lang::tr("解码中…", self.lang));
                        ui.ctx()
                            .request_repaint_after(std::time::Duration::from_millis(
                                self.perf_repaint_ms(),
                            ));
                    }
                } else if self.previews.failed(&path, preview::PREVIEW) {
                    weak(ui, crate::lang::tr("无法解码此图片", self.lang));
                } else {
                    weak(ui, crate::lang::tr("解码中…", self.lang));
                    ui.ctx()
                        .request_repaint_after(std::time::Duration::from_millis(
                            self.perf_repaint_ms(),
                        ));
                }
                }
            } else {
                // Video: decode the first frame in the background and show it
                // as the preview; the viewer adds inline playback.
                let cached = self
                    .side_video_tex
                    .as_ref()
                    .filter(|(p, _)| p == &path)
                    .map(|(_, t)| t.clone());
                // Only ask for a decode while there is nothing to show — the
                // poster job is dropped once it lands, so an unconditional
                // spawn would loop every frame.
                if cached.is_none() {
                    self.spawn_video_job(&path, VideoJobKind::Side);
                }
                match cached {
                    Some(tex) => {
                        let ar = tex.aspect_ratio().max(0.01);
                        let mut w = ui.available_width().max(40.0);
                        let mut h = w / ar;
                        let max_h = (ui.available_height() - 60.0).max(80.0);
                        if h > max_h {
                            h = max_h;
                            w = h * ar;
                        }
                        let sized = egui::load::SizedTexture::new(tex.id(), egui::vec2(w, h));
                        ui.add(egui::Image::new(sized));
                    }
                    None => {
                        if self.video_poster_failed.contains(&path) {
                            weak(
                                ui,
                                crate::lang::tr(
                                    "无法解码此视频（格式不受 Media Foundation 支持，如 MKV/WebM/FLV，或 HEVC/H.265 等需额外解码器的编码）",
                                    self.lang,
                                ),
                            );
                        } else if self
                            .video_jobs
                            .iter()
                            .any(|j| j.path == path && j.kind == VideoJobKind::Side)
                        {
                            weak(ui, crate::lang::tr("正在准备视频…", self.lang));
                            ui.ctx().request_repaint_after(
                                std::time::Duration::from_millis(self.perf_repaint_ms()),
                            );
                        } else {
                            weak(ui, crate::lang::tr("视频文件，点击在查看器中播放", self.lang));
                        }
                    }
                }
                if ui.button(crate::lang::tr("播放视频", self.lang)).clicked() {
                    self.open_viewer(&[], path.clone());
                }
            }
            if ui.button(crate::lang::tr("放大查看", self.lang)).clicked() {
                // No in-memory listing handy here; fall back to a directory read
                // (only this one folder, and only on an explicit button press).
                self.open_viewer(&[], path.clone());
            }
        } else if is_dir {
            weak(ui, crate::lang::tr("文件夹没有内置预览，双击可进入。", self.lang));
        } else if let Some(mut text) = self.text_preview(&path) {
            // Text up to 10 KiB: show the content, read-only and scrollable
            // (binary files are rejected by the NUL-byte test upstream).
            egui::ScrollArea::vertical()
                .id_salt("text_preview")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.add(
                        egui::TextEdit::multiline(&mut text)
                            .desired_width(ui.available_width())
                            .font(egui::TextStyle::Monospace)
                            .interactive(false),
                    );
                });
        } else {
            weak(ui, crate::lang::tr("此类文件没有内置预览", self.lang));
            if ui.button(crate::lang::tr("用默认程序打开", self.lang)).clicked() {
                fs_util::open_default_file(&path);
            }
        }
    }

    /// Animated-GIF playback inside the side preview pane: decodes the whole
    /// clip once (background thread), animates frame-by-frame, and shows a
    /// play/pause key. Static images and videos go through their own paths.
    fn render_side_gif(&mut self, ui: &mut egui::Ui, path: &std::path::Path) {
        // Reset everything when the selected file changes.
        if self.side_gif_path.as_deref() != Some(path) {
            self.side_gif = None;
            self.side_gif_rx = None;
            self.side_gif_idx = 0;
            self.side_gif_due = None;
            self.side_gif_playing = true;
            self.side_gif_tex = None;
            self.side_gif_path = Some(path.to_path_buf());
            if !self.side_gif_failed.contains(path) {
                let (tx, rx) = std::sync::mpsc::channel();
                let p = path.to_path_buf();
                std::thread::spawn(move || {
                    let c = crate::gif::decode(&p);
                    let _ = tx.send(c);
                });
                self.side_gif_rx = Some((path.to_path_buf(), rx));
            }
        }
        // Land a finished decode.
        let mut landed: Option<(std::path::PathBuf, Option<crate::gif::GifClip>)> = None;
        if let Some((p, rx)) = self.side_gif_rx.as_ref() {
            if let Ok(clip) = rx.try_recv() {
                landed = Some((p.clone(), clip));
            }
        }
        if let Some((p, clip)) = landed {
            self.side_gif_rx = None;
            match clip {
                Some(c) => {
                    self.side_gif = Some((p, c));
                    self.side_gif_idx = 0;
                    self.side_gif_due = None;
                    self.side_gif_playing = true;
                }
                None => {
                    self.side_gif_failed.insert(p);
                }
            }
        }
        let n = self
            .side_gif
            .as_ref()
            .map(|(_, c)| c.frames.len())
            .unwrap_or(0);
        if n == 0 {
            if self.side_gif_failed.contains(path) {
                weak(ui, crate::lang::tr("无法解码此 GIF", self.lang));
            } else {
                // Show the cached first frame at once (poster) while the whole
                // clip keeps decoding in the background, instead of a blank
                // "准备中" wait.
                let (t, _ready) = self
                    .previews
                    .get_or_smaller(path, preview::PREVIEW, preview::Priority::Urgent);
                if let Some(t) = t {
                    let ar = t.aspect().max(0.01);
                    let mut w = ui.available_width().max(40.0);
                    let mut h = w / ar;
                    let max_h = (ui.available_height() - 60.0).max(80.0);
                    if h > max_h {
                        h = max_h;
                        w = h * ar;
                    }
                    let tex = egui::load::SizedTexture::new(t.tex.id(), egui::vec2(w, h));
                    ui.add(egui::Image::new(tex));
                }
                weak(ui, crate::lang::tr("正在准备 GIF…", self.lang));
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(
                        self.perf_repaint_ms(),
                    ));
            }
            return;
        }
        // Advance the animation when playing (loop honours 循环播放).
        if self.side_gif_playing {
            let now = std::time::Instant::now();
            let due = self.side_gif_due;
            let at_end = self.side_gif_idx + 1 >= n;
            if due.map_or(true, |d| now >= d) {
                if at_end {
                    if self.viewer_loop {
                        self.side_gif_idx = 0;
                    } else {
                        self.side_gif_idx = n - 1;
                        self.side_gif_due = None;
                    }
                } else {
                    self.side_gif_idx += 1;
                }
                let delay = self
                    .side_gif
                    .as_ref()
                    .and_then(|(_, c)| c.frames.get(self.side_gif_idx))
                    .map(|f| f.delay_ms)
                    .unwrap_or(100);
                self.side_gif_due =
                    Some(now + std::time::Duration::from_millis(delay.clamp(20, 1000) as u64));
            }
        }
        // Upload the current frame to a reused texture only when the visible
        // frame index changed — re-decoding + re-uploading every frame is what
        // burned CPU. Then draw it fitted.
        let (cw, ch, rgba) = {
            let c = self.side_gif.as_ref().unwrap();
            let fr = &c.1.frames[self.side_gif_idx];
            (c.1.w, c.1.h, &fr.rgba as &[u8])
        };
        let id = if self.side_gif_painted_idx != self.side_gif_idx
            || self.side_gif_tex.is_none()
        {
            let img = egui::ColorImage::from_rgba_unmultiplied([cw as usize, ch as usize], rgba);
            let id = match self.side_gif_tex.as_mut() {
                Some(t) => {
                    t.set(img, egui::TextureOptions::LINEAR);
                    t.id()
                }
                None => {
                    let t = ui
                        .ctx()
                        .load_texture("side_gif:frame", img, egui::TextureOptions::LINEAR);
                    let id = t.id();
                    self.side_gif_tex = Some(t);
                    id
                }
            };
            self.side_gif_painted_idx = self.side_gif_idx;
            id
        } else {
            self.side_gif_tex.as_ref().map(|t| t.id()).unwrap()
        };
        let ar = (cw as f32 / ch as f32).max(0.01);
        let mut w = ui.available_width().max(40.0);
        let mut h = w / ar;
        let max_h = (ui.available_height() - 60.0).max(80.0);
        if h > max_h {
            h = max_h;
            w = h * ar;
        }
        let sized = egui::load::SizedTexture::new(id, egui::vec2(w, h));
        ui.add(egui::Image::new(sized));
        // Play / pause key + frame readout.
        ui.horizontal(|ui| {
            let label = if self.side_gif_playing {
                crate::lang::tr("暂停", self.lang)
            } else {
                crate::lang::tr("播放", self.lang)
            };
            if ui.button(label).clicked() {
                self.side_gif_playing = !self.side_gif_playing;
                if self.side_gif_playing {
                    self.side_gif_due = None;
                }
            }
            weak(ui, format!("{}/{}", self.side_gif_idx + 1, n));
        });
        if self.side_gif_playing {
            // Wake exactly when the next frame is due instead of forcing 60 fps.
            if let Some(due) = self.side_gif_due {
                let remaining = (due - std::time::Instant::now()).as_millis() as u64;
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(remaining.max(8)));
            } else {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_millis(
                        self.perf_repaint_ms(),
                    ));
            }
        }
    }
}

// Padding inside every cell so text never touches the column divider lines.
const CELL_PAD: f32 = 6.0;
// Width reserved for the row's leading icon (folder / file / recycle).
const ICON_W: f32 = 20.0;
/// Which leading glyph a list row draws.
#[derive(Clone, Copy)]
enum IconKind {
    Folder,
    File,
    Recycle,
    Image,
}

/// Draw a clean, theme-aware vector icon centred in `rect`, `px` on a side.
/// Replaces the emoji glyphs, which rendered inconsistently across platforms
/// and broke column alignment.
fn draw_icon(
    painter: &egui::Painter,
    kind: IconKind,
    rect: egui::Rect,
    color: egui::Color32,
    px: f32,
) {
    // Centre inside the slot: the glyph used to be left-anchored and sized
    // from the slot *height* only, so in the (wide) grid cells it stayed a
    // 16px speck at the left edge while the shell textures were 48px.
    let s = px.max(1.0);
    let slot = egui::Rect::from_center_size(rect.center(), egui::vec2(s, s));
    let x = slot.left();
    let cy = slot.center().y;
    let w = s;
    let r = egui::CornerRadius::same(3);
    match kind {
        IconKind::Folder => {
            let h = s * 0.74;
            let y0 = cy - h / 2.0;
            let tab_w = w * 0.44;
            let tab_h = h * 0.34;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(x, y0), egui::vec2(tab_w, tab_h)),
                egui::CornerRadius { nw: 3, ne: 3, se: 0, sw: 0 },
                color,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(x, y0 + tab_h * 0.55),
                    egui::vec2(w, h - tab_h * 0.55),
                ),
                r,
                color,
            );
        }
        IconKind::File => {
            let h = s * 0.94;
            let y0 = cy - h / 2.0;
            let fold = w * 0.36;
            let body = egui::Rect::from_min_size(egui::pos2(x, y0), egui::vec2(w, h));
            painter.rect_stroke(body, r, egui::Stroke::new(1.5_f32, color), egui::StrokeKind::Inside);
            // folded top-right corner
            painter.line_segment(
                [egui::pos2(x + w - fold, y0), egui::pos2(x + w, y0 + fold)],
                egui::Stroke::new(1.5_f32, color),
            );
            painter.line_segment(
                [egui::pos2(x + w, y0 + fold), egui::pos2(x + w, y0)],
                egui::Stroke::new(1.5_f32, color),
            );
        }
        IconKind::Recycle => {
            let h = s * 0.92;
            let y0 = cy - h / 2.0;
            let body = egui::Rect::from_min_size(
                egui::pos2(x + w * 0.14, y0 + h * 0.24),
                egui::vec2(w * 0.72, h * 0.76),
            );
            painter.rect_stroke(body, r, egui::Stroke::new(1.5_f32, color), egui::StrokeKind::Inside);
            painter.line_segment(
                [egui::pos2(x + w * 0.04, y0 + h * 0.24), egui::pos2(x + w * 0.96, y0 + h * 0.24)],
                egui::Stroke::new(2.0_f32, color),
            );
            painter.line_segment(
                [egui::pos2(x + w * 0.34, y0 + h * 0.12), egui::pos2(x + w * 0.66, y0 + h * 0.12)],
                egui::Stroke::new(2.0_f32, color),
            );
            painter.line_segment(
                [egui::pos2(x + w * 0.5, y0 + h * 0.02), egui::pos2(x + w * 0.5, y0 + h * 0.12)],
                egui::Stroke::new(2.0_f32, color),
            );
        }
        IconKind::Image => {
            let h = s * 0.88;
            let y0 = cy - h / 2.0;
            let body = egui::Rect::from_min_size(egui::pos2(x, y0), egui::vec2(w, h));
            painter.rect_stroke(body, r, egui::Stroke::new(1.5_f32, color), egui::StrokeKind::Inside);
            painter.circle_filled(egui::pos2(x + w * 0.32, y0 + h * 0.34), s * 0.1, color);
            painter.line_segment(
                [egui::pos2(x + w * 0.16, y0 + h * 0.84), egui::pos2(x + w * 0.46, y0 + h * 0.54)],
                egui::Stroke::new(1.5_f32, color),
            );
            painter.line_segment(
                [egui::pos2(x + w * 0.46, y0 + h * 0.54), egui::pos2(x + w * 0.66, y0 + h * 0.74)],
                egui::Stroke::new(1.5_f32, color),
            );
            painter.line_segment(
                [egui::pos2(x + w * 0.66, y0 + h * 0.74), egui::pos2(x + w * 0.88, y0 + h * 0.5)],
                egui::Stroke::new(1.5_f32, color),
            );
        }
    }
}

/// Secondary text. `Ui::weak` fades the color 50% toward the background:
/// light theme lands on gray ~164 (contrast ~2.5:1 on the panel) and dark
/// theme on gray ~83 — both wash out and read as "too gray". Keep the
/// secondary tone but pick explicit grays that stay legible in both themes
/// (light ~108 on 250 ≈ 5:1, dark ~175 on 27 ≈ 7:1).
/// Timestamp text for a table column of width `w`. The full
/// "YYYY-MM-DD HH:MM" stamp needs ~128px; on a narrow pane (split view with
/// the preview pane open, or a small window) the shorter forms keep the
/// column readable instead of truncating mid-number.
fn col_datetime(unix_secs: u64, w: f32) -> String {
    if unix_secs == 0 {
        return "—".to_string();
    }
    if w >= 128.0 {
        crate::app::human_datetime(unix_secs)
    } else if w >= 88.0 {
        crate::app::human_datetime_short(unix_secs)
    } else {
        crate::app::human_date_short(unix_secs)
    }
}

fn weak(ui: &mut egui::Ui, text: impl Into<String>) -> egui::Response {
    let gray = if ui.visuals().dark_mode { 175 } else { 108 };
    ui.add(egui::Label::new(
        egui::RichText::new(text.into()).color(egui::Color32::from_gray(gray)),
    ))
}

/// One fixed-width table cell. The rect is reserved with `allocate_exact_size`
/// on purpose: `allocate_ui_with_layout` only advances the layout by the
/// *content* width, which made every column start at a different x per row —
/// the reason the table looked ragged instead of forming a real grid.
fn table_cell(ui: &mut egui::Ui, width: f32, draw_divider: bool, text: impl Into<String>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::hover());
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + CELL_PAD, rect.min.y),
        egui::pos2(rect.max.x - CELL_PAD, rect.max.y),
    );
    cell_text(ui, text_rect, text, false);
    if draw_divider {
        col_divider(ui, rect);
    }
}

/// Fixed-width cell for the 名称 column: a leading icon + left-padded,
/// truncated name. Always carries a right divider.
/// `emoji` is `""` unless the emoji icon mode is on.
fn name_cell(
    ui: &mut egui::Ui,
    width: f32,
    name: impl Into<String>,
    kind: IconKind,
    tex: Option<egui::TextureId>,
    emoji: &str,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::hover());
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y),
        egui::vec2(ICON_W, ROW_H),
    );
    // The shell's icon when we have one; the emoji glyph or the vector shape is
    // the fallback (lighter icon modes never ask the shell at all).
    let icon_color = match kind {
        IconKind::Folder => egui::Color32::from_rgb(225, 180, 70),
        _ => ui.visuals().text_color(),
    };
    draw_row_icon(
        ui.painter(),
        tex,
        emoji,
        kind,
        icon_rect,
        LIST_ICON,
        icon_color,
    );
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + ICON_W, rect.min.y),
        egui::pos2(rect.max.x - CELL_PAD, rect.max.y),
    );
    cell_text(ui, text_rect, name, false);
    col_divider(ui, rect);
    ui.interact(rect, egui::Id::new(("name_cell", rect.min.x as i32)), egui::Sense::hover())
}

/// Emoji glyph for a file type, used by the emoji icon mode.
///
/// These come from egui's bundled Noto Emoji face, which is part of the
/// default font set — no extra font is loaded and the shell is never asked for
/// an icon. The atlas is alpha-only, so the glyphs are drawn monochrome
/// (tinted like text) rather than in colour.
fn emoji_for(is_dir: bool, ext: &str) -> &'static str {
    if is_dir {
        return "\u{1F4C1}"; // 📁
    }
    match ext {
        "png" | "jpg" | "jpeg" | "jpe" | "bmp" | "gif" | "webp" | "ico" | "tif" | "tiff"
        | "tga" | "heic" | "svg" => "\u{1F5BC}", // 🖼
        "mp3" | "wav" | "flac" | "aac" | "m4a" | "ogg" | "wma" | "opus" => "\u{1F3B5}", // 🎵
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "flv" | "webm" | "mpg" | "mpeg" => {
            "\u{1F3AC}"
        } // 🎬
        "zip" | "7z" | "rar" | "tar" | "gz" | "bz2" | "xz" | "iso" => "\u{1F4E6}", // 📦
        "pdf" => "\u{1F4D5}",                                                      // 📕
        "doc" | "docx" | "rtf" | "odt" => "\u{1F4D8}",                             // 📘
        "xls" | "xlsx" | "csv" | "ods" => "\u{1F4CA}",                             // 📊
        "ppt" | "pptx" | "odp" => "\u{1F4D7}",                                     // 📗
        "txt" | "md" | "log" | "ini" | "cfg" | "toml" | "json" | "xml" | "yml" | "yaml" => {
            "\u{1F4DD}"
        } // 📝
        "rs" | "c" | "h" | "cpp" | "hpp" | "cc" | "cs" | "java" | "js" | "ts" | "py" | "go"
        | "rb" | "php" | "html" | "css" | "sh" | "bat" | "ps1" | "sql" => "\u{1F4DC}", // 📜
        "exe" | "msi" | "dll" | "sys" => "\u{2699}",                                    // ⚙
        _ => "\u{1F4C4}",                                                               // 📄
    }
}

/// Draw an emoji glyph centred in `rect`, `px` on a side.
///
/// Painted as text on purpose: the glyph then comes out of egui's font atlas
/// like any other character, so an emoji costs a few hundred bytes instead of a
/// texture of its own.
fn draw_emoji(
    painter: &egui::Painter,
    s: &str,
    rect: egui::Rect,
    px: f32,
    color: egui::Color32,
) {
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        s.to_string(),
        egui::FontId::new(px, egui::FontFamily::Proportional),
        color,
    );
}

/// Leading icon of a list row / grid cell: the shell texture when there is one,
/// else the emoji glyph (emoji mode), else the built-in vector shape.
///
/// `emoji` is `""` unless the emoji mode is on.
fn draw_row_icon(
    painter: &egui::Painter,
    tex: Option<egui::TextureId>,
    emoji: &str,
    kind: IconKind,
    rect: egui::Rect,
    px: f32,
    color: egui::Color32,
) {
    if let Some(id) = tex {
        draw_tex(painter, id, rect, px);
    } else if !emoji.is_empty() {
        draw_emoji(painter, emoji, rect, px, color);
    } else {
        draw_icon(painter, kind, rect, color, px);
    }
}

/// Draw a (square) icon texture at `px` size, left-anchored and vertically
/// centred in `rect`.
fn draw_tex(painter: &egui::Painter, id: egui::TextureId, rect: egui::Rect, px: f32) {
    let dest = egui::Rect::from_center_size(rect.center(), egui::vec2(px, px));
    painter.image(
        id,
        dest,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
    );
}

/// Left-aligned, vertically centred, truncated text inside a reserved rect.
///
/// Drawn straight onto the painter on purpose — **not** a `Label` widget. A
/// `Label` is *selectable* by default, which upgrades it to a
/// `Sense::click_and_drag()` widget: it swallowed every click/double-click that
/// landed on the glyphs (so rows only reacted when clicking blank space) and
/// showed a text "I-beam" cursor, making the cells look editable. Painting the
/// galley keeps cells purely decorative so the surrounding row owns all input.
fn cell_text(ui: &mut egui::Ui, rect: egui::Rect, text: impl Into<String>, strong: bool) {
    let color = if strong {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    cell_text_with(ui, rect, text, font_id, color);
}

/// Painter-drawn text with an explicit font/colour (used for the smaller
/// captions under thumbnails). Same rationale as [`cell_text`]: no widget.
fn cell_text_with(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    text: impl Into<String>,
    font_id: egui::FontId,
    color: egui::Color32,
) {
    let mut job = egui::text::LayoutJob::simple(text.into(), font_id, color, rect.width());
    job.wrap = egui::text::TextWrapping::truncate_at_width(rect.width());
    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(rect.min.x, rect.center().y - galley.size().y * 0.5);
    ui.painter().with_clip_rect(rect).galley(pos, galley, color);
}

/// One cell of the pane status band: painter-drawn text starting at `x`,
/// truncated so it never runs past `max_x`. Returns where the next cell may
/// start. Decorative by design (see [`cell_text`]).
fn status_cell(
    ui: &mut egui::Ui,
    x: f32,
    cy: f32,
    max_x: f32,
    text: String,
    color: egui::Color32,
) -> f32 {
    let avail = (max_x - x).max(0.0);
    if avail < 16.0 {
        return x; // too narrow to be worth anything
    }
    let font = egui::TextStyle::Body.resolve(ui.style());
    let mut job = egui::text::LayoutJob::simple(text, font, color, avail);
    job.wrap = egui::text::TextWrapping::truncate_at_width(avail);
    let galley = ui.painter().layout_job(job);
    let pos = egui::pos2(x, cy - galley.size().y * 0.5);
    let w = galley.size().x;
    ui.painter().galley(pos, galley, color);
    x + w + 10.0
}

/// Thin vertical line at the right edge of a column, so every column type is
/// visually separated instead of bleeding into the next one.
fn col_divider(ui: &mut egui::Ui, rect: egui::Rect) {
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    let x = rect.right() + 1.0;
    // Draw the full cell height: rows are flush (no vertical item spacing), so
    // the per-row segments join into one continuous line per column.
    ui.painter().vline(x, rect.top()..=rect.bottom(), stroke);
}

/// Borrow the panel selected by `is_left` (plain fn → clean lifetimes).
fn get_panel<'a>(s: &'a mut Explorer, is_left: bool) -> &'a mut app::Panel {
    s.pane_of(is_left)
}

/// Short label for a tab: the folder name, or the drive root (`C:\`).
fn tab_label(cwd: &std::path::Path) -> String {
    let name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| cwd.to_string_lossy().into_owned());
    let max = 18usize;
    if name.chars().count() > max {
        let cut: String = name.chars().take(max - 1).collect();
        format!("{cut}…")
    } else {
        name
    }
}

