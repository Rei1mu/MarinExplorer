//! Video preview / playback via Windows Media Foundation.
//!
//! Media Foundation ships with Windows, so no extra binary is bundled and there
//! is no licensing obligation (it is an OS component). Frames are decoded on a
//! background thread to RGBA and uploaded as `egui::TextureHandle`s — exactly
//! the same path the image pipeline uses — so there is no GL/D3D interop to
//! get wrong. Playback is silent in v1 (no audio route); the video rectangle
//! is the viewer's own area, drawn with the rest of the UI.
//!
//! Format coverage is whatever Media Foundation supports natively: MP4 / MOV /
//! M4V / 3GP / AVI / WMV and H.264 / HEVC / VP9. MKV / WebM / FLV are *not*
//! supported by stock MF (they need a third-party decoder such as LAV, which
//! we deliberately do not bundle) — for those the viewer shows a clear notice.
//!
//! Hard-won API facts (verified against windows-0.52 + the real decoder):
//! * The reader MUST be created with `MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING`
//!   or `SetCurrentMediaType(RGB32)` fails with `MF_E_INVALIDMEDIATYPE`
//!   (0xC00D36B4) — that flag is what makes H.264→RGB conversion possible.
//! * DXVA (hardware decode) is tried first — software-decoding a 1-2 GB clip
//!   costs a full CPU core. If the hardware reader cannot deliver its first
//!   frame, `open_video` rebuilds it with `MF_SOURCE_READER_DISABLE_DXVA`.
//! * A `ReadSample` *error* is not end-of-stream: hardware readers can fail
//!   transiently mid-clip. Treating an error as EOS stops playback after a few
//!   seconds on long files. `ReadOutcome` keeps the two apart.
//! * Frame size comes from `GetNativeMediaType` (always populated). The RGB32
//!   *output* type carries no frame size before the first sample is decoded.
//! * After `SetCurrentMediaType` the first `ReadSample` only reports
//!   `CURRENTMEDIATYPECHANGED` with no sample — always loop until a real
//!   sample arrives (`read_frame_outcome`).
//! * MF "RGB32" frames are BGRA and usually carry alpha == 0; alpha is forced
//!   to 255 here or every pixel would render fully transparent.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver};
use std::sync::{Arc, Once};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Media::MediaFoundation::{
    IMFAttributes, MFCreateAttributes, MFCreateMediaType, MFCreateSourceReaderFromURL, MFStartup,
    MF_PD_DURATION, MF_SOURCE_READER_DISABLE_DXVA, MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING,
    MF_SOURCE_READER_MEDIASOURCE, MF_SOURCE_READERF_ENDOFSTREAM,
    MF_SOURCE_READER_FIRST_VIDEO_STREAM, MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_SIZE, MF_MT_MAJOR_TYPE,
    MF_MT_SUBTYPE, MFMediaType_Video, MFVideoFormat_RGB32, IMFMediaType, IMFSourceReader,
    IMFSample,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantToUInt64, PROPVARIANT};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::VARENUM;
use windows::core::{GUID, PCWSTR};

/// MF startup version (2.70). `MFStartup` takes `(major << 16) | minor`.
const MF_VERSION: u32 = 0x0002_0070;

/// `VT_I8` — the PROPVARIANT type MF expects for a 100-ns seek position.
const VT_I8: VARENUM = VARENUM(20);

static MF_INIT: Once = Once::new();
static MF_OK: AtomicBool = AtomicBool::new(false);

/// Initialise COM (multithreaded) on the calling thread and Media Foundation
/// once per process. COM must be initialised *per thread*, so `CoInitializeEx`
/// runs every call; `MFStartup` is guarded by `Once`. Returns `false` if MF
/// could not start, in which case callers fall back to "cannot preview".
fn ensure_mf() -> bool {
    unsafe {
        // Already-initialised (S_FALSE) or wrong-apartment (RPC_E_CHANGED_MODE)
        // both mean "leave COM as-is" — we only care that MF can run.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    if MF_OK.load(Ordering::Relaxed) {
        return true;
    }
    MF_INIT.call_once(|| unsafe {
        if MFStartup(MF_VERSION, 0).is_ok() {
            MF_OK.store(true, Ordering::Relaxed);
        }
    });
    MF_OK.load(Ordering::Relaxed)
}

/// Messages from the playback worker to the UI.
pub enum PbMsg {
    /// Clip duration in 100-ns units (sent once, right after open).
    Info { dur_ns: u64 },
    /// A decoded frame; `ts` is its presentation time (100-ns units).
    Frame { ts: i64, w: u32, h: u32, rgba: Vec<u8> },
    /// End of stream reached.
    Eos,
}

/// Owns a playback worker thread. Dropping it signals the thread to stop.
pub struct Playback {
    pub rx: Receiver<PbMsg>,
    /// Send a seek target (100-ns units) to the worker.
    pub seek_tx: std::sync::mpsc::Sender<u64>,
    /// Pause flag: the worker idles (no samples read) while paused, so a
    /// pause/resume continues at the current position instead of restarting.
    pub paused: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl Playback {
    pub fn set_paused(&self, p: bool) {
        self.paused.store(p, Ordering::Relaxed);
    }
    /// Ask the worker to jump to `ns` (100-ns units). Best effort.
    pub fn seek(&self, ns: u64) {
        let _ = self.seek_tx.send(ns);
    }
}

impl Drop for Playback {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Close the receiving end first. The worker sends into a bounded
        // channel: if it is parked in `send` on a full queue, `join` below
        // would block the UI thread until a frame was consumed — which is
        // exactly the "not responding" freeze on closing the viewer.
        let (_dead_tx, dead_rx) = sync_channel::<PbMsg>(1);
        self.rx = dead_rx; // drops the real receiver, unblocking the worker
        // Detach rather than `join`: a worker parked in `ReadSample` (or
        // rebuilding a software reader) can take seconds to notice `stop`, and
        // blocking on it would freeze the window. It exits on its own.
        drop(self.handle.take());
    }
}

/// Media Foundation timestamps are in 100-ns units; `Instant`/`Duration` are
/// in nanoseconds. Treating the raw value as nanoseconds plays the clip 100x
/// too fast (and ends it after a fraction of a second).
fn ts_dur(ts: i64) -> Duration {
    Duration::from_nanos((ts.max(0) as u64).saturating_mul(100))
}

/// Decode the first video frame to RGBA (`w, h, rgba`), synchronously. Used by
/// the async helpers below; runs on a throwaway MTA thread.
///
/// Returns `None` when MF is unavailable or the file is not a decodable video.
fn decode_first_frame(path: &Path) -> Option<(u32, u32, Vec<u8>)> {
    if !ensure_mf() {
        return None;
    }
    let url = wide(path)?;
    let mut p = open_video(&url)?;
    // `open_video` already consumed the first frame on the hardware path;
    // the software fallback still needs one.
    let (_, sample) = match p.first {
        Some(f) => f,
        None => match read_frame_outcome(&p.reader) {
            ReadOutcome::Frame(ts, s) => (ts, s),
            ReadOutcome::Eos | ReadOutcome::Failed => return None,
        },
    };
    // The hardware path already confirmed the pitch; the software fallback
    // must do it here, on the frame it just read.
    if !p.stride_checked {
        p.stride = detect_stride(&sample, p.w, p.h, p.stride);
        p.stride_checked = true;
    }
    sample_to_rgba(&sample, p.w, p.h, p.stride, 0)
}

/// Spawn a background job that decodes the first frame of `path` and downscales
/// it so no side exceeds `max_dim`. The channel yields the result exactly once
/// (`None` = undecodable). Callers poll from the UI thread and upload the RGBA
/// data as an egui texture — no blocking on the UI thread.
pub fn poster_rgba_scaled_async(path: &Path, max_dim: u32) -> Receiver<Option<(u32, u32, Vec<u8>)>> {
    let (tx, rx) = std::sync::mpsc::channel();
    let path = path.to_path_buf();
    thread::spawn(move || {
        let out = decode_first_frame(&path)
            .map(|(w, h, rgba)| downscale_rgba(&rgba, w, h, max_dim));
        let _ = tx.send(out);
    });
    rx
}

/// Box-filter downscale of an RGBA image so its longest side is `max_dim`.
/// Returns the (possibly unchanged) image. Used to keep poster textures small:
/// a 4K first frame would otherwise cost ~33 MB of RAM/VRAM.
pub fn downscale_rgba(rgba: &[u8], w: u32, h: u32, max_dim: u32) -> (u32, u32, Vec<u8>) {
    let (w, h) = (w as usize, h as usize);
    let long = w.max(h);
    if long <= max_dim as usize {
        return (w as u32, h as u32, rgba.to_vec());
    }
    let nw = (w * max_dim as usize / long).max(1);
    let nh = (h * max_dim as usize / long).max(1);
    let mut out = vec![0u8; nw * nh * 4];
    // Area-average over the source box each destination pixel covers.
    for y in 0..nh {
        let sy0 = y * h / nh;
        let sy1 = ((y + 1) * h / nh).max(sy0 + 1).min(h);
        for x in 0..nw {
            let sx0 = x * w / nw;
            let sx1 = ((x + 1) * w / nw).max(sx0 + 1).min(w);
            let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
            let mut n = 0u32;
            for sy in sy0..sy1 {
                let row = (sy * w) * 4;
                for sx in sx0..sx1 {
                    let i = row + sx * 4;
                    r += rgba[i] as u32;
                    g += rgba[i + 1] as u32;
                    b += rgba[i + 2] as u32;
                    n += 1;
                }
            }
            let di = (y * nw + x) * 4;
            out[di] = (r / n) as u8;
            out[di + 1] = (g / n) as u8;
            out[di + 2] = (b / n) as u8;
            out[di + 3] = 255;
        }
    }
    (nw as u32, nh as u32, out)
}

/// Start decoding `path` from the beginning, pacing frames to their timestamps.
/// The returned channel yields [`PbMsg`]s; dropping `Playback` ends the worker.
///
/// The worker is built to survive long clips: a transient `ReadSample` failure
/// is retried instead of being mistaken for end-of-stream, and a hardware
/// reader that dies mid-clip is rebuilt in software mode and resumed from the
/// last shown position.
/// `max_dim` caps the longest side of every emitted frame (0 = native size).
/// Keep it at the on-screen size: uploading 1080p RGBA at 30 fps is pure
/// waste and is the main reason playback used to load a whole core.
pub fn start_playback(path: &Path, max_dim: u32) -> Option<Playback> {
    if !ensure_mf() {
        return None;
    }
    let url = wide(path)?;
    // Bounded: if the UI thread stalls, the worker blocks instead of decoding
    // the whole clip into an unbounded queue.
    let (tx, rx) = sync_channel::<PbMsg>(8);
    let (seek_tx, seek_rx) = std::sync::mpsc::channel::<u64>();
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let paused = Arc::new(AtomicBool::new(false));
    let paused2 = paused.clone();
    let handle = thread::spawn(move || unsafe {
        let mut p = match open_video(&url) {
            Some(p) => p,
            None => return,
        };
        // Duration (100-ns units) straight from the source's presentation
        // attributes; unknown for some containers, which is fine.
        let dur = p
            .reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v| PropVariantToUInt64(&v).ok());
        if let Some(d) = dur {
            let _ = tx.send(PbMsg::Info { dur_ns: d });
        }

        // Pacing model: `t0` is the Instant that corresponds to ts == 0. It is
        // (re)based when the first frame after a start/seek/pause arrives, so
        // that frame shows immediately and the rest follow on their own clock.
        let mut t0: Option<Instant> = None;
        let mut last_ts: i64 = 0;
        let mut was_paused = false;
        // Pending seek target (100-ns). Frames before it are shown but never
        // slept for — the key frame MF lands on is usually slightly earlier.
        let mut pending_seek: i64 = -1;
        // Consecutive read failures. A hardware reader can hiccup mid-clip;
        // only a long unrecoverable streak ends playback.
        let mut err_streak: u32 = 0;
        // One software rebuild per playback, after a hardware reader dies.
        let mut rebuilt = false;

        // `open_video` consumed the first frame while validating the hardware
        // path — emit it so playback starts from frame 0, not frame 1.
        if let Some((ts, s)) = p.first.take() {
            t0 = Some(Instant::now() - ts_dur(ts));
            last_ts = ts;
            if let Some((fw, fh, rgba)) = sample_to_rgba(&s, p.w, p.h, p.stride, max_dim) {
                if tx
                    .send(PbMsg::Frame {
                        ts,
                        w: fw,
                        h: fh,
                        rgba,
                    })
                    .is_err()
                {
                    return; // UI hung up
                }
            }
        }

        loop {
            if stop2.load(Ordering::Relaxed) {
                break;
            }
            if paused2.load(Ordering::Relaxed) {
                was_paused = true;
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            if was_paused {
                // Resume: re-base the clock so the last shown frame is "now".
                was_paused = false;
                t0 = Some(Instant::now() - ts_dur(last_ts));
            }
            // Drain seek requests (keep the newest).
            while let Ok(t) = seek_rx.try_recv() {
                pending_seek = t as i64;
                t0 = None;
                // Reposition the source; the next `ReadSample` lands on the
                // key frame at or before the target.
                seek_reader(&p.reader, t);
            }
            let (ts, s) = match read_frame_outcome(&p.reader) {
                ReadOutcome::Frame(ts, s) => {
                    err_streak = 0;
                    (ts, s)
                }
                ReadOutcome::Eos => {
                    // Genuine end of stream.
                    let _ = tx.send(PbMsg::Eos);
                    break;
                }
                ReadOutcome::Failed => {
                    err_streak += 1;
                    // A hardware reader can die part-way through a long clip
                    // (GPU contention, 1-2 GB files). Rebuild it in software
                    // mode and resume from the last shown frame.
                    if !p.soft && !rebuilt && err_streak >= 40 {
                        if let Some(p2) = open_video_mode(&url, true) {
                            seek_reader(&p2.reader, last_ts.max(0) as u64);
                            p = p2;
                            rebuilt = true;
                            err_streak = 0;
                            t0 = None;
                            continue;
                        }
                    }
                    if err_streak >= 200 {
                        // ~10 s of continuous failure: give up.
                        let _ = tx.send(PbMsg::Eos);
                        break;
                    }
                    thread::sleep(Duration::from_millis(50));
                    continue;
                }
            };
            // Software-fallback readers carry no validated frame; confirm the
            // pitch on the first real one (the hardware path did it already).
            if !p.stride_checked {
                p.stride = detect_stride(&s, p.w, p.h, p.stride);
                p.stride_checked = true;
            }
            if t0.is_none() {
                // First frame after a start/seek: anchor at the greater of the
                // frame's own ts and the seek target so a pre-target key frame
                // shows instantly instead of being slept for.
                let base = ts.max(pending_seek);
                pending_seek = -1;
                t0 = Some(Instant::now() - ts_dur(base));
            }
            let target = ts_dur(ts);
            let elapsed = t0.unwrap().elapsed();
            if target > elapsed {
                let wait = target - elapsed;
                // Windows timers have ~15.6 ms granularity; sleeping the whole
                // wait overshoots and the decode of the next frame fills the
                // rest. Leave the last few milliseconds un-slept.
                if wait > Duration::from_millis(3) {
                    thread::sleep(wait - Duration::from_millis(3));
                }
            } else if elapsed > target + Duration::from_millis(400) {
                // Behind schedule. Old behaviour just dropped the frame and
                // left the clock in the past, so every later frame fired
                // immediately: the clip visibly fast-forwarded and hit the end
                // after a few seconds. Re-anchor instead, so playback
                // continues at real speed from here (slower, but smooth).
                t0 = Some(Instant::now() - target);
                continue;
            }
            last_ts = ts;
            if let Some((fw, fh, rgba)) = sample_to_rgba(&s, p.w, p.h, p.stride, max_dim) {
                if tx
                    .send(PbMsg::Frame {
                        ts,
                        w: fw,
                        h: fh,
                        rgba,
                    })
                    .is_err()
                {
                    break; // UI hung up
                }
            }
        }
    });
    Some(Playback {
        rx,
        seek_tx,
        paused,
        stop,
        handle: Some(handle),
    })
}

/// Build the `PCWSTR` bytes for `path` (UTF-16, NUL-terminated). The returned
/// vec must outlive the `PCWSTR`, so callers keep it in scope.
fn wide(path: &Path) -> Option<Vec<u16>> {
    let s = path.to_string_lossy();
    if s.is_empty() {
        return None;
    }
    let mut v: Vec<u16> = s.encode_utf16().collect();
    v.push(0);
    Some(v)
}

/// An `IMFMediaType` asking the reader to emit 32-bit RGB (BGRA in memory).
fn rgb32_type() -> Option<IMFMediaType> {
    unsafe {
        let mt = MFCreateMediaType().ok()?;
        mt.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok()?;
        mt.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32).ok()?;
        Some(mt)
    }
}

/// A reader prepared for RGB32 output, plus its already-read first frame.
struct Prepared {
    reader: IMFSourceReader,
    w: u32,
    h: u32,
    stride: i32,
    /// `true` once `stride` was confirmed against a real frame (see
    /// `detect_stride`). Metadata alone cannot be trusted for the RGB32
    /// video-processor output.
    stride_checked: bool,
    /// `true` when built with DXVA disabled (software decode).
    soft: bool,
    /// First frame consumed while validating the pipeline, if any. Callers
    /// must emit it before continuing the loop so no frame is lost.
    first: Option<(i64, IMFSample)>,
}

/// Open `url` and prepare RGB32 output. Hardware decode (DXVA) is tried first
/// — software-decoding a 1-2 GB clip costs a full CPU core. The hardware
/// reader is validated by reading one frame; if that fails (DXVA init is lazy
/// and can fail on the first `ReadSample` in elevated / GPU-busy processes),
/// the reader is rebuilt with `MF_SOURCE_READER_DISABLE_DXVA`.
fn open_video(url: &[u16]) -> Option<Prepared> {
    if let Some(mut p) = open_video_mode(url, false) {
        match read_frame_outcome(&p.reader) {
            ReadOutcome::Frame(ts, s) => {
                // Confirm the row pitch against a real frame before anything
                // consumes it (see `detect_stride` for why metadata lies).
                p.stride = detect_stride(&s, p.w, p.h, p.stride);
                p.stride_checked = true;
                return Some(Prepared {
                    first: Some((ts, s)),
                    ..p
                });
            }
            // No frame yet (or a dead hardware path): fall through to software.
            _ => {}
        }
    }
    open_video_mode(url, true)
}

/// Build a source reader for `url` that is allowed to convert frames to RGB32.
///
/// `MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING` **must** be set or the reader
/// refuses to insert its colour-conversion stage: `SetCurrentMediaType(RGB32)`
/// then fails with `MF_E_INVALIDMEDIATYPE` (0xC00D36B4) and *no* video can be
/// decoded. This flag is the difference between "every video fails" and working
/// preview/playback, so it is set here once for both the poster and playback.
/// With `disable_dxva` the reader is additionally forced onto the software
/// decode path (fallback for when the hardware one cannot deliver frames).
fn open_video_mode(url: &[u16], disable_dxva: bool) -> Option<Prepared> {
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 4).ok()?;
        let attrs = attrs?;
        attrs
            .SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)
            .ok()?;
        if disable_dxva {
            attrs.SetUINT32(&MF_SOURCE_READER_DISABLE_DXVA, 1).ok()?;
        }
        let reader = MFCreateSourceReaderFromURL(PCWSTR(url.as_ptr()), Some(&attrs)).ok()?;
        let (w, h) = native_size(&reader)?;
        let mt = rgb32_type()?;
        reader
            .SetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, None, &mt)
            .ok()?;
        let stride = output_stride(&reader).unwrap_or((w as i32) * 4);
        Some(Prepared {
            reader,
            w,
            h,
            stride,
            stride_checked: false,
            soft: disable_dxva,
            first: None,
        })
    }
}

/// Read the video stream's native dimensions from the *native* media type.
/// Unlike the RGB32 output type, this is always populated, so the decode never
/// fails for lack of a frame size.
fn native_size(reader: &IMFSourceReader) -> Option<(u32, u32)> {
    unsafe {
        let native = reader
            .GetNativeMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32, 0)
            .ok()?;
        let val = native.GetUINT64(&MF_MT_FRAME_SIZE).ok()?;
        let w = (val >> 32) as u32;
        let h = val as u32;
        if w == 0 || h == 0 {
            None
        } else {
            Some((w, h))
        }
    }
}

/// Row stride of the RGB32 output, once a sample has been decoded. Falls back to
/// a tight `w * 4` when the converter does not report one.
fn output_stride(reader: &IMFSourceReader) -> Option<i32> {
    unsafe {
        let out = reader
            .GetCurrentMediaType(MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32)
            .ok()?;
        let s = out.GetUINT64(&MF_MT_DEFAULT_STRIDE).ok()?;
        Some(s as i32)
    }
}

/// Compute the RGB32 row pitch deterministically.
///
/// `MF_MT_DEFAULT_STRIDE` is never populated on the video-processor output
/// type, and heuristics on the buffer capacity are unreliable. The dependable
/// identity: decoders pad the coded frame to macroblock boundaries, so the
/// true pitch is `align(w, A) * 4` and the buffer holds exactly
/// `pitch * align(h, B)` bytes for small alignments A/B (16 covers H.264 and
/// HEVC; 32/64 cover AV1 / exotic codecs). Verified exact on every probe:
/// 1080x1790 -> 4352 * 1792, 1920x1080 -> 7680 * 1088. Falls back to the
/// capacity-derived pitch, then the tight layout, only if no identity holds.
fn detect_stride(sample: &IMFSample, w: u32, h: u32, fallback: i32) -> i32 {
    unsafe {
        let Ok(buf) = sample.GetBufferByIndex(0) else {
            return fallback;
        };
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut cur_len: u32 = 0;
        if buf
            .Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
            .is_err()
            || ptr.is_null()
        {
            return fallback;
        }
        let _ = buf.Unlock();
        let len = cur_len as usize;
        let align = |v: u32, a: u32| v.div_ceil(a) * a;
        let w4 = w as usize * 4;
        // 1. Exact identity: pitch * padded height == buffer capacity.
        for aw in [16u32, 32, 64] {
            for ah in [16u32, 32, 64] {
                let s = align(w, aw) as usize * 4;
                if s >= w4 && s * align(h, ah) as usize == len {
                    return s as i32;
                }
            }
        }
        // 2. Capacity-derived pitch against the 16-padded height.
        let hp = align(h, 16) as usize;
        if hp > 0 {
            let s = len / hp;
            if s >= w4 && s % 4 == 0 && s * hp <= len && len - s * hp < s {
                return s as i32;
            }
        }
        fallback
    }
}

/// Read one sample from the first video stream. `flags` carries
/// `MF_SOURCE_READERF_ENDOFSTREAM` when the stream is exhausted. Returns
/// `Err(())` on a real `ReadSample` failure — **not** on end-of-stream, which
/// is reported through the flags.
fn read_sample(reader: &IMFSourceReader) -> Result<(u32, u32, i64, Option<IMFSample>), ()> {
    unsafe {
        let mut stream: u32 = 0;
        let mut flags: u32 = 0;
        let mut ts: i64 = 0;
        let mut sample: Option<IMFSample> = None;
        reader
            .ReadSample(
                MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32,
                0,
                Some(&mut stream),
                Some(&mut flags),
                Some(&mut ts),
                Some(&mut sample),
            )
            .map_err(|_| ())?;
        Ok((stream, flags, ts, sample))
    }
}

/// Outcome of reading towards the next decoded frame.
pub enum ReadOutcome {
    /// A decoded frame with its presentation timestamp (100-ns units).
    Frame(i64, IMFSample),
    /// The stream is exhausted — genuine end of playback.
    Eos,
    /// `ReadSample` failed (hardware reader hiccup, corrupt span, …). Not EOS.
    Failed,
}

/// Read samples until a *decoded* frame is available, skipping the notifications
/// Media Foundation emits after an output-type change:
/// `MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED` and other flag-only reads that
/// carry no sample. A bounded iteration guard prevents a stall if the reader
/// never produces a frame.
fn read_frame_outcome(reader: &IMFSourceReader) -> ReadOutcome {
    for _ in 0..512 {
        let (_, flags, ts, sample) = match read_sample(reader) {
            Ok(v) => v,
            Err(_) => return ReadOutcome::Failed,
        };
        if (flags & (MF_SOURCE_READERF_ENDOFSTREAM.0 as u32)) != 0 {
            return ReadOutcome::Eos;
        }
        if let Some(s) = sample {
            return ReadOutcome::Frame(ts, s);
        }
        // Type-changed / stream-tick / no sample yet — keep reading.
    }
    ReadOutcome::Failed
}

/// Lock an `IMFSample`'s buffer and convert its BGRA pixels to RGBA. MF's
/// "RGB32" is little-endian BGRA, so the R/B channels are swapped.
fn sample_to_rgba(
    sample: &IMFSample,
    w: u32,
    h: u32,
    stride: i32,
    max_dim: u32,
) -> Option<(u32, u32, Vec<u8>)> {
    unsafe {
        let buf = sample.GetBufferByIndex(0).ok()?;
        let mut ptr: *mut u8 = std::ptr::null_mut();
        let mut max_len: u32 = 0;
        let mut cur_len: u32 = 0;
        buf.Lock(&mut ptr, Some(&mut max_len), Some(&mut cur_len))
            .ok()?;
        if ptr.is_null() {
            return None;
        }
        // The pitch is confirmed once per reader (`detect_stride`), but a
        // frame can still arrive in a differently sized buffer. Fall back to
        // the tight layout (or the capacity-derived one) if the assumed pitch
        // does not fit, instead of walking off the end of the buffer.
        let tight = w as usize * 4;
        let mut row = if stride != 0 {
            stride.unsigned_abs() as usize
        } else {
            tight
        };
        if row < tight {
            row = tight;
        }
        if row.checked_mul(h as usize).map_or(true, |n| n > cur_len as usize) {
            let by_buf = if h > 0 {
                cur_len as usize / h as usize
            } else {
                tight
            };
            row = if by_buf >= tight && by_buf % 4 == 0 {
                by_buf
            } else {
                tight
            };
        }
        let mut rgba = vec![0u8; w as usize * h as usize * 4];
        // Video has no transparency, but MF's "RGB32" frames frequently carry
        // alpha == 0. Passed straight to `ColorImage::from_rgba_unmultiplied`
        // that makes every pixel fully transparent, so the poster / frames are
        // decoded fine yet render invisible. Force alpha to opaque (255).
        for y in 0..h as usize {
            let src = ptr.add(y * row);
            let dst = &mut rgba[y * w as usize * 4..(y + 1) * w as usize * 4];
            for x in 0..w as usize {
                let b = *src.add(x * 4);
                let g = *src.add(x * 4 + 1);
                let r = *src.add(x * 4 + 2);
                let di = x * 4;
                dst[di] = r;
                dst[di + 1] = g;
                dst[di + 2] = b;
                dst[di + 3] = 255;
            }
        }
        let _ = buf.Unlock();
        // Playback frames are downscaled before they leave the worker: a full
        // 1920x1080 RGBA frame costs ~8 MB to convert, copy and upload every
        // single tick, which is what made playback eat a core. max_dim == 0
        // means "keep native" (poster path scales later, if at all).
        if max_dim > 0 {
            Some(downscale_rgba(&rgba, w, h, max_dim))
        } else {
            Some((w, h, rgba))
        }
    }
}

/// Seek a reader to `ns` (100-ns units). `GUID_NULL` + `VT_I8` is the one
/// time format every media source must support. MF lands on the nearest key
/// frame at or before the target, so the caller must tolerate a small offset.
pub fn seek_reader(reader: &IMFSourceReader, ns: u64) -> bool {
    unsafe {
        let mut var: PROPVARIANT = std::mem::zeroed();
        // PROPVARIANT.Anonymous is PROPVARIANT_0 (union); its Anonymous is a
        // ManuallyDrop<PROPVARIANT_0_0> holding vt + the value union.
        let inner = &mut *var.Anonymous.Anonymous;
        inner.vt = VT_I8;
        inner.Anonymous.hVal = ns as i64;
        reader
            .SetCurrentPosition(&GUID::zeroed(), &var)
            .is_ok()
    }
}

/// Clip duration in 100-ns units, or `None` when unknown.
#[allow(dead_code)]
pub fn clip_duration(path: &Path) -> Option<u64> {
    if !ensure_mf() {
        return None;
    }
    let url = wide(path)?;
    let p = open_video(&url)?;
    unsafe {
        p.reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .ok()
            .and_then(|v| PropVariantToUInt64(&v).ok())
    }
}

/// Format a 100-ns timestamp as `M:SS` / `H:MM:SS`.
pub fn fmt_time(ns: u64) -> String {
    let total = ns / 10_000_000; // 100ns -> s
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}
