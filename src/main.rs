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

mod app;
mod cache;
mod convert;
mod explorer;
mod fs_util;
mod icons;
mod lang;
mod preview;
mod quickaccess;
mod shell;
mod shellicon;
mod tray;
mod dragout;

use eframe::egui;
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use app::{
    Clip, SortKey, View, human_clock, human_datetime, human_size, human_time, path_segments,
};
use cache::Store;
use explorer::{Explorer, RecInfo};

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
const CELL_H: f32 = 120.0;

fn main() -> eframe::Result {
    let store = Store::load();

    // Folder-size worker: one persistent thread, unbounded queue.
    let (size_tx, size_rx) = channel::<PathBuf>();
    let (back_tx, back_rx) = channel::<(PathBuf, u64, u64)>();
    std::thread::spawn(move || {
        while let Ok(path) = size_rx.recv() {
            let size = explorer::total_size(&path);
            let mtime = match std::fs::metadata(&path) {
                Ok(m) => m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                Err(_) => 0,
            };
            back_tx.send((path, mtime, size)).ok();
        }
    });

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1100.0, 640.0])
            .with_title("MarinExplorer")
            .with_icon(app_icon()),
        ..Default::default()
    };

    // 中文字体：运行时从 Windows 字体目录加载系统自带字体（微软雅黑优先）。
    // 不把字体文件打包进 exe（省掉 ~8MB），中文观感也与 Windows 原生 UI 一致。
    let mut fonts = egui::FontDefinitions::default();
    if let Some((_name, data)) = load_windows_cjk_font() {
        fonts
            .font_data
            .insert("cjk".to_owned(), std::sync::Arc::new(data));
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .push("cjk".to_owned());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .push("cjk".to_owned());
    }

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
            let mut ex = Explorer::new(store, size_tx, back_rx);
            // A path handed over on the command line (context menu, drag onto
            // the exe, `MarinExplorer.exe D:\foo`) opens that folder — or, for
            // a file, its parent with the file selected.
            if let Some(arg) = std::env::args().nth(1) {
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

/// CJK font, loaded at runtime from the Windows font directory — Microsoft YaHei
/// first. We deliberately do *not* bundle a font file: that costs ~8MB of exe
/// size, and the system font also matches the native Windows look.
///
/// egui ships no CJK glyphs at all (only Ubuntu-Light / Hack / NotoEmoji), so
/// without this every Chinese character renders as an empty box.
///
/// epaint *panics* when handed a font file it cannot parse, so every candidate
/// is validated with `ttf-parser` before it reaches egui; a missing or broken
/// file simply moves us on to the next candidate.
fn load_windows_cjk_font() -> Option<(&'static str, egui::FontData)> {
    // (display name, file name inside %WINDIR%\Fonts, face index within a .ttc)
    const CANDIDATES: &[(&str, &str, u32)] = &[
        ("Microsoft YaHei", "msyh.ttc", 0),
        ("Microsoft YaHei UI", "msyhui.ttc", 0),
        ("DengXian", "Deng.ttf", 0),
        ("SimHei", "simhei.ttf", 0),
        ("SimSun", "simsun.ttc", 0),
    ];

    let windir = std::env::var("WINDIR")
        .or_else(|_| std::env::var("SystemRoot"))
        .unwrap_or_else(|_| "C:\\Windows".to_owned());
    let dir = std::path::PathBuf::from(windir).join("Fonts");

    for &(name, file, index) in CANDIDATES {
        let path = dir.join(file);
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        if ttf_parser::Face::parse(&bytes, index).is_err() {
            continue;
        }
        let mut data = egui::FontData::from_owned(bytes);
        data.index = index;
        eprintln!("[font] CJK: {name} ({file})");
        return Some((name, data));
    }

    eprintln!("[font] warning: no Windows CJK font found; Chinese may render as boxes");
    None
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
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Light / dark theme, applied every frame so a toggle is instant.
        let mut visuals = if self.dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        if self.dark {
            // The stock dark palette uses greyish text that is hard to read on
            // the dark panels. `override_text_color` (the only writable text
            // colour in egui 0.31) brightens every text role toward white.
            visuals.override_text_color = Some(egui::Color32::from_gray(240));
            visuals.hyperlink_color = egui::Color32::from_rgb(120, 180, 255);
        }
        ctx.set_visuals(visuals);
        // Font-size setting: scale every text style off egui's *default* sizes
        // (not last frame's, which would compound) so the slider is stable.
        if self.font_scale != 1.0 {
            let mut style = (*ctx.style()).clone();
            let base = egui::Style::default().text_styles;
            for (key, base_font) in base.iter() {
                if let Some(f) = style.text_styles.get_mut(key) {
                    f.size = base_font.size * self.font_scale;
                }
            }
            ctx.set_style(style);
        }
        self.text_focus = false;
        // Each frame starts fresh: set when the address editor consumes Enter.
        self.enter_consumed = false;
        // ----- drain background channels -----
        while let Ok((p, m, s)) = self.size_rx.try_recv() {
            self.apply_size_result(p, m, s);
        }
        if let Some(rx) = &self.drives_rx {
            while let Ok(d) = rx.try_recv() {
                self.drives = d;
                self.drives_at = Some(Instant::now());
            }
        }
        if let Some(rx) = &self.sid_rx {
            while let Ok(s) = rx.try_recv() {
                if let Some(sid) = s {
                    self.sid = Some(sid.clone());
                    self.store.set_meta("sid", &sid);
                }
            }
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
        self.refresh_quick_access();
        self.poll_quick_access();
        if let Some(line) = self.poll_job() {
            self.status = line;
        }
        self.apply_cached_sizes();
        self.request_folder_sizes();
        self.maybe_flush();
        self.previews.poll(ctx);

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
                                egui::Slider::new(&mut self.font_scale, 0.8..=1.6)
                                    .step_by(0.05)
                                    .text(fs_label),
                            )
                            .changed()
                        {
                            self.set_font_scale(self.font_scale);
                        }
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
                    ui.label(t("语言"));
                    egui::ComboBox::from_id_salt("lang_select")
                        .selected_text(
                            crate::lang::Lang::names()
                                .iter()
                                .find(|(l, _)| *l == self.lang)
                                .map(|(_, n)| *n)
                                .unwrap_or(""),
                        )
                        .show_ui(ui, |ui| {
                            for (l, name) in crate::lang::Lang::names() {
                                if ui.selectable_value(&mut self.lang, l, name).clicked() {
                                    self.set_lang(l);
                                }
                            }
                        });
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
                ui.separator();
                if icons::button(ui, &icons::COPY, &t("复制"), true, "").clicked() { self.do_copy(); }
                if icons::button(ui, &icons::MOVE, &t("移动"), true, "").clicked() { self.do_move(); }
                if icons::button(ui, &icons::PASTE, &t("粘贴"), true, "").clicked() { self.do_paste(); }
                if icons::button(ui, &icons::TRASH, &t("删除"), true, "").clicked() { self.do_delete(); }
                ui.separator();
                if icons::button(ui, &icons::FOLDER_PLUS, &t("新建"), true, "").clicked() { self.new_folder_open = true; }
                if icons::button(ui, &icons::PLUS, &t("标签页"), true, "Ctrl+T").clicked() { self.new_tab(); }
                if icons::button(ui, &icons::PIN, &t("固定"), true, &t("把当前目录加入左侧“快速访问”")).clicked() { self.pin_current(); }
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
                ui.strong(&self.status);
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
                        ui.add(
                            egui::ProgressBar::new(job.progress.clamp(0.0, 1.0))
                                .text(format!("{} {:.0}%", job.label, job.progress * 100.0)),
                        );
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
            icons::label(ui, &icons::MAP_PIN, &t("位置"));
            ui.separator();

            // Common known folders (Desktop / Downloads / Documents / …):
            // drawn with the shell's own (full-colour) folder icons.
            // Every rail address opens in the *active* pane (dual-pane rule):
            // click a pane to select it, then any address navigates it.
            icons::label(ui, &icons::FOLDER, &t("常用位置"));
            let folders = known_folders(self.lang);
            let texs: Vec<Option<egui::TextureId>> = folders
                .iter()
                .map(|(_, p)| self.icons.get(ctx, shellicon::Kind::Path(p.clone())))
                .collect();
            for (i, (label, p)) in folders.iter().enumerate() {
                let cur = self.active_panel().cwd == *p;
                let glyph = match texs[i] {
                    Some(id) => icons::Glyph::Tex(id),
                    None => icons::Glyph::Svg(&icons::FOLDER),
                };
                if icons::entry(ui, glyph, label, cur, &p.to_string_lossy(), row_w).clicked() {
                    self.active_panel().navigate(p.clone());
                    self.apply_cached_sizes();
                }
            }
            ui.separator();

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
                                "{} {} · {} {}",
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

            // Quick Access: the user's own pins first, then Windows' own.
            // Sits below the drives on purpose (the most-used rail entries —
            // drives and known folders — stay within reach without scrolling).
            icons::label(ui, &icons::STAR, &t("快速访问"));
            let qa = self.quick_access_list();
            if qa.is_empty() {
                weak(ui, t("暂无；点工具栏“固定”"));
            } else {
                // Resolve the native icons up front: the borrow of `self.icons`
                // must not overlap the `self.active_panel()` calls inside the loop.
                let texs: Vec<Option<egui::TextureId>> = qa
                    .iter()
                    .map(|(p, _)| self.icons.get(ctx, shellicon::Kind::Path(p.clone())))
                    .collect();
                for (i, (path, is_pin)) in qa.iter().enumerate() {
                    let label = path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| path.to_string_lossy().into_owned());
                    let glyph = match texs[i] {
                        Some(id) => icons::Glyph::Tex(id),
                        None => icons::Glyph::Svg(&icons::FOLDER),
                    };
                    ui.horizontal(|ui| {
                        let r = icons::entry(
                            ui,
                            glyph,
                            &label,
                            false,
                            &path.to_string_lossy(),
                            row_w,
                        );
                        if r.clicked() {
                            self.active_panel().navigate(path.clone());
                            self.apply_cached_sizes();
                        }
                        // Right-click a Quick-Access folder: open it in a new
                        // tab / in the right pane, reveal, copy path, unpin.
                        let _ = r.context_menu(|ui| {
                            if ui
                                .button(t("在新标签页打开"))
                                .on_hover_text(path.to_string_lossy().to_string())
                                .clicked()
                            {
                                self.open_path_in_new_tab(path.clone());
                                ui.close_menu();
                            }
                            if ui
                                .button(t("在右栏打开"))
                                .on_hover_text(t("两个文件栏并排（记忆）"))
                                .clicked()
                            {
                                self.open_path_in_right_pane(path.clone());
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button(t("在资源管理器中显示")).clicked() {
                                crate::fs_util::reveal_in_explorer(path);
                                ui.close_menu();
                            }
                            if ui.button(t("复制路径")).clicked() {
                                ui.ctx().copy_text(path.to_string_lossy().into_owned());
                                ui.close_menu();
                            }
                            if *is_pin && ui.button(t("取消固定")).clicked() {
                                self.unpin(path);
                                ui.close_menu();
                            }
                        });
                        if *is_pin {
                            if icons::small_button(ui, &icons::X, &t("取消固定")).clicked() {
                                self.unpin(path);
                            }
                        }
                    });
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
                    crate::lang::tr(
                        if perm {
                            "删除 “%s”？\n（永久删除，不进回收站）"
                        } else {
                            "删除 “%s”？\n（移到回收站，可还原）"
                        },
                        self.lang,
                    )
                    .replace("%s", &self.delete_targets[0].display().to_string())
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
                        if ui.button(crate::lang::tr("清空", self.lang)).clicked() { self.spawn_empty_recycle(); }
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
                }
                egui::CentralPanel::default()
                    .show(vctx, |ui| self.viewer_body(ui, &vpath, n_imgs));
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
                if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::C)) {
                    self.do_copy();
                }
                if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::X)) {
                    self.do_move();
                }
                if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::V)) {
                    self.do_paste();
                }
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
        ctx.request_repaint();
    }
}

impl Explorer {
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
        ui.horizontal_wrapped(|ui| {
        // Greyed out when the folder has a single image: the
        // buttons must not look alive and do nothing.
        ui.add_enabled_ui(n_imgs > 1, |ui| {
        if icons::small_button(ui, &icons::CHEV_LEFT, &crate::lang::tr("上一张", self.lang)).clicked() {
        self.viewer_nav(-1);
        }
        if icons::small_button(ui, &icons::CHEV_RIGHT, &crate::lang::tr("下一张", self.lang)).clicked() {
        self.viewer_nav(1);
        }
        });
        ui.separator();
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
        ui.separator();
        if ui.button(crate::lang::tr("用默认程序打开", self.lang)).clicked() {
        fs_util::open_default_file(&vpath);
        }
        if ui.button(crate::lang::tr("在资源管理器中显示", self.lang)).clicked() {
        fs_util::reveal_in_explorer(&vpath);
        }
        ui.separator();
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
        if n_imgs > 1 {
        weak(ui, format!("({}/{})", self.viewer_index + 1, n_imgs));
        }
        });
        ui.separator();
        // Arrow keys step through the images. The viewer lives in its own
        // OS window now, so these events arrive on the viewport's context.
        if n_imgs > 1 {
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
        }
        // Mouse wheel steps through the images (one step per notch).
        let wheel_dy = ui.input(|i| i.raw_scroll_delta.y);
        if wheel_dy.abs() > 0.01 && n_imgs > 1 {
        let ok = self
        .viewer_wheel_at
        .map(|t| t.elapsed().as_millis() >= 100)
        .unwrap_or(true);
        if ok {
        self.viewer_wheel_at = Some(std::time::Instant::now());
        self.viewer_nav(if wheel_dy > 0.0 { 1 } else { -1 });
        }
        }
        // Main image: centred in the space left above the filmstrip, drag to
        // pan when zoomed, and left/right overlay arrows at the window edges.
        let film_h = if n_imgs > 1 { 84.0 } else { 0.0 };
        let area_w = ui.available_width().max(40.0);
        let area_h = (ui.available_height() - film_h).max(60.0);
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
        // Main image.
        if let Some(t) = self.previews.get(&vpath, preview::FULL) {
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
        if n_imgs > 1 {
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
        let _ = r.context_menu(|ui| self.viewer_context_menu(ui, &vpath));
        ui.painter().rect_filled(
        brect,
        egui::CornerRadius { nw: 6, ne: 6, se: 6, sw: 6 },
        egui::Color32::from_black_alpha(90),
        );
        let col = ui.visuals().text_color();
        icons::paint(ui.painter(), glyph.0, brect.shrink(9.0), col, 2.0);
        }
        }
        } else {
        ui.painter().text(
        area.center(),
        egui::Align2::CENTER_CENTER,
        "加载中…",
        egui::FontId::proportional(18.0),
        ui.visuals().weak_text_color(),
        );
        }
        // Filmstrip: pinned to the very bottom by the vertical flow above.
        // When every thumbnail fits we centre the row; when they would
        // overflow we fall back to a sideways scroll.
        if n_imgs > 1 {
        ui.separator();
        let avail_w = ui.available_width();
        let start = self.viewer_index.saturating_sub(4);
        let end = (self.viewer_index + 4).min(n_imgs - 1);
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
    }


    /// Draw the +/-4 thumbnails of the folder around the current image, in a
    /// single horizontal row. Called inside either a centred `ui.horizontal`
    /// or a sideways `ScrollArea`, so it just lays out 64x64 cells left to
    /// right and does not manage its own scroll.
    fn draw_film_thumbs(&mut self, ui: &mut egui::Ui, n_imgs: usize) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let accent = ui.visuals().selection.bg_fill;
        for o in -4..=4i32 {
        let i = self.viewer_index as i32 + o;
        if i < 0 || i as usize >= n_imgs {
        continue;
        }
        let i = i as usize;
        let p = &self.viewer_list[i];
        let is_cur = i == self.viewer_index;
        let thumb_rect =
        egui::Rect::from_min_size(ui.cursor().min, egui::vec2(64.0, 64.0));
        let resp = ui.allocate_rect(thumb_rect, egui::Sense::click());
        let inner = resp.rect.shrink(4.0);
        if let Some(t) = self.previews.get(p, preview::THUMB) {
        let ar = t.aspect().max(0.01);
        let mut w = inner.width();
        let mut h = w / ar;
        if h > inner.height() {
        h = inner.height();
        w = h * ar;
        }
        let dest = egui::Rect::from_center_size(inner.center(), egui::vec2(w, h));
        ui.painter().image(
        t.tex.id(),
        dest,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::WHITE,
        );
        } else {
        ui.painter().text(
        inner.center(),
        egui::Align2::CENTER_CENTER,
        "…",
        egui::FontId::proportional(20.0),
        ui.visuals().weak_text_color(),
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
                            "{} {} · {} {} · {} {}",
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
        let weak = ui.visuals().weak_text_color();

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
        let vis = get_panel(self, is_left).visible(show_hidden);
        // Multi-selection membership, cloned once per frame: rows read it for
        // their highlight, clicks update it through `click_select`.
        let multi = get_panel(self, is_left).multi.clone();
        // Borrow the listing for the duration of the frame instead of cloning
        // it: a 5000-entry folder would otherwise deep-clone every frame.
        let generation = get_panel(self, is_left).generation;
        let entries = std::mem::take(&mut get_panel(self, is_left).entries);

        // Native (full-colour) icons, resolved before the loop: the borrow of
        // `self.icons` must not overlap the `&mut self` work done per row.
        let ctx = ui.ctx().clone();
        let texs: Vec<Option<egui::TextureId>> = vis
            .iter()
            .map(|&i| {
                let e = &entries[i];
                self.icons
                    .get(&ctx, shellicon::Kind::for_entry(e.is_dir, &e.ext))
            })
            .collect();

        egui::ScrollArea::vertical()
            .id_salt(if is_left { "filelist_left" } else { "filelist_right" })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Rows sit flush: with no vertical gap the column lines join
                // into one continuous divider per column (like a real table)
                // instead of a dashed stack of 20px stubs.
                ui.spacing_mut().item_spacing.y = 0.0;
                if vis.is_empty() {
                    weak(ui, crate::lang::tr("空目录", self.lang));
                }
                for (k, &i) in vis.iter().enumerate() {
                    let e = &entries[i];
                    let name = e.name.clone();
                    let is_dir = e.is_dir;
                    // Highlight comes from `multi` alone: a Ctrl+clicked-off
                    // focus row keeps `selected` (preview, Enter) but shows no
                    // highlight, exactly like Explorer.
                    let is_sel = multi.contains(&name);
                    let tex = texs[k];
                    let size_txt = if is_dir {
                        if e.size_known {
                            human_size(e.size)
                        } else {
                            "…".to_string()
                        }
                    } else {
                        human_size(e.size)
                    };
                    let type_txt = e.type_label(self.lang);
                    // 修改时间 / 创建时间: both full date+time stamps, with
                    // shorter forms when the pane is too narrow to fit them.
                    let mtime_txt = col_datetime(e.mtime, cols.time);
                    let ctime_txt = col_datetime(e.ctime, cols.date);
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
                            );
                            table_cell(ui, cols.ty, true, type_txt);
                            table_cell(ui, cols.size, true, size_txt);
                            table_cell(ui, cols.time, true, mtime_txt);
                            table_cell(ui, cols.date, false, ctime_txt);
                        });
                    });
                    let resp = row.response.interact(egui::Sense::click());
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);

                    // Double-click must win over single-click: test it first,
                    // otherwise every double-click also fires `clicked()` and
                    // only ever selects (the old bug — folders never opened).
                    if resp.double_clicked() {
                        self.open_row(is_left, &name, is_dir);
                        ui.ctx().request_repaint();
                    } else if resp.clicked() {
                        // Explorer-style modifier clicks: Ctrl toggles one row,
                        // Shift extends from the anchor, a plain click starts
                        // a fresh single selection.
                        let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
                        let vis_names: Vec<String> =
                            vis.iter().map(|&j| entries[j].name.clone()).collect();
                        self.set_active_pane(is_left);
                        get_panel(self, is_left).click_select(&name, ctrl, shift, &vis_names);
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

                    let row = egui::Frame::NONE.fill(fill).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            name_cell(ui, cols.name, it.name.clone(), IconKind::Recycle, texs[k]);
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
        let vis = get_panel(self, is_left).visible(show_hidden);
        // Multi-selection membership, cloned once per frame (see render_list).
        let multi = get_panel(self, is_left).multi.clone();
        // Borrow the listing for the duration of the frame instead of cloning
        // it: a 5000-entry folder would otherwise deep-clone every frame.
        let generation = get_panel(self, is_left).generation;
        let entries = std::mem::take(&mut get_panel(self, is_left).entries);
        let sp = ui.spacing().item_spacing.x;
        let cols = (((ui.available_width() + sp) / (CELL_W + sp)).floor() as usize).max(1);
        // Visible names in display order — Shift+click ranges follow what the
        // user sees, not the raw listing.
        let vis_names: Vec<String> = vis.iter().map(|&j| entries[j].name.clone()).collect();

        egui::ScrollArea::vertical()
            .id_salt(if is_left { "grid_left" } else { "grid_right" })
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if vis.is_empty() {
                    weak(ui, crate::lang::tr("空目录", self.lang));
                }
                let mut start = 0usize;
                while start < vis.len() {
                    let end = (start + cols).min(vis.len());
                    ui.horizontal(|ui| {
                        for &i in &vis[start..end] {
                            let e = &entries[i];
                            let is_sel = multi.contains(&e.name);
                            self.grid_cell(ui, is_left, e, is_sel, &vis_names);
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
        vis_names: &[String],
    ) {
        let name = e.name.clone();
        let is_dir = e.is_dir;
        let path = get_panel(self, is_left).cwd.join(&name);
        let fill = if is_sel {
            ui.visuals().selection.bg_fill
        } else {
            egui::Color32::TRANSPARENT
        };
        // Native (full-colour) icon for this cell, at grid size.
        let tex = self
            .icons
            .get(ui.ctx(), shellicon::Kind::for_entry(is_dir, &e.ext));

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
                            egui::vec2(CELL_W - 18.0, CELL_H - 32.0),
                            egui::Sense::hover(),
                        );
                        if is_dir {
                            if let Some(id) = tex {
                                draw_tex(ui.painter(), id, rect, LARGE_ICON);
                            } else {
                                draw_icon(
                                    ui.painter(),
                                    IconKind::Folder,
                                    rect,
                                    egui::Color32::from_rgb(225, 180, 70),
                                    LARGE_ICON,
                                );
                            }
                        } else if crate::preview::is_image(&name) {
                            let mut painted = false;
                            if let Some(t) = self.previews.get(&path, preview::THUMB) {
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
                            if !painted {
                                // Thumbnail still decoding: show the shell's own
                                // icon for this image type instead of a stub.
                                if let Some(id) = tex {
                                    draw_tex(ui.painter(), id, rect, LARGE_ICON);
                                } else {
                                    draw_icon(
                                        ui.painter(),
                                        IconKind::Image,
                                        rect,
                                        ui.visuals().weak_text_color(),
                                        LARGE_ICON,
                                    );
                                }
                            }
                        } else {
                            if let Some(id) = tex {
                                draw_tex(ui.painter(), id, rect, LARGE_ICON);
                            } else {
                                draw_icon(
                                    ui.painter(),
                                    IconKind::File,
                                    rect,
                                    ui.visuals().text_color(),
                                    LARGE_ICON,
                                );
                            }
                        }
                        // Painter-drawn caption instead of a `Label`: a selectable
                        // label would be an interactive widget stealing the
                        // click/double-click on the thumbnail name.
                        let cap_rect = ui
                            .allocate_exact_size(
                                egui::vec2(CELL_W - 18.0, 16.0),
                                egui::Sense::hover(),
                            )
                            .0;
                        let font_id = egui::TextStyle::Small.resolve(ui.style());
                        let color = ui.visuals().text_color();
                        cell_text_with(ui, cap_rect, name.clone(), font_id, color);
                    },
                );
            });
        let resp = cell.response.interact(egui::Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.double_clicked() {
            self.open_row(is_left, &name, is_dir);
        } else if resp.clicked() {
            // Same modifier rules as the list view (Ctrl / Shift / plain).
            let (ctrl, shift) = ui.input(|i| (i.modifiers.ctrl, i.modifiers.shift));
            self.set_active_pane(is_left);
            get_panel(self, is_left).click_select(&name, ctrl, shift, vis_names);
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

    /// Double-click / Enter on a row.
    fn open_row(&mut self, is_left: bool, name: &str, is_dir: bool) {
        self.set_active_pane(is_left);
        let path = get_panel(self, is_left).cwd.join(name);
        if is_dir {
            get_panel(self, is_left).navigate(path);
            self.apply_cached_sizes();
        } else if crate::preview::is_image(name) {
            self.open_viewer(path);
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
                weak(ui, crate::lang::tr("文件夹", self.lang));
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

        if !is_dir && crate::preview::is_image(&name) {
            if let Some(t) = self.previews.get(&path, preview::PREVIEW) {
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
            } else if self.previews.failed(&path, preview::PREVIEW) {
                weak(ui, crate::lang::tr("无法解码此图片", self.lang));
            } else {
                weak(ui, crate::lang::tr("解码中…", self.lang));
                ui.ctx().request_repaint();
            }
            if ui.button(crate::lang::tr("放大查看", self.lang)).clicked() {
                self.open_viewer(path.clone());
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
fn name_cell(
    ui: &mut egui::Ui,
    width: f32,
    name: impl Into<String>,
    kind: IconKind,
    tex: Option<egui::TextureId>,
) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, ROW_H), egui::Sense::hover());
    let icon_rect = egui::Rect::from_min_size(
        egui::pos2(rect.min.x, rect.min.y),
        egui::vec2(ICON_W, ROW_H),
    );
    // The shell's icon when we have one; the vector glyph is only the fallback
    // for platforms without a shell (or an unusual file the shell has no
    // glyph for).
    let drew = if let Some(id) = tex {
        draw_tex(ui.painter(), id, icon_rect, LIST_ICON);
        true
    } else {
        false
    };
    if !drew {
        let icon_color = match kind {
            IconKind::Folder => egui::Color32::from_rgb(225, 180, 70),
            _ => ui.visuals().text_color(),
        };
        draw_icon(ui.painter(), kind, icon_rect, icon_color, LIST_ICON);
    }
    let text_rect = egui::Rect::from_min_max(
        egui::pos2(rect.min.x + ICON_W, rect.min.y),
        egui::pos2(rect.max.x - CELL_PAD, rect.max.y),
    );
    cell_text(ui, text_rect, name, false);
    col_divider(ui, rect);
    ui.interact(rect, egui::Id::new(("name_cell", rect.min.x as i32)), egui::Sense::hover())
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
    let font = egui::TextStyle::Small.resolve(ui.style());
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

/// Common user folders that exist (Desktop / Downloads / Documents / …).
fn known_folders(lang: crate::lang::Lang) -> Vec<(String, PathBuf)> {
    let up = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut out = vec![];
    // No emoji in the label: the row shows the shell's own full-colour folder
    // icon instead, so the text stays plain and selectable-free.
    for (word, name) in [
        ("桌面", "Desktop"),
        ("下载", "Downloads"),
        ("文档", "Documents"),
        ("图片", "Pictures"),
        ("音乐", "Music"),
        ("视频", "Videos"),
    ] {
        let p = up.join(name);
        if p.is_dir() {
            out.push((crate::lang::tr(word, lang).to_string(), p));
        }
    }
    out
}
