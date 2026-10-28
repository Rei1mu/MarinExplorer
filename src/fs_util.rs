//! Platform & format helpers. No new dependencies.
//!
//! - Drive enumeration + free-space query (`fsutil` on Windows).
//! - Recycle-Bin metadata parsing (reads the `$I<suffix>` files to recover the
//!   original filename, size and deletion time).
//! - `open` a file / open a terminal in the given directory.
//! - Percent-encoding (for the on-disk cache keys) and UTF-16LE decoding.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Spawn a child with no console window. Every helper below that shells out to
/// a console-subsystem tool (`powershell`, `cmd`, `where.exe`) passes this, so
/// the GUI never flashes a black window. `open_terminal` deliberately uses
/// `CREATE_NEW_CONSOLE` instead — it *wants* a visible terminal.
#[cfg(target_os = "windows")]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "windows")]
use std::os::windows::process::CommandExt;

/// One fixed drive with its capacity.
#[derive(Clone)]
pub struct DriveInfo {
    pub letter: char,
    pub total: u64,
    pub free: u64,
}

impl DriveInfo {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }
    /// Fraction used, 0.0..1.0 (0.0 when total unknown).
    pub fn used_frac(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.used() as f32) / (self.total as f32)
        }
    }
}

/// Enumerate fixed drives (A–Z that exist) and query their free space.
/// `fsutil volume diskfree <drive>` prints the totals; we parse those numbers.
/// Non-Windows or unsupported volumes yield total=free=0 (rendered as "-").
pub fn enumerate_drives() -> Vec<DriveInfo> {
    let mut out = vec![];
    for c in 'A'..='Z' {
        let root = PathBuf::from(format!("{}:\\", c));
        if !root.is_dir() {
            continue;
        }
        let (total, free) = disk_free(&root).unwrap_or((0, 0));
        out.push(DriveInfo { letter: c, total, free });
    }
    out
}

#[cfg(target_os = "windows")]
fn disk_free(root: &Path) -> Option<(u64, u64)> {
    // Use the Win32 API directly: `fsutil volume diskfree` prints *localized*
    // text (e.g. Chinese "总字节数"), so parsing its output is unreliable.
    // GetDiskFreeSpaceExW is language-independent and needs no extra crate.
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lp_directory_name: *const u16,
            lp_free_bytes_available_to_caller: *mut u64,
            lp_total_number_of_bytes: *mut u64,
            lp_total_number_of_free_bytes: *mut u64,
        ) -> i32;
    }
    let wide: Vec<u16> = root
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut free_to_caller = 0u64;
    let mut total = 0u64;
    let mut total_free = 0u64;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok != 0 && total > 0 {
        Some((total, total_free))
    } else {
        None
    }
}

#[cfg(not(target_os = "windows"))]
fn disk_free(_root: &Path) -> Option<(u64, u64)> {
    None
}

/// The current user's SID (`S-1-5-21-...`), used to locate their
/// `$Recycle.Bin\<SID>` folder. Cached on disk so we only spawn once.
///
/// We ask PowerShell for `[WindowsIdentity]::GetCurrent().User.Value`, which
/// returns the SID string directly and is not shadowed by the coreutils
/// `whoami` shim that Git-Bash puts on `PATH` (the bare `whoami` would fail
/// with "extra operand" and never yield a SID).
pub fn get_user_sid() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for tok in text.split_whitespace() {
            if tok.starts_with("S-1-5-") && tok.len() > 8 {
                return Some(tok.to_string());
            }
        }
        None
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Open a file with its default application.
pub fn open_default_file(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        // `start` needs an empty title slot before the real path.
        Command::new("cmd")
            .args(["/C", "start", "", &path.to_string_lossy()])
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok();
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn().ok();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn().ok();
    }
}

/// Locate VS Code once: `code` on PATH first, then the usual install
/// locations. Cached — the context menu asks every frame.
pub fn vscode_exe() -> Option<&'static Path> {
    #[cfg(target_os = "windows")]
    {
        use std::sync::OnceLock;
        static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
        FOUND
            .get_or_init(|| {
                if let Ok(out) = Command::new("where.exe")
                    .arg("code")
                    .creation_flags(CREATE_NO_WINDOW)
                    .output()
                {
                    if out.status.success() {
                        if let Some(line) = String::from_utf8_lossy(&out.stdout).lines().next() {
                            let p = PathBuf::from(line.trim());
                            if p.is_file() {
                                return Some(p);
                            }
                        }
                    }
                }
                let mut cands: Vec<PathBuf> = Vec::new();
                for var in ["LOCALAPPDATA", "PROGRAMFILES", "PROGRAMFILES(X86)"] {
                    if let Ok(base) = std::env::var(var) {
                        cands.push(PathBuf::from(base).join("Microsoft VS Code").join("Code.exe"));
                    }
                }
                cands.into_iter().find(|p| p.is_file())
            })
            .as_deref()
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Open `path` (a file *or* a directory) in VS Code.
pub fn open_with_vscode(path: &Path) -> bool {
    match vscode_exe() {
        Some(exe) => Command::new(exe).arg(path).spawn().is_ok(),
        None => false,
    }
}

/// Open a file in Notepad (files only — Notepad has no folder mode).
pub fn open_with_notepad(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        Command::new("notepad.exe").arg(path).spawn().is_ok()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        false
    }
}

/// Open a terminal with its working directory set to `dir`.
pub fn open_terminal(dir: &Path) {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        // This app is a console-subsystem binary, so a plain `spawn()` lets
        // the child inherit the parent console: no new window ever appears
        // and the toolbar button looks dead. `CREATE_NEW_CONSOLE` forces a
        // real window of its own.
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        // Prefer Windows Terminal (Win11 default); fall back to powershell.
        let wt = Command::new("wt.exe")
            .arg("-d")
            .arg(dir)
            .creation_flags(CREATE_NEW_CONSOLE)
            .spawn();
        if wt.is_err() {
            Command::new("powershell.exe")
                .current_dir(dir)
                .creation_flags(CREATE_NEW_CONSOLE)
                .spawn()
                .ok();
        }
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg("-a").arg("Terminal").arg(dir).spawn().ok();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("x-terminal-emulator").current_dir(dir).spawn().ok();
    }
}

/// Show `path` selected inside Windows Explorer.
///
/// `explorer.exe /select,` is the documented switch, but the command line has
/// to reach the shell **verbatim**. `std::process::Command` cannot do that: it
/// escapes the quotes inside `/select,"…"` into `\"`, and explorer.exe's own
/// command-line parser reads the backslash as part of the path — the parent
/// folder then opens and the item is never selected (the "it showed the
/// folder, not my file" bug). So the line is built by hand and handed to
/// `CreateProcessW`, which is the form every other program uses:
/// `explorer.exe /select,"D:\dir\file.txt"`.
///
/// `explorer.exe /select,` only selects *files*; directories are opened
/// instead.
pub fn reveal_in_explorer(path: &Path) {
    #[cfg(target_os = "windows")]
    {
        let line = if path.is_dir() {
            format!("explorer.exe \"{}\"", path.display())
        } else {
            format!("explorer.exe /select,\"{}\"", path.display())
        };
        spawn_line(&line);
    }
    #[cfg(not(target_os = "windows"))]
    {
        if path.is_dir() {
            open_default_file(path);
        } else if let Some(p) = path.parent() {
            open_default_file(p);
        }
    }
}

/// Start a process from a **pre-built** command line, byte for byte.
///
/// Needed for `explorer.exe /select,"…"`: `Command::arg` re-quotes every
/// argument that contains a space and backslash-escapes the quotes we put in,
/// which explorer.exe cannot read. Returns false when the shell refuses to
/// start anything.
#[cfg(target_os = "windows")]
fn spawn_line(line: &str) -> bool {
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct StartupInfoW {
        cb: u32,
        reserved: *const u16,
        desktop: *const u16,
        title: *const u16,
        x: u32,
        y: u32,
        x_size: u32,
        y_size: u32,
        x_chars: u32,
        y_chars: u32,
        fill: u32,
        flags: u32,
        show_window: u16,
        cb_reserved2: u16,
        reserved2: *const u8,
        std_in: *mut core::ffi::c_void,
        std_out: *mut core::ffi::c_void,
        std_err: *mut core::ffi::c_void,
    }

    #[repr(C)]
    struct ProcessInformation {
        process: *mut core::ffi::c_void,
        thread: *mut core::ffi::c_void,
        pid: u32,
        tid: u32,
    }

    // Renamed through `link_name`: `tray.rs` and `explorer.rs` already declare
    // these two with different (handle-as-integer) signatures, and a second
    // declaration under the same name is a `clashing_extern_declarations`
    // warning.
    #[link(name = "kernel32")]
    extern "system" {
        #[link_name = "CreateProcessW"]
        fn create_process_w(
            app: *const u16,
            cmd: *mut u16,
            pa: *const core::ffi::c_void,
            ta: *const core::ffi::c_void,
            inherit: i32,
            flags: u32,
            env: *const core::ffi::c_void,
            dir: *const u16,
            si: *const StartupInfoW,
            pi: *mut ProcessInformation,
        ) -> i32;
        #[link_name = "CloseHandle"]
        fn close_handle(h: *mut core::ffi::c_void) -> i32;
    }

    let mut wide: Vec<u16> = std::ffi::OsStr::new(line)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut si: StartupInfoW = unsafe { std::mem::zeroed() };
    si.cb = std::mem::size_of::<StartupInfoW>() as u32;
    let mut pi: ProcessInformation = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        create_process_w(
            std::ptr::null(),
            wide.as_mut_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            0,
            std::ptr::null(),
            std::ptr::null(),
            &si,
            &mut pi,
        )
    };
    if ok == 0 {
        return false;
    }
    // The handles are ours the moment the process exists — drop them, or every
    // "show in Explorer" click would leak two kernel handles.
    unsafe {
        close_handle(pi.process);
        close_handle(pi.thread);
    }
    true
}

/// Open a *folder* in the native file explorer (Windows Explorer).
///
/// Unlike [`reveal_in_explorer`] this passes the plain folder path — no
/// `/select,` switch — which Explorer handles reliably even with spaces in
/// the path. Used by the convert dialog's "open native output folder";
/// "open output folder" itself stays inside this app (new tab).
pub fn open_native_folder(dir: &Path) {
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer.exe").arg(dir).spawn().ok();
    }
    #[cfg(not(target_os = "windows"))]
    {
        open_default_file(dir);
    }
}

/// Move `path` to the Recycle Bin (undoable), like Explorer's Delete key.
///
/// Uses `SHFileOperationW` with `FOF_ALLOWUNDO`: a plain Win32 call, no COM and
/// no extra crate. Falls back to permanent deletion only when the shell cannot
/// recycle the item (e.g. a network share or a file too big for the bin).
#[cfg(target_os = "windows")]
pub fn move_to_recycle_bin(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct ShFileOpStructW {
        hwnd: *mut core::ffi::c_void,
        w_func: u32,
        p_from: *const u16,
        p_to: *const u16,
        f_flags: u16,
        f_any_operations_aborted: i32,
        h_name_mappings: *mut core::ffi::c_void,
        lpsz_progress_title: *const u16,
    }

    #[link(name = "shell32")]
    extern "system" {
        fn SHFileOperationW(op: *mut ShFileOpStructW) -> i32;
    }

    const FO_DELETE: u32 = 3;
    const FOF_SILENT: u16 = 0x0004;
    const FOF_NOCONFIRMATION: u16 = 0x0010;
    const FOF_ALLOWUNDO: u16 = 0x0040;
    const FOF_NOERRORUI: u16 = 0x0400;

    // `p_from` must be a double-NUL-terminated list of paths.
    let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
    from.push(0);
    from.push(0);

    let mut op = ShFileOpStructW {
        hwnd: std::ptr::null_mut(),
        w_func: FO_DELETE,
        p_from: from.as_ptr(),
        p_to: std::ptr::null(),
        f_flags: FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_NOERRORUI,
        f_any_operations_aborted: 0,
        h_name_mappings: std::ptr::null_mut(),
        lpsz_progress_title: std::ptr::null(),
    };
    let rc = unsafe { SHFileOperationW(&mut op) };
    if rc == 0 && op.f_any_operations_aborted == 0 {
        Ok(())
    } else {
        Err(shell_delete_error(rc))
    }
}

/// Human text for an `SHFileOperationW` return code. The raw numbers are
/// Win32 error codes; without this a locked file just made the status bar
/// flash "shell 返回 32", which read like a silent no-op.
#[cfg(target_os = "windows")]
fn shell_delete_error(rc: i32) -> String {
    match rc {
        0 => "操作已取消".into(),
        2 => "文件不存在".into(),
        3 => "路径不存在".into(),
        5 => "没有权限".into(),
        32 => "文件正被其他程序占用，请先关闭使用它的程序".into(),
        _ => format!("系统错误 {rc}（可能被占用或没有权限）"),
    }
}

#[cfg(not(target_os = "windows"))]
pub fn move_to_recycle_bin(path: &Path) -> Result<(), String> {
    if path.is_dir() {
        std::fs::remove_dir_all(path).map_err(|e| e.to_string())
    } else {
        std::fs::remove_file(path).map_err(|e| e.to_string())
    }
}

/// `path` expressed relative to `base` when it lives underneath it.
///
/// `("C:\\a\\b\\c.txt", "C:\\a\\b")` -> `Some("c.txt")`.
pub fn relative_to(path: &Path, base: &Path) -> Option<String> {
    let p = path.strip_prefix(base).ok()?;
    let s = p.to_string_lossy().replace('/', "\\");
    if s.is_empty() {
        Some(".".to_string())
    } else {
        Some(s)
    }
}

/// Current UTC offset in seconds (e.g. +8h in China), used so the 修改时间 /
/// 修改日期 columns show wall-clock time instead of UTC.
///
/// Computed once from `GetLocalTime` vs `GetSystemTime`. DST transitions mean
/// old timestamps can be off by an hour, which is acceptable for a file list.
#[cfg(target_os = "windows")]
pub fn local_offset_secs() -> i64 {
    #[repr(C)]
    struct SysTime {
        year: u16,
        month: u16,
        day_of_week: u16,
        day: u16,
        hour: u16,
        minute: u16,
        second: u16,
        millis: u16,
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetLocalTime(lp_system_time: *mut SysTime);
        fn GetSystemTime(lp_system_time: *mut SysTime);
    }

    fn to_unix(t: &SysTime) -> i64 {
        let days = days_from_civil(t.year as i64, t.month as u32, t.day as u32);
        days * 86400 + (t.hour as i64) * 3600 + (t.minute as i64) * 60 + t.second as i64
    }

    let mut local = SysTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    let mut utc = SysTime {
        year: 0,
        month: 0,
        day_of_week: 0,
        day: 0,
        hour: 0,
        minute: 0,
        second: 0,
        millis: 0,
    };
    unsafe {
        GetLocalTime(&mut local);
        GetSystemTime(&mut utc);
    }
    if local.year < 1970 || utc.year < 1970 {
        return 0;
    }
    // Round to the nearest minute to avoid jitter from the millisecond field.
    let diff = to_unix(&local) - to_unix(&utc);
    (diff as f64 / 60.0).round() as i64 * 60
}

#[cfg(not(target_os = "windows"))]
pub fn local_offset_secs() -> i64 {
    0
}

/// Howard Hinnant's days-from-civil algorithm (inverse of the one in `app`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}


/// A recovered Recycle-Bin item (parsed from its `$I` metadata).
#[derive(Clone, Debug)]
pub struct RecItem {
    /// Display name (file name only, taken from the end of `orig_path`).
    pub name: String,
    /// Full path the item was deleted from (`$I` stores it; empty for an
    /// orphaned `$R` payload whose metadata file is gone).
    pub orig_path: String,
    pub is_dir: bool,
    pub size: u64,
    pub deleted_at: u64, // unix seconds
    pub suffix: String,  // the part after `$I` (matches the `$R` file)
}

/// FILETIME for 1970-01-01 = 11_644_473_600 seconds × 10^7 ticks. It is
/// 1.1644e17, NOT 1.1644e19 — with the extra zero every real timestamp
/// saturated to 0, so the whole bin showed "1970-01-01" as its deletion date.
const FILETIME_UNIX_EPOCH: u64 = 116_444_736_000_000_000;

pub fn filetime_to_unix(ft: u64) -> u64 {
    ft.saturating_sub(FILETIME_UNIX_EPOCH) / 10_000_000
}

/// Parse a recycle-bin `$I` metadata file.
///
/// Two on-disk layouts exist on Windows:
///   * Legacy (Vista/7/8, version 1): bytes 0..4 are the `"RbIJ"` signature,
///     then version(4), FILETIME(8..16), size(16..24), name(24..).
///   * Modern (Win10/11, version 2): bytes 0..4 are the version number (2),
///     then reserved(4), size u64(8..16), FILETIME(16..24), name-length(24..28),
///     name UTF-16LE(28..).  No `"RbIJ"` signature is present.
///
/// We detect the legacy format by the signature and fall back to the version
/// field for everything else, so both parse correctly.
pub fn parse_recycle_i(bytes: &[u8], suffix: &str) -> Option<RecItem> {
    if bytes.len() < 28 {
        return None;
    }
    let head = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
    if head == 0x4A_49_62_52 {
        // Legacy signature format.
        let deleted_at = filetime_to_unix(u64::from_le_bytes(bytes[8..16].try_into().ok()?));
        let size = u64::from_le_bytes(bytes[16..24].try_into().ok()?);
        let name = utf16le_read_until_null(bytes, 24);
        if name.is_empty() {
            return None;
        }
        return Some(RecItem {
            name,
            orig_path: String::new(),
            is_dir: false,
            size,
            deleted_at,
            suffix: suffix.to_string(),
        });
    }
    // Modern format: bytes[0..4] is the version number (1 or 2).
    if head != 1 && head != 2 {
        return None;
    }
    let size = u64::from_le_bytes(bytes[8..16].try_into().ok()?);
    let deleted_at = filetime_to_unix(u64::from_le_bytes(bytes[16..24].try_into().ok()?));
    let name = utf16le_read_until_null(bytes, 28);
    if name.is_empty() {
        return None;
    }
    Some(RecItem {
        name,
        orig_path: String::new(),
        is_dir: false,
        size,
        deleted_at,
        suffix: suffix.to_string(),
    })
}

/// Read a UTF-16LE null-terminated string starting at byte `off`.
fn utf16le_read_until_null(bytes: &[u8], off: usize) -> String {
    let mut i = off;
    let mut raw = Vec::new();
    while i + 1 < bytes.len() {
        let pair = [bytes[i], bytes[i + 1]];
        if pair == [0, 0] {
            break;
        }
        raw.extend_from_slice(&pair);
        i += 2;
    }
    utf16le_to_string(&raw)
}

/// Decode UTF-16LE, handling surrogate pairs (needed for Chinese/emoji names).
pub fn utf16le_to_string(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let u = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
        i += 2;
        match char::from_u32(u as u32) {
            Some(c) => out.push(c),
            None => {
                if (0xD800..=0xDBFF).contains(&u) && i + 1 < bytes.len() {
                    let lo = u16::from_le_bytes([bytes[i], bytes[i + 1]]);
                    i += 2;
                    if (0xDC00..=0xDFFF).contains(&lo) {
                        let cp = 0x10000 + ((u as u32 - 0xD800) << 10) + (lo as u32 - 0xDC00);
                        out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                    } else {
                        out.push('\u{FFFD}');
                    }
                } else {
                    out.push('\u{FFFD}');
                }
            }
        }
    }
    out
}

/// Percent-encode bytes so arbitrary paths are safe on-disk cache keys.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify the `$I` parser against the *real* recycle bin on this machine,
    /// so the offsets stay correct as Windows formats evolve.
    #[test]
    fn recycle_parse_real_files() {
        let Some(sid) = get_user_sid() else {
            eprintln!("skip: no SID (non-Windows or whoami unavailable)");
            return;
        };
        let rb = PathBuf::from("C:\\$Recycle.Bin").join(&sid);
        let mut parsed = 0usize;
        let mut errors = 0usize;
        if let Ok(rd) = std::fs::read_dir(&rb) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().into_owned();
                if let Some(suf) = n.strip_prefix("$I") {
                    match std::fs::read(e.path()).ok().and_then(|b| parse_recycle_i(&b, suf)) {
                        Some(it) if !it.name.is_empty() => {
                            println!("  parsed: {}  ({} bytes)", it.name, it.size);
                            parsed += 1;
                        }
                        _ => errors += 1,
                    }
                }
            }
        }
        println!("recycle parse: {parsed} ok, {errors} dropped");
        assert!(parsed > 0, "expected to parse at least one real $I file");
    }
}

// ---------------------------------------------------------------------------
// Windows clipboard as a *file list* (CF_HDROP)
//
// Explorer, browsers and editors all exchange copied files through this
// format. Reading and writing it makes Ctrl+C / Ctrl+V work across programs:
// copy here, paste into Explorer — or copy in Explorer, paste here.
// ---------------------------------------------------------------------------

/// Put `paths` on the Windows clipboard as a file list. No-op on other
/// platforms (and silently no-op when another app holds the clipboard open).
pub fn set_clipboard_files(paths: &[PathBuf]) {
    #[cfg(target_os = "windows")]
    unsafe {
        #[repr(C)]
        struct Dropfiles {
            /// Offset of the file list from the start of this structure.
            p_files: u32,
            pt_x: i32,
            pt_y: i32,
            f_nc: i32,
            /// 1 = the paths are UTF-16 (`DragQueryFileW`).
            f_wide: i32,
        }

        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalAlloc(flags: u32, bytes: usize) -> *mut u8;
            fn GlobalFree(handle: *mut u8) -> *mut u8;
            fn GlobalLock(handle: *mut u8) -> *mut u8;
            fn GlobalUnlock(handle: *mut u8) -> i32;
        }
        #[link(name = "user32")]
        extern "system" {
            fn OpenClipboard(new_owner: *mut u8) -> i32;
            fn CloseClipboard() -> i32;
            fn EmptyClipboard() -> i32;
            fn SetClipboardData(format: u32, mem: *mut u8) -> *mut u8;
        }

        const CF_HDROP: u32 = 15;
        const CF_UNICODETEXT: u32 = 13;
        const GMEM_MOVEABLE: u32 = 0x0002;

        if paths.is_empty() {
            return;
        }
        use std::os::windows::ffi::OsStrExt;
        let mut wide: Vec<u16> = Vec::new();
        for p in paths {
            wide.extend(p.as_os_str().encode_wide());
            wide.push(0);
        }
        wide.push(0); // list terminator
        let hdr = std::mem::size_of::<Dropfiles>() as u32;
        let bytes = hdr as usize + wide.len() * 2;
        let h = GlobalAlloc(GMEM_MOVEABLE, bytes);
        if h.is_null() {
            return;
        }
        let lp = GlobalLock(h);
        if lp.is_null() {
            GlobalFree(h);
            return;
        }
        let df = Dropfiles {
            p_files: hdr,
            pt_x: 0,
            pt_y: 0,
            f_nc: 0,
            f_wide: 1,
        };
        std::ptr::copy_nonoverlapping(
            &df as *const Dropfiles as *const u8,
            lp,
            std::mem::size_of::<Dropfiles>(),
        );
        std::ptr::copy_nonoverlapping(wide.as_ptr() as *const u8, lp.add(hdr as usize), wide.len() * 2);
        GlobalUnlock(h);

        // Publish the same paths as plain text (CF_UNICODETEXT), as Explorer
        // does. Besides letting a text target accept the paths, this is what
        // makes egui-winit emit a Paste event for Ctrl+V — that integration
        // only reports a paste when the clipboard actually holds *text*.
        let mut text: Vec<u16> = Vec::new();
        for (i, p) in paths.iter().enumerate() {
            if i > 0 {
                text.extend("\r\n".encode_utf16());
            }
            text.extend(p.as_os_str().encode_wide());
        }
        text.push(0);
        let h_text = GlobalAlloc(GMEM_MOVEABLE, text.len() * 2);
        let mut text_ok = false;
        if !h_text.is_null() {
            let tp = GlobalLock(h_text);
            if tp.is_null() {
                GlobalFree(h_text);
            } else {
                std::ptr::copy_nonoverlapping(text.as_ptr() as *const u8, tp, text.len() * 2);
                GlobalUnlock(h_text);
                text_ok = true;
            }
        }

        if OpenClipboard(std::ptr::null_mut()) == 0 {
            GlobalFree(h);
            if text_ok {
                GlobalFree(h_text);
            }
            return;
        }
        EmptyClipboard();
        let got = SetClipboardData(CF_HDROP, h);
        let got_text = if text_ok {
            SetClipboardData(CF_UNICODETEXT, h_text)
        } else {
            std::ptr::null_mut()
        };
        let _ = CloseClipboard();
        // On failure the caller keeps ownership of the block.
        if got.is_null() {
            GlobalFree(h);
        }
        if text_ok && got_text.is_null() {
            GlobalFree(h_text);
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = paths;
    }
}

/// Files currently on the Windows clipboard, if any. Used by paste when the
/// in-app clipboard is empty, so a copy made in Explorer lands here.
pub fn get_clipboard_files() -> Option<Vec<PathBuf>> {
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn OpenClipboard(new_owner: *mut u8) -> i32;
            fn CloseClipboard() -> i32;
            fn IsClipboardFormatAvailable(format: u32) -> i32;
            fn GetClipboardData(format: u32) -> *mut u8;
            fn DragQueryFileW(drop: *mut u8, item: u32, file: *mut u16, chars: u32) -> u32;
        }

        const CF_HDROP: u32 = 15;
        const COUNT: u32 = 0xFFFF_FFFF;

        if IsClipboardFormatAvailable(CF_HDROP) == 0 {
            return None;
        }
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let h = GetClipboardData(CF_HDROP);
        if h.is_null() {
            let _ = CloseClipboard();
            return None;
        }
        let n = DragQueryFileW(h, COUNT, std::ptr::null_mut(), 0);
        let mut out: Vec<PathBuf> = Vec::new();
        for i in 0..n {
            let len = DragQueryFileW(h, i, std::ptr::null_mut(), 0);
            if len == 0 {
                continue;
            }
            let mut buf = vec![0u16; len as usize + 1];
            let got = DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32);
            buf.truncate(got as usize);
            out.push(PathBuf::from(String::from_utf16_lossy(&buf)));
        }
        let _ = CloseClipboard();
        if out.is_empty() {
            None
        } else {
            Some(out)
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

// ---------------------------------------------------------------------------
// Windows clipboard as an *image* (CF_DIB + registered "PNG")
//
// Two formats go out together: CF_DIB is what classic apps (Paint, Word,
// Explorer) read; the registered "PNG" format is what modern ones (browsers,
// PowerToys, chat apps) prefer because it keeps the alpha channel. Anything
// pasting an image will pick whichever it understands.
// ---------------------------------------------------------------------------

/// Copy the image at `path` to the clipboard as a bitmap. Returns `false`
/// when the file cannot be decoded or the clipboard is unavailable.
pub fn set_clipboard_image(path: &Path) -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        let loaded = (|| {
            let r = image::ImageReader::open(path).ok()?.with_guessed_format().ok()?;
            r.decode().ok()
        })();
        // Cap the long edge: a 100 MP scan would put a 400 MB DIB on the
        // clipboard, which no recipient wants.
        let img = match loaded {
            Some(img) if img.width() > 8192 || img.height() > 8192 => {
                img.thumbnail(8192, 8192)
            }
            Some(img) => img,
            None => return false,
        };
        let rgba = img.to_rgba8();
        let png = match crate::convert::encode_png(&rgba, 6) {
            Ok(b) => b,
            Err(_) => return false,
        };
        let dib = dib_bytes(&rgba);

        #[link(name = "kernel32")]
        extern "system" {
            fn GlobalAlloc(flags: u32, bytes: usize) -> *mut u8;
            fn GlobalFree(handle: *mut u8) -> *mut u8;
            fn GlobalLock(handle: *mut u8) -> *mut u8;
            fn GlobalUnlock(handle: *mut u8) -> i32;
        }
        #[link(name = "user32")]
        extern "system" {
            fn OpenClipboard(new_owner: *mut u8) -> i32;
            fn CloseClipboard() -> i32;
            fn EmptyClipboard() -> i32;
            fn SetClipboardData(format: u32, mem: *mut u8) -> *mut u8;
            fn RegisterClipboardFormatW(name: *const u16) -> u32;
        }
        const CF_DIB: u32 = 8;
        const GMEM_MOVEABLE: u32 = 0x0002;

        fn alloc_copy(bytes: &[u8]) -> *mut u8 {
            unsafe {
                let h = GlobalAlloc(GMEM_MOVEABLE, bytes.len());
                if h.is_null() {
                    return h;
                }
                let p = GlobalLock(h);
                if p.is_null() {
                    GlobalFree(h);
                    return p;
                }
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
                GlobalUnlock(h);
                h
            }
        }

        let h_dib = alloc_copy(&dib);
        let h_png = alloc_copy(&png);
        if h_dib.is_null() || h_png.is_null() {
            if !h_dib.is_null() {
                GlobalFree(h_dib);
            }
            if !h_png.is_null() {
                GlobalFree(h_png);
            }
            return false;
        }
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            GlobalFree(h_dib);
            GlobalFree(h_png);
            return false;
        }
        EmptyClipboard();
        let got_dib = !SetClipboardData(CF_DIB, h_dib).is_null();
        let png_fmt = RegisterClipboardFormatW(
            [80u16, 78, 71, 0] // "PNG\0"
                .as_ptr(),
        );
        let mut got_png = true;
        if png_fmt != 0 {
            got_png = !SetClipboardData(png_fmt, h_png).is_null();
        } else {
            // No PNG slot registered: the DIB alone still pastes everywhere.
            GlobalFree(h_png);
        }
        let _ = CloseClipboard();
        // On failure the caller still owns the block; on success the system
        // took it over and freeing would be a use-after-free.
        if !got_dib {
            GlobalFree(h_dib);
        }
        if !got_png {
            GlobalFree(h_png);
        }
        got_dib && got_png
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = path;
        false
    }
}

/// True when the Windows clipboard currently holds *any* image — either a
/// classic CF_DIB or a registered "PNG" format. Used so the Paste menu is not
/// greyed out when a screenshot / copied picture is on the clipboard (no file
/// list is).
pub fn clipboard_has_image() -> bool {
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn IsClipboardFormatAvailable(format: u32) -> i32;
            fn RegisterClipboardFormatW(name: *const u16) -> u32;
        }
        const CF_DIB: u32 = 8;
        if IsClipboardFormatAvailable(CF_DIB) != 0 {
            return true;
        }
        let png_fmt =
            RegisterClipboardFormatW([80u16, 78u16, 71u16, 0].as_ptr());
        png_fmt != 0 && IsClipboardFormatAvailable(png_fmt) != 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

/// Read an image off the Windows clipboard and return it decoded. Returns
/// `None` when there is no image, or the data cannot be parsed. Prefers the
/// registered "PNG" format (keeps alpha); falls back to CF_DIB.
pub fn get_clipboard_image() -> Option<image::RgbaImage> {
    #[cfg(target_os = "windows")]
    unsafe {
        #[link(name = "user32")]
        extern "system" {
            fn OpenClipboard(new_owner: *mut u8) -> i32;
            fn CloseClipboard() -> i32;
            fn IsClipboardFormatAvailable(format: u32) -> i32;
            fn GetClipboardData(format: u32) -> *mut u8;
            fn RegisterClipboardFormatW(name: *const u16) -> u32;
            fn GlobalLock(handle: *mut u8) -> *mut u8;
            fn GlobalUnlock(handle: *mut u8) -> i32;
            fn GlobalSize(handle: *mut u8) -> usize;
        }
        const CF_DIB: u32 = 8;
        let png_fmt = RegisterClipboardFormatW([80u16, 78u16, 71u16, 0].as_ptr());
        if OpenClipboard(std::ptr::null_mut()) == 0 {
            return None;
        }
        let res = (|| {
            if png_fmt != 0 && IsClipboardFormatAvailable(png_fmt) != 0 {
                let h = GetClipboardData(png_fmt);
                if !h.is_null() {
                    let p = GlobalLock(h);
                    if !p.is_null() {
                        let len = GlobalSize(h);
                        let slice = std::slice::from_raw_parts(p, len);
                        let img = image::load_from_memory(slice).ok().map(|i| i.to_rgba8());
                        GlobalUnlock(h);
                        if let Some(img) = img {
                            return Some(img);
                        }
                    }
                }
            }
            if IsClipboardFormatAvailable(CF_DIB) != 0 {
                let h = GetClipboardData(CF_DIB);
                if !h.is_null() {
                    let p = GlobalLock(h);
                    if !p.is_null() {
                        let len = GlobalSize(h);
                        let slice = std::slice::from_raw_parts(p, len);
                        let img = dib_to_rgba(slice);
                        GlobalUnlock(h);
                        if let Some(img) = img {
                            return Some(img);
                        }
                    }
                }
            }
            None
        })();
        let _ = CloseClipboard();
        res
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Parse a Windows CF_DIB (BITMAPINFOHEADER + pixels) into an RGBA image.
/// Handles bottom-up (biHeight > 0) 24/32 bpp DIBs, which is what
/// [`set_clipboard_image`] produces.
#[cfg(target_os = "windows")]
fn dib_to_rgba(buf: &[u8]) -> Option<image::RgbaImage> {
    if buf.len() < 40 {
        return None;
    }
    let bi_size = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]);
    let width = i32::from_le_bytes([buf[4], buf[5], buf[6], buf[7]]);
    let height = i32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]);
    let bpp = u16::from_le_bytes([buf[14], buf[15]]);
    if bi_size < 40 || width <= 0 || height == 0 || (bpp != 32 && bpp != 24) {
        return None;
    }
    let (w, h) = (width as u32, height.unsigned_abs() as u32);
    let bottom_up = height > 0;
    let pixel_off = bi_size as usize;
    let stride = ((w as usize * bpp as usize + 31) / 32) * 4;
    if buf.len() < pixel_off + stride * h as usize {
        return None;
    }
    let mut img = image::RgbaImage::new(w, h);
    let bytes_pp = (bpp / 8) as usize;
    for y in 0..h {
        let src_y = if bottom_up { h - 1 - y } else { y };
        let row_start = pixel_off + src_y as usize * stride;
        for x in 0..w {
            let idx = row_start + x as usize * bytes_pp;
            let (r, g, b, a) = if bpp == 32 {
                (buf[idx + 2], buf[idx + 1], buf[idx], buf[idx + 3])
            } else {
                (buf[idx + 2], buf[idx + 1], buf[idx], 255)
            };
            img.put_pixel(x, y, image::Rgba([r, g, b, a]));
        }
    }
    Some(img)
}

/// Build a 32bpp bottom-up BGRA DIB (BITMAPINFOHEADER + pixels) from RGBA8.
#[cfg(target_os = "windows")]
fn dib_bytes(rgba: &image::RgbaImage) -> Vec<u8> {
    let w = rgba.width() as i32;
    let h = rgba.height() as i32;
    let mut out = Vec::with_capacity(40 + (w as usize * h as usize * 4));
    out.extend_from_slice(&40u32.to_le_bytes()); // biSize
    out.extend_from_slice(&w.to_le_bytes()); // biWidth
    out.extend_from_slice(&h.to_le_bytes()); // biHeight (>0 = bottom-up)
    out.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    out.extend_from_slice(&32u16.to_le_bytes()); // biBitCount
    out.extend_from_slice(&0u32.to_le_bytes()); // biCompression = BI_RGB
    out.extend_from_slice(&((w as usize * h as usize * 4) as u32).to_le_bytes()); // biSizeImage
    out.extend_from_slice(&0i32.to_le_bytes()); // biXPelsPerMeter
    out.extend_from_slice(&0i32.to_le_bytes()); // biYPelsPerMeter
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrUsed
    out.extend_from_slice(&0u32.to_le_bytes()); // biClrImportant
    for y in (0..rgba.height()).rev() {
        for x in 0..rgba.width() {
            let p = rgba.get_pixel(x, y);
            out.push(p[2]); // B
            out.push(p[1]); // G
            out.push(p[0]); // R
            out.push(p[3]); // A
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Context-menu registration no longer lives here — see `src/shell.rs`. The
// app writes `HKCU\Software\Classes` itself now, triggered by the Settings
// toggle. That key is per-user, so it needs no elevation, touches nobody
// else's account, and is fully reversible from the same toggle.
// ---------------------------------------------------------------------------

/// Directory that holds the running executable — also where the portable app
/// keeps its own store (see `cache::cache_dir`).
pub fn exe_dir() -> Option<std::path::PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}
