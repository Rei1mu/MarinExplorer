//! Vector icon glyphs for the app chrome (toolbar, side rails, tabs, grid).
//!
//! # Licensing — free for commercial use
//! The glyph geometry is taken from **Lucide** (<https://lucide.dev>), which is
//! published under the **ISC licence**: commercial use is allowed, there is no
//! fee and no share-alike requirement. The only obligation is to keep the
//! notice, which is reproduced here.
//!
//! ```text
//! ISC License
//!
//! Copyright (c) for portions of Lucide are held by Cole Bemis 2013-2022 as
//! part of Feather (MIT). All other copyright (c) for Lucide are held by
//! their respective copyright holders, 2024. All rights reserved.
//!
//! Permission to use, copy, modify, and/or distribute this software for any
//! purpose with or without fee is hereby granted, provided that the above
//! copyright notice and this permission notice appear in all copies.
//!
//! THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
//! WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
//! MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
//! ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
//! WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
//! ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
//! OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
//! ```
//!
//! Nothing here is a *font*: the shapes are stroked vector paths rendered by
//! egui's tessellator, so they scale cleanly and never depend on which emoji
//! font happens to be installed.

use std::collections::HashMap;
use std::sync::OnceLock;

use egui::{epaint::PathShape, Color32, Painter, Pos2, Rect, Shape, StrokeKind};

/// Every glyph is authored on a 24×24 grid, like the source icon set.
const VIEW: f32 = 24.0;
/// Stroke weight on that grid (the source set uses 2 as well).
const WEIGHT: f32 = 2.0;

/// One stroke-only vector glyph.
#[derive(Clone, Copy)]
pub struct Icon(pub &'static str);

// ---------------------------------------------------------------- glyph set

pub const BACK: Icon = Icon("M19 12H5M12 19l-7-7 7-7");
pub const FORWARD: Icon = Icon("M5 12h14M12 5l7 7-7 7");
pub const UP: Icon = Icon("M12 19V5M5 12l7-7 7 7");
pub const REFRESH: Icon = Icon(
    "M21 12a9 9 0 1 1-9-9c2.52 0 4.93 1 6.74 2.74L21 8M21 3v5h-5",
);
/// 重新计算大小: a storage cylinder, deliberately unlike REFRESH's circular
/// arrow so the two toolbar buttons next to each other stay tellable apart.
pub const SIZE: Icon = Icon(
    "M12 8c4.97 0 9-1.34 9-3s-4.03-3-9-3-9 1.34-9 3 4.03 3 9 3zM21 5v6c0 1.66-4.03 3-9 3s-9-1.34-9-3V5M21 11v6c0 1.66-4.03 3-9 3s-9-1.34-9-3v-6",
);
pub const COPY: Icon = Icon(
    "M20 9h-9a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h9a2 2 0 0 0 2-2v-9a2 2 0 0 0-2-2zM5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1",
);
/// 剪贴 (cut): a pair of scissors — the dedicated glyph for the cut-to-clipboard
/// action, kept visually apart from 复制/粘贴 so the three toolbar buttons read
/// at a glance.
pub const CUT: Icon = Icon(
    "M6 4a2 2 0 1 0 0 4 2 2 0 1 0 0-4M6 16a2 2 0 1 0 0 4 2 2 0 1 0 0-4M6 6l12 12M6 18l12-12",
);
pub const PASTE: Icon = Icon(
    "M15 2H9a1 1 0 0 0-1 1v2a1 1 0 0 0 1 1h6a1 1 0 0 0 1-1V3a1 1 0 0 0-1-1zM8 4H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2M16 4h2a2 2 0 0 1 2 2v2M9 14h10",
);
pub const TRASH: Icon = Icon(
    "M3 6h18M19 6v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2M10 11v6M14 11v6",
);
pub const FOLDER_PLUS: Icon = Icon(
    "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2zM12 10v6M9 13h6",
);
pub const FOLDER: Icon = Icon(
    "M20 20a2 2 0 0 0 2-2V8a2 2 0 0 0-2-2h-7.9a2 2 0 0 1-1.69-.9L9.6 3.9A2 2 0 0 0 7.93 3H4a2 2 0 0 0-2 2v13a2 2 0 0 0 2 2z",
);
pub const FILE: Icon = Icon("M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8zM14 2v6h6");
pub const PLUS: Icon = Icon("M5 12h14M12 5v14");
pub const PIN: Icon = Icon(
    "M12 17v5M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V7a1 1 0 0 1 1-1 2 2 0 0 0 0-4H8a2 2 0 0 0 0 4 1 1 0 0 1 1 1z",
);
pub const NAV: Icon = Icon("M3 11l19-9-9 19-2-8-8-2z");
pub const TERMINAL: Icon = Icon("m4 17 6-6-6-6M12 19h8");
pub const CLOCK: Icon = Icon("M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20zM12 6v6l4 2");
pub const STAR: Icon = Icon(
    "M11.525 2.295a.53.53 0 0 1 .95 0l2.31 4.679a2.123 2.123 0 0 0 1.595 1.16l5.166.756a.53.53 0 0 1 .294.904l-3.736 3.638a2.123 2.123 0 0 0-.611 1.878l.882 5.14a.53.53 0 0 1-.771.56l-4.618-2.428a2.122 2.122 0 0 0-1.973 0L6.396 21.16a.53.53 0 0 1-.77-.56l.881-5.139a2.122 2.122 0 0 0-.611-1.879L2.16 12.944a.53.53 0 0 1 .294-.904l5.165-.755a2.122 2.122 0 0 0 1.597-1.16z",
);
/// 收藏: a heart — the user's own list, kept apart from Windows' pins.
pub const HEART: Icon = Icon(
    "M19 14c1.49-1.46 3-3.21 3-5.5A5.5 5.5 0 0 0 16.5 3c-1.76 0-3 .5-4.5 2-1.5-1.5-2.74-2-4.5-2A5.5 5.5 0 0 0 2 8.5c0 2.3 1.5 4.05 3 5.5l7 7z",
);
pub const X: Icon = Icon("M18 6 6 18M6 6l12 12");
pub const SLIDERS: Icon = Icon("M21 4h-7M10 4H3M21 12h-9M8 12H3M21 20h-5M12 20H3M14 2v4M8 10v4M16 18v4");
pub const GRID: Icon = Icon("M3 3h7v7H3zM14 3h7v7h-7zM3 14h7v7H3zM14 14h7v7h-7z");
pub const LIST: Icon = Icon("M8 6h13M8 12h13M8 18h13M3 6h.01M3 12h.01M3 18h.01");
pub const HOME: Icon = Icon("m3 9 9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2zM9 22V12h6v10");
pub const DRIVE: Icon = Icon(
    "M22 12H2M5.45 5.11 2 12v6a2 2 0 0 0 2 2h16a2 2 0 0 0 2-2v-6l-3.45-6.89A2 2 0 0 0 16.76 4H7.24a2 2 0 0 0-1.79 1.11zM6 16h.01M10 16h.01",
);
pub const POWER: Icon = Icon("M12 3v9M18.36 6.64a9 9 0 1 1-12.73 0");
pub const CHEV_LEFT: Icon = Icon("m15 18-6-6 6-6");
pub const CHEV_RIGHT: Icon = Icon("m9 18 6-6-6-6");
pub const ZOOM_IN: Icon = Icon("M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16zM21 21l-4.35-4.35M11 8v6M8 11h6");
pub const ZOOM_OUT: Icon = Icon("M11 19a8 8 0 1 0 0-16 8 8 0 0 0 0 16zM21 21l-4.35-4.35M8 11h6");
pub const PENCIL: Icon = Icon("M17 3a2.85 2.83 0 1 1 4 4L7.5 20.5 2 22l1.5-5.5zM15 5l4 4");
pub const EYE: Icon = Icon(
    "M2.062 12.348a1 1 0 0 1 0-.696 10.75 10.75 0 0 1 19.876 0 1 1 0 0 1 0 .696 10.75 10.75 0 0 1-19.876 0zM15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0z",
);

// ---------------------------------------------------------------- widgets

/// A toolbar button: stroked glyph + a plain (non-selectable) text label.
///
/// The label is painted with the painter on purpose — a `Label` is selectable,
/// which turns it into an interactive widget that swallows clicks landing on
/// the glyphs and shows a text I-beam cursor.
pub fn button(
    ui: &mut egui::Ui,
    ic: &Icon,
    label: &str,
    enabled: bool,
    hint: &str,
) -> egui::Response {
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let color = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id, color);
    let pad = ui.spacing().button_padding;
    let ic_w = 15.0;
    let h = (galley.size().y + pad.y * 2.0).max(23.0);
    let size = egui::vec2(ic_w + 5.0 + galley.size().x + pad.x * 2.0, h);
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let visuals = ui.style().visuals.widgets.style(&resp);
    if ui.is_rect_visible(rect) {
        if visuals.weak_bg_fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        }
        if visuals.bg_stroke.width > 0.0 {
            ui.painter().rect_stroke(
                rect,
                visuals.corner_radius,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
        }
        let ic_rect = Rect::from_min_size(
            egui::pos2(rect.min.x + pad.x, rect.center().y - ic_w * 0.5),
            egui::vec2(ic_w, ic_w),
        );
        let c = visuals.text_color();
        paint(ui.painter(), ic.0, ic_rect, c, WEIGHT);
        let pos = egui::pos2(
            rect.min.x + pad.x + ic_w + 5.0,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter()
            .with_clip_rect(rect)
            .galley(pos, galley, visuals.text_color());
    }
    if hint.is_empty() {
        resp
    } else {
        resp.on_hover_text(hint)
    }
}

/// Square glyph-only button (used for the small "×" closers).
pub fn small_button(ui: &mut egui::Ui, ic: &Icon, hint: &str) -> egui::Response {
    let size = ui.spacing().icon_width.max(16.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let visuals = ui.style().visuals.widgets.style(&resp);
    if ui.is_rect_visible(rect) {
        if visuals.weak_bg_fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        }
        if visuals.bg_stroke.width > 0.0 {
            ui.painter().rect_stroke(
                rect,
                visuals.corner_radius,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
        }
        let ic_rect = Rect::from_center_size(rect.center(), egui::vec2(size - 7.0, size - 7.0));
        paint(ui.painter(), ic.0, ic_rect, visuals.text_color(), WEIGHT);
    }
    if hint.is_empty() {
        resp
    } else {
        resp.on_hover_text(hint)
    }
}

/// A square glyph-only button that stays highlighted while `selected` (view
/// switcher: 列表 / 缩略图).
pub fn toggle_button(
    ui: &mut egui::Ui,
    ic: &Icon,
    selected: bool,
    hint: &str,
) -> egui::Response {
    let size = ui.spacing().icon_width.max(16.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    let mut visuals = *ui.style().visuals.widgets.style(&resp);
    if selected {
        visuals.weak_bg_fill = ui.visuals().selection.bg_fill;
        visuals.bg_stroke.color = ui.visuals().selection.stroke.color;
        visuals.fg_stroke.color = ui.visuals().strong_text_color();
    }
    if ui.is_rect_visible(rect) {
        if visuals.weak_bg_fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        }
        if visuals.bg_stroke.width > 0.0 {
            ui.painter().rect_stroke(
                rect,
                visuals.corner_radius,
                visuals.bg_stroke,
                StrokeKind::Inside,
            );
        }
        let ic_rect = Rect::from_center_size(rect.center(), egui::vec2(size - 7.0, size - 7.0));
        paint(ui.painter(), ic.0, ic_rect, visuals.text_color(), WEIGHT);
    }
    if hint.is_empty() {
        resp
    } else {
        resp.on_hover_text(hint)
    }
}

/// What a rail/list row shows in its leading 16px slot.
#[derive(Clone, Copy)]
pub enum Glyph {
    /// A vector glyph (used when there is no native icon).
    Svg(&'static Icon),
    /// A native (shell) icon texture, already uploaded to egui.
    Tex(egui::TextureId),
}

/// A selectable row (side rail / quick access): a leading icon + plain text.
pub fn entry(
    ui: &mut egui::Ui,
    glyph: Glyph,
    label: &str,
    selected: bool,
    hint: &str,
    max_w: f32,
) -> egui::Response {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_string(), font_id.clone(), color);
    let pad = ui.spacing().button_padding;
    let ic_w = 16.0;
    let h = (galley.size().y + pad.y * 2.0).max(22.0);
    let w = (ic_w + 6.0 + galley.size().x + pad.x * 2.0).min(max_w.max(40.0));
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click());
    let mut visuals = *ui.style().visuals.widgets.style(&resp);
    if selected {
        visuals.weak_bg_fill = ui.visuals().selection.bg_fill;
        visuals.fg_stroke.color = ui.visuals().strong_text_color();
    }
    if ui.is_rect_visible(rect) {
        if visuals.weak_bg_fill != Color32::TRANSPARENT {
            ui.painter()
                .rect_filled(rect, visuals.corner_radius, visuals.weak_bg_fill);
        }
        let ic_rect = Rect::from_min_size(
            egui::pos2(rect.min.x + pad.x, rect.center().y - ic_w * 0.5),
            egui::vec2(ic_w, ic_w),
        );
        match glyph {
            Glyph::Svg(ic) => paint(ui.painter(), ic.0, ic_rect, visuals.text_color(), WEIGHT),
            Glyph::Tex(id) => {
                let s = ic_rect.width().min(ic_rect.height());
                ui.painter().image(
                    id,
                    Rect::from_center_size(ic_rect.center(), egui::vec2(s, s)),
                    Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        let pos = egui::pos2(
            rect.min.x + pad.x + ic_w + 6.0,
            rect.center().y - galley.size().y * 0.5,
        );
        ui.painter()
            .with_clip_rect(rect)
            .galley(pos, galley, visuals.text_color());
    }
    if hint.is_empty() {
        resp
    } else {
        resp.on_hover_text(hint)
    }
}

/// A group heading *inside* a rail section — smaller and weaker than [`label`],
/// and nudged right, so it reads as nested under its parent section. Windows
/// Explorer splits Quick Access into "已固定" and "最近使用"; this draws those.
pub fn sub_label(ui: &mut egui::Ui, ic: &Icon, text: &str, weak: egui::Color32) {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = weak;
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font_id, color);
    let ic_w = 12.0;
    let indent = 6.0;
    let h = galley.size().y.max(16.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(indent + ic_w + 5.0 + galley.size().x, h),
        egui::Sense::hover(),
    );
    let ic_rect = Rect::from_min_size(
        egui::pos2(rect.min.x + indent, rect.center().y - ic_w * 0.5),
        egui::vec2(ic_w, ic_w),
    );
    paint(ui.painter(), ic.0, ic_rect, color, WEIGHT);
    let pos = egui::pos2(
        rect.min.x + indent + ic_w + 5.0,
        rect.center().y - galley.size().y * 0.5,
    );
    ui.painter()
        .with_clip_rect(rect)
        .galley(pos, galley, color);
}

/// A section heading: stroked glyph + emphasised text (no widget, so it can't
/// swallow clicks).
pub fn label(ui: &mut egui::Ui, ic: &Icon, text: &str) {
    let font_id = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().strong_text_color();
    let galley = ui
        .painter()
        .layout_no_wrap(text.to_string(), font_id, color);
    let ic_w = 15.0;
    let h = galley.size().y.max(20.0);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ic_w + 6.0 + galley.size().x, h), egui::Sense::hover());
    let ic_rect = Rect::from_min_size(
        egui::pos2(rect.min.x, rect.center().y - ic_w * 0.5),
        egui::vec2(ic_w, ic_w),
    );
    paint(ui.painter(), ic.0, ic_rect, color, WEIGHT);
    let pos = egui::pos2(rect.min.x + ic_w + 6.0, rect.center().y - galley.size().y * 0.5);
    ui.painter()
        .with_clip_rect(rect)
        .galley(pos, galley, color);
}

// ---------------------------------------------------------------- renderer

/// Parsed-and-flattened geometry: one polyline per SVG sub-path.
struct Sub {
    pts: Vec<(f32, f32)>,
    closed: bool,
}

/// Flattened geometries are parsed once per distinct path string and then
/// reused — parsing 30 short paths every frame would be wasteful.
///
/// NOTE: `Arc`, not `Rc`. The cache lives in a `static`, so its contents must
/// be `Send`; `Rc` is not, and the borrow checker rejects the whole static
/// (`shared static variables must have a type that implements Sync`).
fn cache() -> &'static std::sync::Mutex<HashMap<&'static str, std::sync::Arc<Vec<Sub>>>> {
    static C: OnceLock<std::sync::Mutex<HashMap<&'static str, std::sync::Arc<Vec<Sub>>>>> = OnceLock::new();
    C.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Returns a shared, ref-counted copy of the parsed geometry. Painting a glyph
/// used to deep-copy the whole `Vec<Sub>` (with its `Vec<(f32, f32)>`) on
/// every call; now only the `Arc` is cloned (a refcount bump). The per-glyph
/// point transform inside `paint` still allocates its own `Vec<Pos2>`.
fn geometry(d: &'static str) -> std::sync::Arc<Vec<Sub>> {
    if let Ok(guard) = cache().lock() {
        if let Some(v) = guard.get(d) {
            return v.clone();
        }
    }
    let subs = std::sync::Arc::new(parse(d));
    if let Ok(mut guard) = cache().lock() {
        guard.insert(d, subs.clone());
    }
    subs
}

/// Stroke `d` (24×24 grid) into `rect`, centred, with `w` the stroke weight in
/// grid units.
pub fn paint(painter: &Painter, d: &'static str, rect: Rect, color: Color32, w: f32) {
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let subs = geometry(d);
    let size = rect.width().min(rect.height());
    let s = size / VIEW;
    let c = rect.center();
    let stroke_w = (w * s).max(0.75);
    for sub in subs.iter() {
        let pts: Vec<Pos2> = sub
            .pts
            .iter()
            .map(|&(x, y)| Pos2::new(c.x + (x - 12.0) * s, c.y + (y - 12.0) * s))
            .collect();
        if pts.len() == 2 {
            // A degenerate "M x y h .01" in the source set: a dot accent.
            let d2 = (pts[0].x - pts[1].x).hypot(pts[0].y - pts[1].y);
            if d2 < 0.9 {
                painter.circle_filled(pts[0], stroke_w * 0.6, color);
                continue;
            }
        }
        if pts.len() < 2 {
            continue;
        }
        let mut path = PathShape::line(pts, egui::Stroke::new(stroke_w, color));
        path.closed = sub.closed;
        painter.add(Shape::Path(path));
    }
}

// ---------------------------------------------------------------- parser

type Pt = (f32, f32);

struct P<'a> {
    b: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn eof(&self) -> bool {
        self.i >= self.b.len()
    }
    fn peek(&self) -> u8 {
        if self.eof() {
            0
        } else {
            self.b[self.i]
        }
    }
    fn skip_sep(&mut self) {
        while !self.eof() {
            match self.b[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' | b',' => self.i += 1,
                _ => break,
            }
        }
    }
    fn num(&mut self) -> f32 {
        self.skip_sep();
        let neg = if self.peek() == b'-' {
            self.i += 1;
            true
        } else {
            false
        };
        let mut v = 0.0f32;
        let mut any = false;
        while !self.eof() && self.b[self.i].is_ascii_digit() {
            v = v * 10.0 + (self.b[self.i] - b'0') as f32;
            self.i += 1;
            any = true;
        }
        if self.peek() == b'.' {
            self.i += 1;
            let mut f = 0.1f32;
            while !self.eof() && self.b[self.i].is_ascii_digit() {
                v += (self.b[self.i] - b'0') as f32 * f;
                f *= 0.1;
                self.i += 1;
                any = true;
            }
        }
        if !any {
            self.i += 1; // never stall on a stray character
        }
        if neg {
            -v
        } else {
            v
        }
    }
    fn flag(&mut self) -> bool {
        self.skip_sep();
        let c = self.peek();
        self.i += 1;
        c == b'1'
    }
}

/// Parse an SVG `d` attribute into flattened sub-paths (curves and arcs are
/// sampled into polylines: at 16–32px it is indistinguishable and it keeps the
/// tessellator simple).
fn parse(d: &str) -> Vec<Sub> {
    let mut p = P { b: d.as_bytes(), i: 0 };
    let (mut cur_pt, mut start_pt) = ((0.0, 0.0), (0.0, 0.0));
    let mut prev_ctrl: Option<Pt> = None;
    let mut prev_cmd = 'M';
    let mut pts: Vec<Pt> = vec![];
    let mut out: Vec<Sub> = vec![];
    let mut closed = false;
    while !p.eof() {
        p.skip_sep();
        if p.eof() {
            break;
        }
        let c = p.peek() as char;
        let explicit = c.is_ascii_alphabetic();
        if explicit {
            p.i += 1;
        }
        let cmd = if explicit { c } else { prev_cmd };
        let up = cmd.to_ascii_uppercase();
        let rel = cmd.is_ascii_lowercase();
        prev_cmd = cmd;

        // Coordinates for the curve commands arrive in (x,y) pairs.
        let abs = |p: &mut P, rel: bool, cur: Pt| -> Pt {
            let a = (p.num(), p.num());
            if rel {
                (cur.0 + a.0, cur.1 + a.1)
            } else {
                a
            }
        };

        match up {
            'M' | 'L' | 'T' => {
                let a = abs(&mut p, rel, cur_pt);
                if up == 'M' {
                    if !pts.is_empty() {
                        out.push(Sub { pts: std::mem::take(&mut pts), closed });
                    }
                    closed = false;
                    start_pt = a;
                    // The moveto point *is* the first point of the new sub-path;
                    // without it the leading segment of every glyph went missing
                    // (arrows drew as bare chevrons, boxes as bare frames).
                    pts.push(a);
                    prev_ctrl = None;
                    // Extra coordinate pairs after a moveto are *implicit*
                    // linetos ("M18 6 6 18" is a move plus one line). Without
                    // this the parser would start a fresh sub-path per pair,
                    // leaving every point a 1-point stub — which is exactly why
                    // the X / move / grid glyphs used to draw as nothing.
                    prev_cmd = if rel { 'l' } else { 'L' };
                } else if up == 'T' {
                    let ctrl = prev_ctrl.unwrap_or(cur_pt);
                    let reflect = (2.0 * cur_pt.0 - ctrl.0, 2.0 * cur_pt.1 - ctrl.1);
                    quad(&mut pts, cur_pt, reflect, a);
                    // The reflection is the control point the *next* T continues
                    // from; L and M break the chain instead (see the two others).
                    prev_ctrl = Some(reflect);
                } else {
                    pts.push(a);
                    prev_ctrl = None;
                }
                cur_pt = a;
            }
            'H' => {
                let x = p.num();
                let a = if rel { (cur_pt.0 + x, cur_pt.1) } else { (x, cur_pt.1) };
                pts.push(a);
                cur_pt = a;
                prev_ctrl = None;
            }
            'V' => {
                let y = p.num();
                let a = if rel { (cur_pt.0, cur_pt.1 + y) } else { (cur_pt.0, y) };
                pts.push(a);
                cur_pt = a;
                prev_ctrl = None;
            }
            'C' => {
                let c1 = abs(&mut p, rel, cur_pt);
                let c2 = abs(&mut p, rel, cur_pt);
                let a = abs(&mut p, rel, cur_pt);
                cubic(&mut pts, cur_pt, c1, c2, a);
                prev_ctrl = Some(c2);
                cur_pt = a;
            }
            'S' => {
                let c2 = abs(&mut p, rel, cur_pt);
                let c1 = match prev_ctrl {
                    Some(c) => (2.0 * cur_pt.0 - c.0, 2.0 * cur_pt.1 - c.1),
                    None => cur_pt,
                };
                let a = abs(&mut p, rel, cur_pt);
                cubic(&mut pts, cur_pt, c1, c2, a);
                prev_ctrl = Some(c2);
                cur_pt = a;
            }
            'Q' => {
                let c1 = abs(&mut p, rel, cur_pt);
                let a = abs(&mut p, rel, cur_pt);
                quad(&mut pts, cur_pt, c1, a);
                prev_ctrl = Some(c1);
                cur_pt = a;
            }
            'A' => {
                let rx = p.num();
                let ry = p.num();
                let rot = p.num();
                let large = p.flag();
                let sweep = p.flag();
                let a = abs(&mut p, rel, cur_pt);
                arc(&mut pts, cur_pt, rx, ry, rot, large, sweep, a);
                cur_pt = a;
                prev_ctrl = None;
            }
            'Z' => {
                if !pts.is_empty() {
                    pts.push(start_pt);
                    out.push(Sub { pts: std::mem::take(&mut pts), closed: true });
                }
                closed = false;
                cur_pt = start_pt;
                prev_ctrl = None;
            }
            _ => {
                // Unknown command: bail out rather than loop forever.
                break;
            }
        }
    }
    if !pts.is_empty() {
        out.push(Sub { pts, closed });
    }
    out
}

/// Sample a cubic Bézier into the polyline (skipping the duplicated start).
fn cubic(pts: &mut Vec<Pt>, p0: Pt, p1: Pt, p2: Pt, p3: Pt) {
    const N: usize = 10;
    for k in 1..=N {
        let t = k as f32 / N as f32;
        let u = 1.0 - t;
        let x = u * u * u * p0.0 + 3.0 * u * u * t * p1.0 + 3.0 * u * t * t * p2.0 + t * t * t * p3.0;
        let y = u * u * u * p0.1 + 3.0 * u * u * t * p1.1 + 3.0 * u * t * t * p2.1 + t * t * t * p3.1;
        pts.push((x, y));
    }
}

/// Sample a quadratic Bézier.
fn quad(pts: &mut Vec<Pt>, p0: Pt, p1: Pt, p2: Pt) {
    const N: usize = 8;
    for k in 1..=N {
        let t = k as f32 / N as f32;
        let u = 1.0 - t;
        let x = u * u * p0.0 + 2.0 * u * t * p1.0 + t * t * p2.0;
        let y = u * u * p0.1 + 2.0 * u * t * p1.1 + t * t * p2.1;
        pts.push((x, y));
    }
}

/// Sample an elliptical arc (endpoint → centre parameterisation, SVG spec
/// F.6.5). Coordinates keep SVG's y-down orientation, which is also egui's.
fn arc(pts: &mut Vec<Pt>, p0: Pt, rx: f32, ry: f32, rot: f32, large: bool, sweep: bool, p1: Pt) {
    if (p0.0 - p1.0).abs() < 1e-6 && (p0.1 - p1.1).abs() < 1e-6 {
        return;
    }
    let mut rx = rx.abs();
    let mut ry = ry.abs();
    if rx < 1e-6 || ry < 1e-6 {
        pts.push(p1);
        return;
    }
    let phi = rot * std::f32::consts::PI / 180.0;
    let (cos_p, sin_p) = (phi.cos(), phi.sin());
    let dx2 = (p0.0 - p1.0) / 2.0;
    let dy2 = (p0.1 - p1.1) / 2.0;
    let x1p = cos_p * dx2 + sin_p * dy2;
    let y1p = -sin_p * dx2 + cos_p * dy2;
    let mut rx2 = rx * rx;
    let mut ry2 = ry * ry;
    let lambda = (x1p * x1p) / rx2 + (y1p * y1p) / ry2;
    if lambda > 1.0 {
        let s = lambda.sqrt();
        rx *= s;
        ry *= s;
        rx2 = rx * rx;
        ry2 = ry * ry;
    }
    let sign = if large == sweep { -1.0 } else { 1.0 };
    let num = rx2 * ry2 - rx2 * y1p * y1p - ry2 * x1p * x1p;
    let den = rx2 * y1p * y1p + ry2 * x1p * x1p;
    let co = if den <= 0.0 {
        0.0
    } else {
        sign * (num / den).max(0.0).sqrt()
    };
    let cxp = co * (rx * y1p) / ry;
    let cyp = -co * (ry * x1p) / rx;
    let cx = cos_p * cxp - sin_p * cyp + (p0.0 + p1.0) / 2.0;
    let cy = sin_p * cxp + cos_p * cyp + (p0.1 + p1.1) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| -> f32 {
        let d = (ux * vx + uy * vy) / ((ux * ux + uy * uy).sqrt() * (vx * vx + vy * vy).sqrt());
        let mut a = d.clamp(-1.0, 1.0).acos();
        if ux * vy - uy * vx < 0.0 {
            a = -a;
        }
        a
    };
    let th1 = ang(1.0, 0.0, (x1p - cxp) / rx, (y1p - cyp) / ry);
    let mut dth = ang(
        (x1p - cxp) / rx,
        (y1p - cyp) / ry,
        (-x1p - cxp) / rx,
        (-y1p - cyp) / ry,
    );
    if !sweep && dth > 0.0 {
        dth -= 2.0 * std::f32::consts::PI;
    } else if sweep && dth < 0.0 {
        dth += 2.0 * std::f32::consts::PI;
    }
    let n = ((dth.abs() / (std::f32::consts::PI / 18.0)).ceil() as usize).clamp(4, 96);
    for k in 1..=n {
        let t = th1 + dth * (k as f32 / n as f32);
        let x = cx + rx * t.cos() * cos_p - ry * t.sin() * sin_p;
        let y = cy + rx * t.cos() * sin_p + ry * t.sin() * cos_p;
        pts.push((x, y));
    }
}
