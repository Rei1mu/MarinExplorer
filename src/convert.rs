//! Image → ICO / PNG conversion, opened from the viewer's right-click menu.
//!
//! The dialog keeps the **original** decoded image in memory (the viewer's
//! own texture is downscaled to 1600 px and would lose detail) and lets the
//! user drag a square region out of it:
//!
//!   * **ICO** — always **one** `<stem>.ico` sitting next to the source image,
//!     holding every checked size as a PNG payload (the Vista+ form Windows
//!     Explorer itself writes). No extra folder, no `_16` suffix.
//!   * **PNG** — the whole image (or the crop) re-encoded at a chosen
//!     deflate level, 0 = no compression … 9 = smallest. Written into a new
//!     folder, because a `name.png` export would otherwise collide with — and
//!     overwrite — the source file of the same name.
//!
//! All encoding helpers are pure functions so they can be unit-tested
//! without a GUI context; the egui-dependent parts (texture upload, drag
//! handling) live in `main.rs`.

use std::path::{Path, PathBuf};

/// ICO sizes offered in the dialog, in display order.
pub const ICO_SIZES: [u32; 7] = [16, 24, 32, 48, 64, 128, 256];

/// Deflate level used for the PNG payloads embedded in an .ico file.
/// Icon payloads are tiny; balanced is the right trade-off.
const ICO_PNG_LEVEL: u8 = 6;

/// Long-edge cap when decoding the source for conversion. Keeps a crazy
/// 100 MP scan from eating 400 MB of RAM; normal photos are untouched.
const MAX_SOURCE_EDGE: u32 = 8192;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ConvertMode {
    Ico,
    Png,
}

/// Why an export failed. Kept free of UI text so `main.rs` can localise.
#[derive(Debug)]
pub enum ConvertError {
    /// The source image could not be decoded (dialog refuses to open).
    NoImage,
    /// ICO mode with no size checkbox ticked.
    NoSizes,
    Encode(String),
    Io(String),
}

/// Everything the convert dialog needs, including the decoded source.
pub struct ConvertState {
    pub mode: ConvertMode,
    pub path: PathBuf,
    /// Original pixels (RGBA8). `None` only when the dialog is closing.
    pub img: Option<image::RgbaImage>,
    /// Preview texture for the dialog.
    pub tex: Option<egui::TextureHandle>,
    /// Crop region in image pixels `[x, y, w, h]`; `None` = whole image.
    /// In ICO mode "whole image" means the largest centred square.
    pub crop: Option<[u32; 4]>,
    /// Image-pixel corner the current crop drag started from.
    pub drag_from: Option<[u32; 2]>,
    /// Which ICO sizes are ticked (indexes into [`ICO_SIZES`]).
    pub sizes: [bool; ICO_SIZES.len()],
    /// PNG deflate level, 0 = no compression … 9 = smallest.
    pub level: u8,
    /// Name of the new folder inside the source's directory (editable).
    pub folder: String,
    /// Last status line shown under the controls.
    pub status: String,
    /// Folder the last successful export wrote to (enables "open folder").
    pub out_dir: Option<PathBuf>,
    /// True when `status` reports a failure (painted in the error colour).
    pub status_err: bool,
}

impl ConvertState {
    /// Decode `path` and build the dialog state. Returns `None` when the
    /// file is not a decodable image.
    pub fn open(mode: ConvertMode, path: &Path, ctx: &egui::Context) -> Option<Self> {
        let img = decode_source(path)?;
        let (w, h) = (img.width(), img.height());
        let color = egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
        let tex = ctx.load_texture(
            format!("conv:{}", path.display()),
            color,
            egui::TextureOptions::LINEAR,
        );
        let stem = file_stem(path);
        Some(Self {
            mode,
            path: path.to_path_buf(),
            img: Some(img),
            tex: Some(tex),
            crop: None,
            drag_from: None,
            sizes: [true; ICO_SIZES.len()],
            level: 6,
            folder: match mode {
                ConvertMode::Ico => format!("{stem}_ico"),
                ConvertMode::Png => format!("{stem}_png"),
            },
            status: String::new(),
            out_dir: None,
            status_err: false,
        })
    }

    /// Region actually exported: the user crop, or (ICO) the largest
    /// centred square / (PNG) the whole image.
    fn base_region(&self, w: u32, h: u32) -> [u32; 4] {
        match (self.mode, self.crop) {
            (_, Some(r)) => r,
            (ConvertMode::Ico, None) => centered_square(w, h),
            (ConvertMode::Png, None) => [0, 0, w, h],
        }
    }

    /// Write the converted file(s). On success returns the output folder
    /// and the number of files written.
    pub fn export(&self) -> Result<(PathBuf, usize), ConvertError> {
        let img = self.img.as_ref().ok_or(ConvertError::NoImage)?;
        let (iw, ih) = (img.width(), img.height());
        let r = self.base_region(iw, ih);
        let base = crop_square(img, r);
        let stem = file_stem(&self.path);
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        // ICO lands right next to the source (a different extension, so there
        // is nothing to overwrite); PNG needs its own folder — see the module
        // doc — because `name.png` would clobber the source.
        let dir = match self.mode {
            ConvertMode::Ico => parent.to_path_buf(),
            ConvertMode::Png => parent.join(out_folder_name(&stem, &self.folder)),
        };
        std::fs::create_dir_all(&dir).map_err(|e| ConvertError::Io(e.to_string()))?;

        match self.mode {
            ConvertMode::Ico => {
                let mut entries: Vec<(u32, Vec<u8>)> = Vec::new();
                for (i, &sz) in ICO_SIZES.iter().enumerate() {
                    if !self.sizes[i] {
                        continue;
                    }
                    let small = image::imageops::resize(
                        &base,
                        sz,
                        sz,
                        image::imageops::FilterType::Lanczos3,
                    );
                    let png = encode_png(&small, ICO_PNG_LEVEL)
                        .map_err(|e| ConvertError::Encode(e.to_string()))?;
                    entries.push((sz, png));
                }
                if entries.is_empty() {
                    return Err(ConvertError::NoSizes);
                }
                // One file, every ticked size inside it: `pic.ico`.
                let all = ico_bytes(&entries);
                let f = dir.join(format!("{stem}.ico"));
                std::fs::write(&f, &all).map_err(|e| ConvertError::Io(e.to_string()))?;
                Ok((dir, 1))
            }
            ConvertMode::Png => {
                let data = encode_png(&base, self.level.clamp(0, 9))
                    .map_err(|e| ConvertError::Encode(e.to_string()))?;
                let f = dir.join(format!("{stem}.png"));
                std::fs::write(&f, &data).map_err(|e| ConvertError::Io(e.to_string()))?;
                Ok((dir, 1))
            }
        }
    }

    /// [`ConvertState::export`] plus a localised status line. Used by the
    /// dialog's 导出 button.
    pub fn run_export(&mut self, lang: crate::lang::Lang) {
        match self.export() {
            Ok((dir, n)) => {
                self.out_dir = Some(dir.clone());
                self.status_err = false;
                self.status = format!(
                    "{} {}{}{}",
                    crate::lang::tr("已导出", lang),
                    n,
                    crate::lang::tr("个文件到 ", lang),
                    dir.to_string_lossy(),
                );
            }
            Err(e) => {
                self.status_err = true;
                self.status = match e {
                    ConvertError::NoImage => crate::lang::tr("无法读取图片", lang).into(),
                    ConvertError::NoSizes => crate::lang::tr("未选择任何尺寸", lang).into(),
                    ConvertError::Encode(m) => {
                        format!("{}{}", crate::lang::tr("编码失败", lang), m)
                    }
                    ConvertError::Io(m) => format!("{}{}", crate::lang::tr("写入失败", lang), m),
                };
            }
        }
    }
}

/// Decode the source image, capping the long edge at [`MAX_SOURCE_EDGE`].
fn decode_source(path: &Path) -> Option<image::RgbaImage> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() == 0 || meta.len() > 96 * 1024 * 1024 {
        return None;
    }
    let img = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .decode()
        .ok()?;
    let img = if img.width() > MAX_SOURCE_EDGE || img.height() > MAX_SOURCE_EDGE {
        img.thumbnail(MAX_SOURCE_EDGE, MAX_SOURCE_EDGE)
    } else {
        img
    };
    Some(img.to_rgba8())
}

/// File name without the final extension (`a/b/c.png` → `c`).
pub fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "image".to_string())
}

/// The square region between two image-space points. The side is
/// `max(|dx|, |dy|)` so the selection stays square no matter the drag
/// direction, and the result is clamped inside the image.
pub fn square_between(a: [u32; 2], b: [u32; 2], w: u32, h: u32) -> [u32; 4] {
    let dx = (b[0] as i64 - a[0] as i64).abs();
    let dy = (b[1] as i64 - a[1] as i64).abs();
    let side = (dx.max(dy) as u32).clamp(1, w.min(h).max(1));
    let x = if b[0] >= a[0] {
        a[0]
    } else {
        a[0].saturating_sub(side)
    };
    let y = if b[1] >= a[1] {
        a[1]
    } else {
        a[1].saturating_sub(side)
    };
    let x = x.min(w.saturating_sub(side));
    let y = y.min(h.saturating_sub(side));
    [x, y, side, side]
}

/// Largest centred square of a `w × h` image — the ICO default source.
pub fn centered_square(w: u32, h: u32) -> [u32; 4] {
    let s = w.min(h).max(1);
    [(w - s) / 2, (h - s) / 2, s, s]
}

/// Copy the region out of `img` (the caller guarantees it is in bounds).
pub fn crop_square(img: &image::RgbaImage, r: [u32; 4]) -> image::RgbaImage {
    let (x, y, w, h) = (r[0], r[1], r[2], r[3]);
    image::ImageBuffer::from_fn(w, h, |px, py| *img.get_pixel(x + px, y + py))
}

/// Name of the new output folder (PNG export only). The typed name wins when
/// non-empty; otherwise `<stem>_png`. Directory separators and
/// Windows-reserved characters are folded to `_`.
pub fn out_folder_name(stem: &str, typed: &str) -> String {
    let fallback = format!("{stem}_png");
    let raw = typed.trim();
    let base = if raw.is_empty() { fallback } else { raw.to_string() };
    let cleaned: String = base
        .chars()
        .map(|c| {
            if matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches('.').to_string();
    if cleaned.is_empty() {
        format!("{stem}_out")
    } else {
        cleaned
    }
}

/// Encode RGBA8 pixels as PNG with a deflate level (0 = none, 1-9).
pub fn encode_png(img: &image::RgbaImage, level: u8) -> Result<Vec<u8>, String> {
    use png::{BitDepth, ColorType, DeflateCompression, Encoder};
    let mut buf: Vec<u8> = Vec::new();
    {
        let mut enc = Encoder::new(&mut buf, img.width(), img.height());
        enc.set_color(ColorType::Rgba);
        enc.set_depth(BitDepth::Eight);
        enc.set_deflate_compression(if level == 0 {
            DeflateCompression::NoCompression
        } else {
            DeflateCompression::Level(level.clamp(1, 9))
        });
        let mut writer = enc
            .write_header()
            .map_err(|e| format!("png header: {e}"))?;
        writer
            .write_image_data(img.as_raw())
            .map_err(|e| format!("png data: {e}"))?;
        writer.finish().map_err(|e| format!("png finish: {e}"))?;
    }
    Ok(buf)
}

/// Wrap PNG payloads in an ICO container (ICONDIR + entries + data).
/// Each entry is `(size, png_bytes)`; `size` 256 is stored as 0 in the
/// directory, per the spec.
pub fn ico_bytes(entries: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::with_capacity(6 + 16 * entries.len());
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // 1 = icon
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    let mut offset = (6 + 16 * entries.len()) as u32;
    for (size, data) in entries {
        let dim = if *size >= 256 { 0 } else { *size as u8 };
        out.push(dim); // width  (0 = 256)
        out.push(dim); // height (0 = 256)
        out.push(0); // colour count (0 = truecolour)
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += data.len() as u32;
    }
    for (_, data) in entries {
        out.extend_from_slice(data);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::GenericImageView;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("re_conv_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn square_between_stays_square_and_clamps() {
        // drag down-right: side = max(30, 10) = 30
        assert_eq!(square_between([10, 10], [40, 20], 100, 100), [10, 10, 30, 30]);
        // drag up-left: square extends backwards from the anchor
        assert_eq!(square_between([50, 50], [20, 40], 100, 100), [20, 20, 30, 30]);
        // side larger than the image is clamped
        assert_eq!(square_between([90, 90], [200, 200], 100, 100), [0, 0, 100, 100]);
        // a click (same point) yields a 1px square, never zero
        assert_eq!(square_between([5, 5], [5, 5], 100, 100), [5, 5, 1, 1]);
    }

    #[test]
    fn centered_square_uses_short_edge() {
        assert_eq!(centered_square(100, 60), [20, 0, 60, 60]);
        assert_eq!(centered_square(60, 100), [0, 20, 60, 60]);
        assert_eq!(centered_square(50, 50), [0, 0, 50, 50]);
    }

    #[test]
    fn crop_square_copies_exact_pixels() {
        // 4x2 image, pixel (x,y) = (x*10 + y, 0, 0)
        let img = image::ImageBuffer::from_fn(4, 2, |x, y| {
            image::Rgba([(x * 10 + y) as u8, 0, 0, 255])
        });
        let sub = crop_square(&img, [1, 0, 2, 2]);
        assert_eq!((sub.width(), sub.height()), (2, 2));
        assert_eq!(*sub.get_pixel(0, 0), image::Rgba([10, 0, 0, 255]));
        assert_eq!(*sub.get_pixel(1, 0), image::Rgba([20, 0, 0, 255]));
        assert_eq!(*sub.get_pixel(0, 1), image::Rgba([11, 0, 0, 255]));
    }

    #[test]
    fn ico_container_layout() {
        let a = vec![1u8; 20];
        let b = vec![2u8; 40];
        let ico = ico_bytes(&[(16, a.clone()), (256, b.clone())]);
        assert_eq!(&ico[0..2], &0u16.to_le_bytes()); // reserved
        assert_eq!(&ico[2..4], &1u16.to_le_bytes()); // type
        assert_eq!(&ico[4..6], &2u16.to_le_bytes()); // count
        // entry 1 at offset 6: w,h,colors,reserved,planes,bpp,size,offset
        assert_eq!(ico[6], 16);
        assert_eq!(ico[7], 16);
        assert_eq!(&ico[8..10], &[0u8, 0]); // colours / reserved
        assert_eq!(&ico[10..12], &1u16.to_le_bytes()); // planes
        assert_eq!(&ico[12..14], &32u16.to_le_bytes()); // bpp
        assert_eq!(&ico[14..18], &20u32.to_le_bytes()); // size
        assert_eq!(&ico[18..22], &38u32.to_le_bytes()); // offset 6+2*16
        // entry 2 at offset 22: 256px stored as 0
        assert_eq!(ico[22], 0);
        assert_eq!(ico[23], 0);
        assert_eq!(&ico[26..28], &1u16.to_le_bytes()); // planes
        assert_eq!(&ico[28..30], &32u16.to_le_bytes()); // bpp
        assert_eq!(&ico[30..34], &40u32.to_le_bytes()); // size
        assert_eq!(&ico[34..38], &58u32.to_le_bytes()); // 38 + 20
        assert_eq!(ico.len(), 6 + 32 + 20 + 40);
        assert_eq!(&ico[38..58], &a[..]);
        assert_eq!(&ico[58..98], &b[..]);
    }

    #[test]
    fn png_level_controls_size_and_roundtrips() {
        // smooth gradient: higher deflate level must not be bigger
        let img = image::ImageBuffer::from_fn(64, 64, |x, y| {
            image::Rgba([(x * 4) as u8, (y * 4) as u8, 128, 255])
        });
        let none = encode_png(&img, 0).unwrap();
        let mid = encode_png(&img, 6).unwrap();
        let best = encode_png(&img, 9).unwrap();
        assert!(!none.is_empty() && !best.is_empty());
        assert!(best.len() <= none.len(), "level 9 larger than level 0");
        assert!(mid.len() <= none.len());
        // roundtrip through the image crate
        for (lvl, bytes) in [(0u8, &none), (9, &best)] {
            let back = image::load_from_memory_with_format(bytes, image::ImageFormat::Png).unwrap();
            assert_eq!(back.dimensions(), (64, 64), "level {lvl} dims");
            assert_eq!(back.to_rgba8().as_raw(), img.as_raw(), "level {lvl} pixels");
        }
    }

    #[test]
    fn out_folder_name_sanitises() {
        assert_eq!(out_folder_name("pic", ""), "pic_png");
        assert_eq!(out_folder_name("pic", " a/b:c "), "a_b_c");
        assert_eq!(out_folder_name("pic", "..."), "pic_out");
        assert_eq!(out_folder_name("pic", "icons"), "icons");
    }

    /// Fresh ICO state for `src`/`img` (the tests need several independent
    /// ones — `ConvertState` is not `Clone` because of its texture handle).
    fn ico_state(src: &Path, img: &image::RgbaImage) -> ConvertState {
        ConvertState {
            mode: ConvertMode::Ico,
            path: src.to_path_buf(),
            img: Some(img.clone()),
            tex: None,
            crop: None,
            drag_from: None,
            sizes: [true; ICO_SIZES.len()],
            level: 6,
            folder: String::new(),
            status: String::new(),
            out_dir: None,
            status_err: false,
        }
    }

    #[test]
    fn export_ico_png_end_to_end() {
        let dir = temp_dir("e2e");
        // source: 40x20 red/blue image
        let src_img = image::ImageBuffer::from_fn(40, 20, |x, _y| {
            if x < 20 {
                image::Rgba([255, 0, 0, 255])
            } else {
                image::Rgba([0, 0, 255, 255])
            }
        });
        let src = dir.join("shot.png");
        std::fs::write(&src, encode_png(&src_img, 9).unwrap()).unwrap();

        // --- ICO: one file next to the source, every size embedded ---
        let st = ico_state(&src, &src_img);
        let (out, n) = st.export().expect("ico export");
        assert_eq!(n, 1);
        assert_eq!(out, dir);
        assert!(!dir.join("shot_ico").exists(), "must not create a folder");
        let ico = std::fs::read(dir.join("shot.ico")).unwrap();
        assert_eq!(&ico[4..6], &7u16.to_le_bytes()); // 7 entries
        assert!(!dir.join("shot_16.ico").exists(), "no per-size files");
        // decodes back (image crate's ICO reader takes the largest entry)
        let back = image::load_from_memory_with_format(&ico, image::ImageFormat::Ico).unwrap();
        assert_eq!(back.dimensions(), (256, 256));

        // --- ICO: only the ticked sizes go in, still one plain file ---
        let mut st2 = ico_state(&src, &src_img);
        st2.sizes = [false; ICO_SIZES.len()];
        st2.sizes[0] = true; // 16
        st2.sizes[6] = true; // 256
        let (out2, n2) = st2.export().expect("ico export 2");
        assert_eq!(n2, 1);
        assert_eq!(out2, dir);
        let ico2 = std::fs::read(dir.join("shot.ico")).unwrap();
        assert_eq!(&ico2[4..6], &2u16.to_le_bytes()); // 2 entries

        // --- PNG with a crop region ---
        let st3 = ConvertState {
            mode: ConvertMode::Png,
            path: src.clone(),
            img: Some(src_img.clone()),
            tex: None,
            crop: Some([0, 0, 20, 20]),
            drag_from: None,
            sizes: [true; ICO_SIZES.len()],
            level: 9,
            folder: "small".into(),
            status: String::new(),
            out_dir: None,
            status_err: false,
        };
        let (out3, n3) = st3.export().expect("png export");
        assert_eq!((out3.as_path(), n3), (dir.join("small").as_path(), 1));
        let png = std::fs::read(out3.join("shot.png")).unwrap();
        let back = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!(back.dimensions(), (20, 20));
        // left half is red
        assert_eq!(back.to_rgba8().get_pixel(5, 10), &image::Rgba([255, 0, 0, 255]));

        // --- no sizes ticked is an error, not an empty folder ---
        let mut st4 = st3;
        st4.mode = ConvertMode::Ico;
        st4.crop = None;
        st4.sizes = [false; ICO_SIZES.len()];
        assert!(matches!(st4.export(), Err(ConvertError::NoSizes)));
    }
}
