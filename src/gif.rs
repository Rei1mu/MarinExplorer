//! Animated-GIF playback for the built-in viewer.
//!
//! `image` hands out one frame per `next()` and stops at the end, so looping
//! is ours to decide. The whole clip is decoded once into memory (downscaled,
//! and capped by a byte budget); the viewer then just walks the frame list, so
//! wrapping back to the first frame costs nothing and a loop has no gap.

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

use image::AnimationDecoder;

/// One decoded frame plus how long it stays on screen.
pub struct GifFrame {
    pub rgba: Vec<u8>,
    pub delay_ms: u32,
}

/// A fully decoded clip: every frame shares `w` × `h`.
pub struct GifClip {
    pub w: u32,
    pub h: u32,
    pub frames: Vec<GifFrame>,
}

/// Total RGBA we are willing to hold for a single clip.
const MAX_BYTES: usize = 128 * 1024 * 1024;
/// …plus a frame cap, so a pathological GIF cannot stall the worker forever.
const MAX_FRAMES: usize = 600;
/// Frames are downscaled to this long edge before they are kept.
pub const MAX_DIM: u32 = 1024;
/// GIF delays are unreliable at the ends: 0 means "no delay specified" and a
/// few encoders write absurdly long ones. Clamp like a browser would.
const MIN_DELAY_MS: u32 = 20;
const MAX_DELAY_MS: u32 = 1000;

/// Decode every frame of `path`. Returns `None` when the file is not a
/// readable GIF. Runs on a worker thread: a 13 MB GIF takes a moment.
pub fn decode(path: &Path) -> Option<GifClip> {
    let f = File::open(path).ok()?;
    let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(f)).ok()?;
    let mut frames = decoder.into_frames();
    let mut out: Vec<GifFrame> = Vec::new();
    let (mut w, mut h) = (0u32, 0u32);
    let mut bytes = 0usize;
    while out.len() < MAX_FRAMES && bytes < MAX_BYTES {
        let frame = match frames.next() {
            Some(Ok(f)) => f,
            // A broken tail still gives us the frames decoded so far.
            Some(Err(_)) | None => break,
        };
        let (num, den) = frame.delay().numer_denom_ms();
        let raw = if den == 0 { 100 } else { (num / den).max(1) };
        let delay_ms = raw.clamp(MIN_DELAY_MS, MAX_DELAY_MS);
        let mut buf = frame.into_buffer();
        let long = buf.width().max(buf.height());
        if long > MAX_DIM {
            let scale = MAX_DIM as f32 / long as f32;
            let nw = ((buf.width() as f32) * scale).round().max(1.0) as u32;
            let nh = ((buf.height() as f32) * scale).round().max(1.0) as u32;
            buf = image::imageops::resize(&buf, nw, nh, image::imageops::FilterType::Triangle);
        }
        w = buf.width();
        h = buf.height();
        bytes += 4 * w as usize * h as usize;
        out.push(GifFrame {
            rgba: buf.into_raw(),
            delay_ms,
        });
    }
    if out.is_empty() || w == 0 || h == 0 {
        return None;
    }
    Some(GifClip { w, h, frames: out })
}
