//! Best-effort loader for Windows Explorer's pinned "Quick Access" folders.
//!
//! Explorer stores its jump list under
//! `%APPDATA%\Microsoft\Windows\Recent\AutomaticDestinations\
//!   f01b4d95cf55d32a.automaticDestinations-ms`
//! which is an OLE2 / Compound-File (CFB) document.  Each numbered stream is a
//! `.lnk` (shell link) whose target is the pinned folder.
//!
//! Everything here is *best effort*: any malformed byte, missing field or
//! unreadable file yields an empty result for that part rather than a panic.
//! The caller still validates that each returned path is an existing directory,
//! so a wrong guess can never be navigated to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

const CFB_MAGIC: [u8; 8] = [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
const END_OF_CHAIN: usize = 0xFFFF_FFFF;
const FREE_SECTOR: usize = 0xFFFF_FFFE;

/// One row of Windows Explorer's Quick Access list.
pub struct QaEntry {
    pub path: PathBuf,
    /// `true` = Explorer shows it under "已固定", `false` = recent-only.
    pub pinned: bool,
}

/// Load Windows Explorer's Quick Access folders, keeping the pinned/recent
/// split that Explorer itself shows.
///
/// The `DestList` stream is the authoritative index. Each entry has a fixed
/// part followed by a UTF-16 path, and carries an explicit *pin status*
/// (`-1` = only recent, `>= 0` = pinned; the value doubles as the display
/// order of the pinned group). Entries whose target is a shell known folder are
/// stored as `knownfolder:{GUID}` rather than a plain path — half of a default
/// Windows pin set is exactly that (Desktop / Downloads / Documents / …), so
/// those must be resolved through `SHGetKnownFolderPath` or the whole pinned
/// group silently disappears.
///
/// Falls back to plain path scanning (everything unpinned) for DestList v1
/// (Windows 7) or an unparsable stream.
pub fn load_windows_quick_access() -> Vec<QaEntry> {
    let Some(path) = jump_list_path() else {
        return vec![];
    };
    let Ok(data) = std::fs::read(&path) else {
        return vec![];
    };
    let Some(streams) = parse_cfb(&data) else {
        return vec![];
    };

    if let Some(dl) = streams.get("DestList") {
        let out = parse_dest_list(dl);
        if !out.is_empty() {
            return out;
        }
    }

    // Fallback: scan the numbered `.lnk` streams (or DestList v1) for paths.
    let mut scanned: Vec<String> = Vec::new();
    if let Some(dl) = streams.get("DestList") {
        scanned = scan_utf16_paths(dl);
    }
    if scanned.is_empty() {
        let mut names: Vec<&String> = streams
            .keys()
            .filter(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_hexdigit()))
            .collect();
        // Stream names are hexadecimal sequence numbers.
        names.sort_by_key(|n| u64::from_str_radix(n, 16).unwrap_or(u64::MAX));
        for name in names {
            if let Some(target) = lnk_target_path(&streams[name]) {
                scanned.push(target);
            }
        }
    }

    let mut out: Vec<QaEntry> = Vec::new();
    for p in scanned {
        let pb = PathBuf::from(p);
        if pb.is_dir() && !out.iter().any(|e| e.path == pb) {
            out.push(QaEntry { path: pb, pinned: false });
        }
    }
    out.truncate(MAX_QUICK_ACCESS);
    out
}

/// Fixed part of a DestList entry (v2+ layout, 0x80 bytes) plus the u16 path
/// size at `0x80` and the path itself at `0x82`.
const DL_PIN_OFF: usize = 0x6C;
const DL_PATH_SIZE_OFF: usize = 0x80;
const DL_PATH_OFF: usize = 0x82;
/// Alignment padding after a v2+ path, before the next entry.
const DL_TRAILING: usize = 4;

/// Walk the DestList entries of a v2+ (Windows 8 and later) jump list.
fn parse_dest_list(dl: &[u8]) -> Vec<QaEntry> {
    if dl.len() < 32 {
        return vec![];
    }
    let version = u32::from_le_bytes([dl[0], dl[1], dl[2], dl[3]]);
    if version < 2 {
        return vec![]; // v1 (Windows 7) has a different layout — use fallback.
    }
    let n_entries = u32::from_le_bytes([dl[4], dl[5], dl[6], dl[7]]) as usize;

    let mut pinned: Vec<(i32, PathBuf)> = Vec::new();
    let mut recent: Vec<PathBuf> = Vec::new();
    let mut off = 32usize;
    for _ in 0..n_entries {
        if off + DL_PATH_OFF > dl.len() {
            break;
        }
        let pin = i32::from_le_bytes([
            dl[off + DL_PIN_OFF],
            dl[off + DL_PIN_OFF + 1],
            dl[off + DL_PIN_OFF + 2],
            dl[off + DL_PIN_OFF + 3],
        ]);
        let psize = u16::from_le_bytes([dl[off + DL_PATH_SIZE_OFF], dl[off + DL_PATH_SIZE_OFF + 1]])
            as usize;
        let pstart = off + DL_PATH_OFF;
        let pend = pstart + psize * 2;
        if psize == 0 || pend > dl.len() {
            break;
        }
        let raw = decode_utf16(&dl[pstart..pend]).unwrap_or_default();
        off = (pend + DL_TRAILING).min(dl.len());

        let Some(path) = resolve_target(&raw) else {
            continue;
        };
        if pinned.iter().any(|(_, p)| *p == path) || recent.contains(&path) {
            continue;
        }
        if pin >= 0 {
            pinned.push((pin, path));
        } else {
            recent.push(path);
        }
    }

    // The pin status doubles as the display order of Explorer's pinned group.
    pinned.sort_by_key(|(p, _)| *p);
    let mut out: Vec<QaEntry> = pinned
        .into_iter()
        .map(|(_, path)| QaEntry { path, pinned: true })
        .collect();
    let room = MAX_QUICK_ACCESS.saturating_sub(out.len());
    out.extend(recent.into_iter().take(room).map(|path| QaEntry { path, pinned: false }));
    out
}

/// Turn one DestList path field into an existing directory.
///
/// Shell known folders are recorded as `knownfolder:{GUID}`; everything else is
/// a plain `X:\…` path. Anything that does not resolve to a live directory is
/// dropped, so a bad parse can never be navigated to.
fn resolve_target(raw: &str) -> Option<PathBuf> {
    let raw = raw.trim_end_matches('\0');
    if let Some(rest) = raw.strip_prefix("knownfolder:") {
        let p = known_folder_path(rest.trim())?;
        return p.is_dir().then_some(p);
    }
    if !is_plausible_path(raw) {
        return None;
    }
    let p = PathBuf::from(raw);
    p.is_dir().then_some(p)
}

/// How many jump-list folders the sidebar shows.
const MAX_QUICK_ACCESS: usize = 24;

// ---------------------------------------------------------------------------
// knownfolder:{GUID} — shell known folder resolution
// ---------------------------------------------------------------------------

#[repr(C)]
struct Guid {
    d1: u32,
    d2: u16,
    d3: u16,
    d4: [u8; 8],
}

#[link(name = "shell32")]
extern "system" {
    fn SHGetKnownFolderPath(
        rfid: *const Guid,
        flags: u32,
        token: *mut core::ffi::c_void,
        path: *mut *mut u16,
    ) -> i32;
}

#[link(name = "ole32")]
extern "system" {
    fn CoTaskMemFree(p: *mut core::ffi::c_void);
}

/// `{374DE290-123F-4565-9164-39C4925E467B}` → UTF-8 string.
fn parse_guid(s: &str) -> Option<Guid> {
    let s = s.trim().trim_start_matches('{').trim_end_matches('}');
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 5 {
        return None;
    }
    let d1 = u32::from_str_radix(parts[0], 16).ok()?;
    let d2 = u16::from_str_radix(parts[1], 16).ok()?;
    let d3 = u16::from_str_radix(parts[2], 16).ok()?;
    if parts[3].len() != 4 || parts[4].len() != 12 {
        return None;
    }
    let mut d4 = [0u8; 8];
    let tail = format!("{}{}", parts[3], parts[4]);
    for i in 0..8 {
        d4[i] = u8::from_str_radix(&tail[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(Guid { d1, d2, d3, d4 })
}

/// Resolve a KNOWNFOLDERID to its current path (honours OneDrive redirection).
fn known_folder_path(guid_str: &str) -> Option<PathBuf> {
    let guid = parse_guid(guid_str)?;
    let mut buf: *mut u16 = std::ptr::null_mut();
    // SAFETY: `guid` is a valid GUID, `buf` receives an owned, NUL-terminated
    // UTF-16 string that we free with `CoTaskMemFree` right after copying it.
    let hr = unsafe { SHGetKnownFolderPath(&guid, 0, std::ptr::null_mut(), &mut buf) };
    if hr < 0 || buf.is_null() {
        return None;
    }
    let mut len = 0usize;
    unsafe {
        while *buf.add(len) != 0 {
            len += 1;
        }
        let slice = std::slice::from_raw_parts(buf, len);
        let s = String::from_utf16_lossy(slice);
        CoTaskMemFree(buf as *mut core::ffi::c_void);
        Some(PathBuf::from(s))
    }
}

/// `%APPDATA%\Microsoft\Windows\Recent\AutomaticDestinations\
///   f01b4d95cf55d32a.automaticDestinations-ms`
fn jump_list_path() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    Some(
        Path::new(&appdata)
            .join("Microsoft")
            .join("Windows")
            .join("Recent")
            .join("AutomaticDestinations")
            .join("f01b4d95cf55d32a.automaticDestinations-ms"),
    )
}

// ---------------------------------------------------------------------------
// OLE2 / Compound File (CFB) reader
// ---------------------------------------------------------------------------

/// Parse a CFB document and return every stream keyed by its (UTF-16) name.
fn parse_cfb(data: &[u8]) -> Option<HashMap<String, Vec<u8>>> {
    if data.len() < 512 {
        return None;
    }
    if data[0..8] != CFB_MAGIC {
        return None;
    }
    let sector_shift = u16::from_le_bytes([data[30], data[31]]) as usize;
    let mini_shift = u16::from_le_bytes([data[32], data[33]]) as usize;
    if sector_shift < 9 || sector_shift > 16 || mini_shift < 6 || mini_shift > 12 {
        return None;
    }
    let ss = 1usize << sector_shift; // sector size
    let ms = 1usize << mini_shift; // mini-sector size
    if ss == 0 || ms == 0 {
        return None;
    }
    let num_fat = u32::from_le_bytes(data[44..48].try_into().ok()?) as usize;
    let first_dir = u32::from_le_bytes(data[48..52].try_into().ok()?) as usize;
    let mini_cutoff = u32::from_le_bytes(data[56..60].try_into().ok()?) as u64;
    let first_minifat = u32::from_le_bytes(data[60..64].try_into().ok()?) as usize;
    let _num_minifat = u32::from_le_bytes(data[64..68].try_into().ok()?) as usize;
    if std::env::var("CFB_DEBUG").is_ok() {
        eprintln!(
            "sector_shift={sector_shift} ss={ss} mini_shift={mini_shift} ms={ms} num_fat={num_fat} \
             first_dir={first_dir} mini_cutoff={mini_cutoff} first_minifat={first_minifat}",
        );
    }

    // ---- gather FAT sector ids (header array + DIFAT chain) ----
    let mut fat_sectors: Vec<usize> = Vec::with_capacity(num_fat.max(1));
    for i in 0..109.min(num_fat) {
        let off = 76 + i * 4;
        if off + 4 > data.len() {
            break;
        }
        fat_sectors.push(u32::from_le_bytes(data[off..off + 4].try_into().ok()?) as usize);
    }
    let mut difat = u32::from_le_bytes(data[68..72].try_into().ok()?) as usize;
    while difat != END_OF_CHAIN && difat != FREE_SECTOR && fat_sectors.len() < num_fat {
        // Sector N lives at byte (N + 1) * ss — the 512-byte header is *not*
        // part of sector 0. Forgetting the +1 shifts the whole FAT by one
        // sector, which silently truncates the directory to a single sector.
        let base = (difat + 1) * ss;
        if base + ss > data.len() {
            break;
        }
        let entries = ss / 4;
        for j in 1..entries {
            if fat_sectors.len() >= num_fat {
                break;
            }
            let off = base + j * 4;
            if off + 4 > data.len() {
                break;
            }
            fat_sectors.push(u32::from_le_bytes(data[off..off + 4].try_into().ok()?) as usize);
        }
        difat = u32::from_le_bytes(data[base..base + 4].try_into().ok()?) as usize;
    }

    // ---- build the FAT (sector -> next sector), indexed by sector number ----
    let mut fat: Vec<usize> = Vec::with_capacity(fat_sectors.len() * (ss / 4));
    for &fs in &fat_sectors {
        let base = (fs + 1) * ss;
        if base + ss > data.len() {
            // missing sector: pad with terminators so indexing stays safe
            fat.resize(fat.len() + ss / 4, END_OF_CHAIN);
            continue;
        }
        for k in 0..ss / 4 {
            let off = base + k * 4;
            let v = u32::from_le_bytes(data[off..off + 4].try_into().ok()?);
            fat.push(v as usize);
        }
    }

    let read_chain = |first: usize, fat: &[usize]| -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = first;
        let mut guard = 0;
        while cur < fat.len() && cur != END_OF_CHAIN && cur != FREE_SECTOR && guard < 100_000 {
            // CFB sector N begins at byte (N + 1) * ss — the 512-byte header
            // precedes sector 0.
            let base = (cur + 1) * ss;
            if base + ss > data.len() {
                break;
            }
            out.extend_from_slice(&data[base..base + ss]);
            cur = fat[cur];
            guard += 1;
        }
        out
    };

    // ---- read the directory to find the root (mini-stream) entry ----
    let dir_data = read_chain(first_dir, &fat);
    let mut root_start = 0usize;
    let mut root_size = 0u64;
    let mut found_root = false;
    for e in 0..dir_data.len() / 128 {
        let off = e * 128;
        if off + 128 > dir_data.len() {
            break;
        }
        let obj_type = dir_data[off + 66];
        if obj_type == 5 {
            root_start = u32::from_le_bytes(dir_data[off + 116..off + 120].try_into().ok()?) as usize;
            root_size = u64::from_le_bytes(dir_data[off + 120..off + 128].try_into().ok()?);
            found_root = true;
            break;
        }
    }
    let mini_stream = if found_root {
        let mut s = read_chain(root_start, &fat);
        // Bound the mini-stream to its declared size (the last sector is padded).
        if (root_size as usize) < s.len() {
            s.truncate(root_size as usize);
        }
        s
    } else {
        Vec::new()
    };

    // ---- build the mini-FAT (mini-sector -> next mini-sector) ----
    let minifat_data = read_chain(first_minifat, &fat);
    let mut minifat: Vec<usize> = Vec::with_capacity(minifat_data.len() / 4);
    for k in 0..minifat_data.len() / 4 {
        let off = k * 4;
        let v = u32::from_le_bytes(minifat_data[off..off + 4].try_into().ok()?);
        minifat.push(v as usize);
    }

    let read_mini_chain = |first: usize| -> Vec<u8> {
        let mut out = Vec::new();
        let mut cur = first;
        let mut guard = 0;
        while cur < minifat.len() && cur != END_OF_CHAIN && cur != FREE_SECTOR && guard < 1_000_000 {
            let base = cur * ms;
            if base + ms > mini_stream.len() {
                break;
            }
            out.extend_from_slice(&mini_stream[base..base + ms]);
            cur = minifat[cur];
            guard += 1;
        }
        out
    };

    // ---- enumerate all directory entries, collect streams ----
    let mut result: HashMap<String, Vec<u8>> = HashMap::new();
    let mut debug_entries = 0usize;
    for e in 0..dir_data.len() / 128 {
        let off = e * 128;
        if off + 128 > dir_data.len() {
            break;
        }
        let obj_type = dir_data[off + 66];
        let name_len = u16::from_le_bytes([dir_data[off + 64], dir_data[off + 65]]) as usize;
        if std::env::var("CFB_DEBUG").is_ok() && debug_entries < 12 {
            let start = u32::from_le_bytes(dir_data[off + 116..off + 120].try_into().ok()?);
            let size = u64::from_le_bytes(dir_data[off + 120..off + 128].try_into().ok()?);
            eprintln!("  entry[{e}] obj_type={obj_type} name_len={name_len} start={start} size={size}");
            debug_entries += 1;
        }
        if obj_type != 2 && obj_type != 5 {
            continue; // only streams (and the root, which we already used)
        }
        if name_len == 0 || name_len > 64 {
            continue;
        }
        let name = match decode_utf16(&dir_data[off..off + name_len]) {
            Some(n) => n,
            None => continue,
        };
        if name.is_empty() {
            continue;
        }
        let start = u32::from_le_bytes(dir_data[off + 116..off + 120].try_into().ok()?) as usize;
        let size = u64::from_le_bytes(dir_data[off + 120..off + 128].try_into().ok()?);

        if obj_type == 5 {
            continue; // root storage itself — already consumed as mini-stream
        }
        let content = if size > 0 && (size as usize) < mini_cutoff as usize {
            let mut c = read_mini_chain(start);
            c.truncate(size as usize);
            c
        } else {
            let mut c = read_chain(start, &fat);
            c.truncate(size as usize);
            c
        };
        result.entry(name).or_insert(content);
    }
    if std::env::var("CFB_DEBUG").is_ok() {
        eprintln!("dir_data.len()={} streams collected={}", dir_data.len(), result.len());
        for (n, c) in &result {
            eprintln!("  stream {n:?} len={}", c.len());
        }
    }

    Some(result)
}

/// Decode a UTF-16LE byte slice into a String (best effort).
fn decode_utf16(bytes: &[u8]) -> Option<String> {
    if bytes.len() % 2 != 0 {
        return None;
    }
    let mut s = String::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let u = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        if u == 0 {
            break;
        }
        match char::from_u32(u as u32) {
            Some(c) => s.push(c),
            None => s.push('\u{FFFD}'),
        }
        i += 2;
    }
    Some(s)
}

// ---------------------------------------------------------------------------
// .lnk target extraction
// ---------------------------------------------------------------------------

/// Pull a Windows path out of a `.lnk` stream.
///
/// Strategy (first hit wins, and the caller still validates `is_dir`):
///   1. Parse the LinkInfo `LocalBasePath` (ANSI, or UTF-16 when the Unicode
///      offset is present) — the most reliable source.
///   2. Fall back to scanning for an embedded UTF-16LE `X:\…` path.
///   3. Fall back to scanning for an embedded ASCII `X:\…` path.
fn lnk_target_path(stream: &[u8]) -> Option<String> {
    if let Some(p) = lnk_linkinfo_path(stream) {
        if is_plausible_path(&p) {
            return Some(p);
        }
    }
    if let Some(p) = scan_utf16_path(stream) {
        if is_plausible_path(&p) {
            return Some(p);
        }
    }
    if let Some(p) = scan_ascii_path(stream) {
        if is_plausible_path(&p) {
            return Some(p);
        }
    }
    None
}

/// Parse the shell-link header and read the LinkInfo `LocalBasePath`.
fn lnk_linkinfo_path(stream: &[u8]) -> Option<String> {
    // Header is at least 76 bytes; magic is the fixed header size (0x4C).
    if stream.len() < 76 {
        return None;
    }
    let hdr = u32::from_le_bytes(stream[0..4].try_into().ok()?);
    if hdr != 0x4C {
        return None;
    }
    let flags = u32::from_le_bytes(stream[20..24].try_into().ok()?);
    let mut cur = 76usize;
    if flags & 0x1 != 0 {
        // HasLinkTargetIDList: u16 size, then the IDList bytes.
        if cur + 2 > stream.len() {
            return None;
        }
        let idlist_size = u16::from_le_bytes([stream[cur], stream[cur + 1]]) as usize;
        cur += 2 + idlist_size;
    }
    if flags & 0x2 == 0 {
        return None; // no LinkInfo section
    }
    if cur + 24 > stream.len() {
        return None;
    }
    let hdr_size = u32::from_le_bytes(stream[cur + 4..cur + 8].try_into().ok()?) as usize;
    let local_off = u32::from_le_bytes(stream[cur + 16..cur + 20].try_into().ok()?) as usize;
    let unicode_off = if hdr_size >= 0x24 && cur + 32 <= stream.len() {
        u32::from_le_bytes(stream[cur + 28..cur + 32].try_into().ok()?) as usize
    } else {
        0
    };
    // Prefer the Unicode base path when present, else the ANSI one.
    if unicode_off > 0 && cur + unicode_off < stream.len() {
        if let Some(p) = read_utf16le_path(stream, cur + unicode_off) {
            if is_plausible_path(&p) {
                return Some(p);
            }
        }
    }
    if local_off > 0 && cur + local_off < stream.len() {
        if let Some(p) = read_cstr(stream, cur + local_off) {
            return Some(p);
        }
    }
    None
}

/// Read a null-terminated ASCII string starting at byte `start`.
fn read_cstr(stream: &[u8], start: usize) -> Option<String> {
    if start >= stream.len() {
        return None;
    }
    let end = stream[start..].iter().position(|&b| b == 0).map(|i| start + i)?;
    let s = std::str::from_utf8(&stream[start..end]).ok()?.to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Scan a buffer for every UTF-16LE `X:\…` path, in the order they appear.
///
/// Used on the `DestList` stream: each entry stores its target path inline,
/// NUL-terminated, so reading until the terminator is exact (the declared
/// length fields differ between jump-list versions and cannot be trusted).
fn scan_utf16_paths(buf: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 5 < buf.len() {
        let letter = u16::from_le_bytes([buf[i], buf[i + 1]]);
        let colon = u16::from_le_bytes([buf[i + 2], buf[i + 3]]);
        let back = u16::from_le_bytes([buf[i + 4], buf[i + 5]]);
        if letter < 128
            && (letter as u8).is_ascii_alphabetic()
            && colon == 0x3A
            && back == 0x5C
        {
            if let Some(p) = read_utf16le_path(buf, i) {
                let ok = is_plausible_path(&p) && p.len() < 512 && !p.contains('\u{FFFD}');
                if ok {
                    // Skip past the whole string (NUL terminator included).
                    let step = (p.chars().count() + 1) * 2;
                    out.push(p);
                    i += step;
                    continue;
                }
            }
        }
        i += 2;
    }
    out
}

/// Scan for the first UTF-16LE `X:\…` (letter + colon + backslash) path.
fn scan_utf16_path(stream: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 5 < stream.len() {
        let letter = u16::from_le_bytes([stream[i], stream[i + 1]]);
        let colon = u16::from_le_bytes([stream[i + 2], stream[i + 3]]);
        let back = u16::from_le_bytes([stream[i + 4], stream[i + 5]]);
        if colon == 0x3A && back == 0x5C && (letter as u8).is_ascii_alphabetic() {
            if let Some(p) = read_utf16le_path(stream, i) {
                return Some(p);
            }
        }
        i += 2;
    }
    None
}

/// Scan for the first ASCII `X:\…` (letter + colon + backslash) path.
fn scan_ascii_path(stream: &[u8]) -> Option<String> {
    let mut i = 0;
    while i + 2 < stream.len() {
        if stream[i].is_ascii_alphabetic()
            && stream[i + 1] == b':'
            && stream[i + 2] == b'\\'
        {
            let end = stream[i..]
                .iter()
                .position(|&b| b == 0 || b.is_ascii_control())
                .map(|o| i + o)
                .unwrap_or(stream.len());
            // Only keep printable path characters.
            let path: String = stream[i..end]
                .iter()
                .take_while(|&&b| (0x20..0x7F).contains(&b))
                .map(|&b| b as char)
                .collect();
            if path.len() > 3 {
                return Some(path);
            }
        }
        i += 1;
    }
    None
}

/// Read a null-terminated UTF-16LE path starting at byte `start`.
fn read_utf16le_path(stream: &[u8], start: usize) -> Option<String> {
    let mut s = String::new();
    let mut i = start;
    while i + 1 < stream.len() {
        let u = u16::from_le_bytes([stream[i], stream[i + 1]]);
        i += 2;
        if u == 0 {
            break;
        }
        match char::from_u32(u as u32) {
            Some(c) => s.push(c),
            None => s.push('\u{FFFD}'),
        }
    }
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// A plausible Windows path: contains `:\` (drive) or starts with `\\`.
fn is_plausible_path(p: &str) -> bool {
    (p.contains(":\\") || p.starts_with("\\\\")) && p.chars().all(|c| !c.is_control())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_qa_does_not_panic() {
        // Should never panic; returns whatever it can (possibly empty).
        let list = load_windows_quick_access();
        println!("windows_qa: {} existing folders", list.len());
        for e in &list {
            println!("  qa: pinned={} {}", e.pinned, e.path.display());
        }
    }

    #[test]
    fn cfb_parses_real_file_and_extracts_paths() {
        let Some(path) = jump_list_path() else {
            eprintln!("skip: no APPDATA");
            return;
        };
        let Ok(data) = std::fs::read(&path) else {
            eprintln!("skip: jump-list file not readable: {}", path.display());
            return;
        };
        let streams = parse_cfb(&data).expect("parse_cfb should succeed on a real file");
        println!("cfb streams: {}", streams.len());
        let mut extracted = 0;
        for (name, bytes) in &streams {
            if name.chars().all(|c| c.is_ascii_digit()) && !name.is_empty() {
                if let Some(p) = lnk_target_path(bytes) {
                    extracted += 1;
                    if is_plausible_path(&p) {
                        println!("  stream {name}: {p}");
                    }
                }
            }
        }
        println!("cfb: extracted {extracted} target paths from numbered streams");
        assert!(extracted > 0, "expected to extract at least one target path");
    }
}
