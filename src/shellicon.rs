//! Native, full-colour file icons taken straight from the Windows shell, plus
//! the dark-window-frame handling.
//!
//! # Licensing — free for commercial use
//! Nothing from Windows is **bundled or redistributed** here: the icons are
//! queried at runtime with `SHGetFileInfoW`, i.e. the app asks the shell "what
//! icon do you show for this thing" and draws whatever it gets back — the same
//! thing every file manager (including Explorer itself) does. There is
//! therefore no Microsoft asset to licence. When the query fails, or when the
//! app is built for a non-Windows target, the caller falls back to the vector
//! glyphs in [`crate::icons`] (ISC licensed).

use std::collections::HashMap;
use std::path::PathBuf;

/// What we want an icon for. Cheap kinds avoid touching the disk, which
/// matters because a folder can hold thousands of rows.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Kind {
    /// The generic folder glyph (no disk access, no `desktop.ini` lookup).
    Folder,
    /// Icon of a file type, addressed by extension (`"png"`).
    Ext(String),
    /// A file with no extension.
    GenericFile,
    /// The icon of one concrete path (drives, `.exe`, `.ico`, known folders).
    Path(PathBuf),
    /// The Recycle Bin (asked for by its CLSID, so it keeps its real glyph).
    Recycle,
}

impl Kind {
    /// The kind that fits a directory entry: folders share one cached glyph,
    /// files are keyed by their lower-cased extension so a whole folder of
    /// `.png` files costs a single shell query.
    pub fn for_entry(is_dir: bool, ext: &str) -> Self {
        if is_dir {
            return Kind::Folder;
        }
        if ext.is_empty() {
            return Kind::GenericFile;
        }
        Kind::Ext(ext.to_ascii_lowercase())
    }
}

/// Raw 32-bit ARGB pixels lifted out of an `HICON`.
struct Bmp {
    w: u32,
    h: u32,
    rgba: Vec<u8>,
}

/// A cache of shell icons as egui textures.
pub struct Icons {
    map: HashMap<Kind, Option<egui::TextureHandle>>,
}

impl Default for Icons {
    fn default() -> Self {
        Self { map: HashMap::new() }
    }
}

impl Icons {
    pub fn new() -> Self {
        Self::default()
    }

    /// Texture for `kind`, uploaded on first use. `None` when the shell has no
    /// icon for it (or when the platform has no shell to ask) — callers fall
    /// back to a vector glyph then.
    pub fn get(&mut self, ctx: &egui::Context, kind: Kind) -> Option<egui::TextureId> {
        if let Some(slot) = self.map.get(&kind) {
            return slot.as_ref().map(|t| t.id());
        }
        let tex = extract(&kind).map(|b| {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [b.w as usize, b.h as usize],
                &b.rgba,
            );
            ctx.load_texture(format!("shell:{kind:?}"), img, egui::TextureOptions::LINEAR)
        });
        let id = tex.as_ref().map(|t| t.id());
        self.map.insert(kind, tex);
        id
    }

    /// Drop the cached texture for `kind` so the next `get` re-queries the
    /// shell. Needed when the on-disk state the glyph depends on changes —
    /// e.g. the Recycle Bin flipping between its empty and full icon after
    /// "empty recycle bin".
    pub fn invalidate(&mut self, kind: &Kind) {
        self.map.remove(kind);
    }

}

// ------------------------------------------------------------------ windows

#[cfg(windows)]
#[allow(non_snake_case)]
mod imp {
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;

    use super::{Bmp, Kind};

    const SHGFI_ICON: u32 = 0x0000_0100;
    const SHGFI_LARGEICON: u32 = 0x0000_0000; // 32×32
    /// `psfi` is a PIDL, not a path string.
    const SHGFI_PIDL: u32 = 0x0000_0008;
    const SHGFI_USEFILEATTRIBUTES: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;
    const FILE_ATTRIBUTE_NORMAL: u32 = 0x0000_0080;
    const DIB_RGB_COLORS: u32 = 0;
    const BI_RGB: u32 = 0;

    #[repr(C)]
    struct SHFILEINFOW {
        h_icon: isize,
        i_icon: i32,
        dw_attributes: u32,
        _sz_display_name: [u16; 260],
        _sz_type_name: [u16; 80],
    }

    #[repr(C)]
    struct ICONINFO {
        _f_icon: i32,
        _x_hotspot: u32,
        _y_hotspot: u32,
        hbm_mask: isize,
        hbm_color: isize,
    }

    #[repr(C)]
    struct BITMAP {
        _bm_type: i32,
        bm_width: i32,
        bm_height: i32,
        _bm_width_bytes: i32,
        _bm_planes: u16,
        bm_bits_pixel: u16,
        _bm_bits: usize,
    }

    #[repr(C)]
    struct BITMAPINFOHEADER {
        bi_size: u32,
        bi_width: i32,
        bi_height: i32,
        bi_planes: u16,
        bi_bit_count: u16,
        bi_compression: u32,
        bi_size_image: u32,
        _bi_x_pels_per_meter: i32,
        _bi_y_pels_per_meter: i32,
        bi_clr_used: u32,
        _bi_clr_important: u32,
    }

    #[repr(C)]
    struct BITMAPINFO {
        header: BITMAPINFOHEADER,
        colors: [u32; 3],
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetFileInfoW(
            psz_path: *const u16,
            dw_file_attributes: u32,
            psfi: *mut SHFILEINFOW,
            cb_file_info: u32,
            u_flags: u32,
        ) -> usize;
        /// Parse a display-name string (e.g. `"::{GUID}"`) into a PIDL.
        fn SHParseDisplayName(
            psz_name: *const u16,
            pbc: *mut core::ffi::c_void,
            ppidl: *mut *mut core::ffi::c_void,
            sfgao_in: u32,
            psfgao_out: *mut u32,
        ) -> i32;
    }

    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(pv_reserved: *mut core::ffi::c_void, dw_co_init: u32) -> i32;
        fn CoUninitialize();
        fn CoTaskMemFree(pv: *mut core::ffi::c_void);
    }

    #[link(name = "user32")]
    extern "system" {
        fn GetIconInfo(h_icon: isize, info: *mut ICONINFO) -> i32;
        fn DestroyIcon(h_icon: isize) -> i32;
        fn GetDC(h_wnd: isize) -> isize;
        fn ReleaseDC(h_wnd: isize, h_dc: isize) -> i32;
    }

    #[link(name = "gdi32")]
    extern "system" {
        fn GetObjectW(h: isize, c: i32, pv: *mut u8) -> i32;
        fn DeleteObject(h: isize) -> i32;
        fn GetDIBits(
            hdc: isize,
            hbm: isize,
            start: u32,
            lines: u32,
            bits: *mut u8,
            bmi: *mut BITMAPINFO,
            usage: u32,
        ) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Ask the shell for the 32×32 icon of `kind` and lift its pixels.
    pub fn extract(kind: &Kind) -> Option<Bmp> {
        // The Recycle Bin is a *namespace* item: its CLSID string has to be
        // parsed into a PIDL first, or the shell cannot resolve it (see
        // `recycle_icon`).
        if matches!(kind, Kind::Recycle) {
            return unsafe { recycle_icon() };
        }
        let (path, attrs, synthetic) = match kind {
            Kind::Folder => (String::new(), FILE_ATTRIBUTE_DIRECTORY, true),
            Kind::Ext(e) => (format!("*.{e}"), FILE_ATTRIBUTE_NORMAL, true),
            Kind::GenericFile => (String::new(), FILE_ATTRIBUTE_NORMAL, true),
            Kind::Path(p) => (p.to_string_lossy().into_owned(), 0, false),
            Kind::Recycle => unreachable!("handled above"),
        };
        // With SHGFI_USEFILEATTRIBUTES the shell never touches the disk, but it
        // also cannot see an empty path — use a harmless placeholder.
        let path = if synthetic && path.is_empty() {
            "x".to_string()
        } else {
            path
        };
        let w = wide(&path);
        unsafe {
            let mut sfi: SHFILEINFOW = std::mem::zeroed();
            let mut flags = SHGFI_ICON | SHGFI_LARGEICON;
            if synthetic {
                flags |= SHGFI_USEFILEATTRIBUTES;
            }
            let ok = SHGetFileInfoW(
                w.as_ptr(),
                attrs,
                &mut sfi,
                size_of::<SHFILEINFOW>() as u32,
                flags,
            );
            if ok == 0 || sfi.h_icon == 0 {
                if sfi.h_icon != 0 {
                    DestroyIcon(sfi.h_icon);
                }
                return None;
            }
            let bmp = pixels(sfi.h_icon);
            DestroyIcon(sfi.h_icon);
            bmp
        }
    }

    /// The real Recycle Bin glyph, resolved through its namespace CLSID.
    ///
    /// The CLSID string must be parsed into a PIDL first
    /// (`SHParseDisplayName`) and queried *without* `SHGFI_USEFILEATTRIBUTES`.
    /// With that flag the shell never parses the `"::{GUID}"` namespace string
    /// and answers with the plain folder icon (`FILE_ATTRIBUTE_DIRECTORY`) —
    /// which is why the rail entry used to wear a folder glyph. The namespace
    /// icon is also state-aware (empty vs. full bin); the cache invalidation in
    /// `Explorer` re-queries it after the bin changes.
    ///
    /// COM is initialised around the call (STA): `SHParseDisplayName` may load
    /// shell extensions, and the initialisation is balanced so a thread that
    /// already had a different apartment is left untouched.
    unsafe fn recycle_icon() -> Option<Bmp> {
        const COINIT_APARTMENTTHREADED: u32 = 0x2;
        let hr = CoInitializeEx(std::ptr::null_mut(), COINIT_APARTMENTTHREADED);
        // S_OK (freshly initialised) / S_FALSE (already STA on this thread) →
        // we own the matching `CoUninitialize`.
        let uninit = hr == 0 || hr == 1;

        let w = wide("::{645FF040-5081-101B-9F08-00AA002F954E}");
        let mut pidl: *mut core::ffi::c_void = std::ptr::null_mut();
        let parsed = SHParseDisplayName(
            w.as_ptr(),
            std::ptr::null_mut(),
            &mut pidl,
            0,
            std::ptr::null_mut(),
        );
        let bmp = if parsed >= 0 && !pidl.is_null() {
            let mut sfi: SHFILEINFOW = std::mem::zeroed();
            let ok = SHGetFileInfoW(
                pidl as *const u16,
                0,
                &mut sfi,
                size_of::<SHFILEINFOW>() as u32,
                SHGFI_PIDL | SHGFI_ICON | SHGFI_LARGEICON,
            );
            let b = if ok != 0 && sfi.h_icon != 0 {
                pixels(sfi.h_icon)
            } else {
                None
            };
            if sfi.h_icon != 0 {
                DestroyIcon(sfi.h_icon);
            }
            b
        } else {
            None
        };
        if !pidl.is_null() {
            CoTaskMemFree(pidl);
        }
        if uninit {
            CoUninitialize();
        }
        bmp
    }

    /// HICON → straight-alpha RGBA. The colour plane carries the alpha; the 1bpp
    /// AND mask decides transparency for the pixels that carry none (icons
    /// authored without an alpha channel).
    unsafe fn pixels(hicon: isize) -> Option<Bmp> {
        let mut ii: ICONINFO = std::mem::zeroed();
        if GetIconInfo(hicon, &mut ii) == 0 {
            return None;
        }
        if ii.hbm_color == 0 {
            if ii.hbm_mask != 0 {
                DeleteObject(ii.hbm_mask);
            }
            return None;
        }
        let mut bm: BITMAP = std::mem::zeroed();
        if GetObjectW(ii.hbm_color, size_of::<BITMAP>() as i32, &mut bm as *mut _ as *mut u8)
            != size_of::<BITMAP>() as i32
        {
            DeleteObject(ii.hbm_mask);
            DeleteObject(ii.hbm_color);
            return None;
        }
        let w = bm.bm_width;
        let h = bm.bm_height.abs();
        if w <= 0 || h <= 0 || w > 256 || h > 256 || bm.bm_bits_pixel < 32 {
            DeleteObject(ii.hbm_mask);
            DeleteObject(ii.hbm_color);
            return None;
        }
        let (w, h) = (w as u32, h as u32);
        let hdc = GetDC(0);

        // Negative biHeight asks for top-down rows, so we never have to guess
        // which way up the DDB stores them.
        let mut bmi: BITMAPINFO = std::mem::zeroed();
        bmi.header.bi_size = size_of::<BITMAPINFOHEADER>() as u32;
        bmi.header.bi_width = w as i32;
        bmi.header.bi_height = -(h as i32);
        bmi.header.bi_planes = 1;
        bmi.header.bi_bit_count = 32;
        bmi.header.bi_compression = BI_RGB;
        let mut color = vec![0u32; (w * h) as usize];
        let got_color = GetDIBits(
            hdc,
            ii.hbm_color,
            0,
            h,
            color.as_mut_ptr() as *mut u8,
            &mut bmi,
            DIB_RGB_COLORS,
        );

        let stride = ((w + 31) / 32) * 4; // 1bpp rows are padded to 4 bytes
        let mut mask = vec![0u8; (stride * h) as usize];
        let mut mbmi: BITMAPINFO = std::mem::zeroed();
        mbmi.header.bi_size = size_of::<BITMAPINFOHEADER>() as u32;
        mbmi.header.bi_width = w as i32;
        mbmi.header.bi_height = -(h as i32);
        mbmi.header.bi_planes = 1;
        mbmi.header.bi_bit_count = 1;
        mbmi.header.bi_compression = BI_RGB;
        let got_mask = GetDIBits(
            hdc,
            ii.hbm_mask,
            0,
            h,
            mask.as_mut_ptr(),
            &mut mbmi,
            DIB_RGB_COLORS,
        );

        ReleaseDC(0, hdc);
        DeleteObject(ii.hbm_mask);
        DeleteObject(ii.hbm_color);

        if got_color != h as i32 {
            return None;
        }
        let has_mask = got_mask == h as i32;
        // A 32bpp colour plane already carries straight alpha. When *any*
        // pixel's alpha differs from 255 the icon was authored with a real
        // alpha channel — and per the icon format the AND mask is then all
        // zero, so consulting it would declare every transparent pixel
        // (whose RGB is black) opaque: that is exactly what painted a black
        // square behind some icons. Only when the alpha plane is uniformly
        // 255 (no alpha info) does the mask decide transparency, which is the
        // case for icons authored without an alpha channel.
        let has_alpha = color.iter().any(|v| (v >> 24) != 0xFF);

        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let v = color[(y * w + x) as usize];
                let b = (v & 0xFF) as u8;
                let g = ((v >> 8) & 0xFF) as u8;
                let r = ((v >> 16) & 0xFF) as u8;
                let a = if has_alpha {
                    (v >> 24) as u8
                } else if !has_mask {
                    255
                } else {
                    let bit = (mask[(y * stride + x / 8) as usize] >> (7 - (x % 8))) & 1;
                    // In the AND mask a set bit means "leave the destination
                    // alone", i.e. the pixel is transparent.
                    if bit == 1 {
                        0
                    } else {
                        255
                    }
                };
                rgba.extend_from_slice(&[r, g, b, a]);
            }
        }
        Some(Bmp { w, h, rgba })
    }
}

#[cfg(windows)]
fn extract(kind: &Kind) -> Option<Bmp> {
    imp::extract(kind)
}

#[cfg(not(windows))]
fn extract(_kind: &Kind) -> Option<Bmp> {
    None
}

// ------------------------------------------------------- dark window frame

/// Turn the OS window frame (title bar + border) dark, matching egui's dark
/// theme inside the client area.
///
/// Windows has no public "set my theme" API for a single window; the supported
/// way is the `DWMWA_USE_IMMERSIVE_DARK_MODE` window attribute, which is what
/// every modern Windows app with a dark title bar uses. On older builds the
/// (pre-20H1) constant is tried as a fallback.
#[cfg(windows)]
pub fn set_dark_frame(hwnd: isize, dark: bool) {
    #[link(name = "dwmapi")]
    extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: isize,
            dw_attribute: u32,
            pv_attribute: *const i32,
            cb_attribute: u32,
        ) -> i32;
    }
    const USE_IMMERSIVE_DARK_MODE: u32 = 20;
    const USE_IMMERSIVE_DARK_MODE_LEGACY: u32 = 19;
    let value = if dark { 1i32 } else { 0i32 };
    unsafe {
        if DwmSetWindowAttribute(hwnd, USE_IMMERSIVE_DARK_MODE, &value, 4) != 0 {
            DwmSetWindowAttribute(hwnd, USE_IMMERSIVE_DARK_MODE_LEGACY, &value, 4);
        }
    }
}

#[cfg(not(windows))]
pub fn set_dark_frame(_hwnd: isize, _dark: bool) {}
