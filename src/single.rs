//! Single-instance guard + cross-instance path hand-off.
//!
//! A second launch must not open a second window: it finds the window the
//! first instance owns and brings it to the front instead. When the launch
//! carries a path (Explorer's "open with", a drag onto the exe, the command
//! line) the path is handed to the running instance, which opens it in a
//! new tab.
//!
//! Why the hand-off is a **file drop** and not `WM_COPYDATA`: the running
//! instance is elevated (it relaunches itself with `runas`), the launcher
//! is not, and UIPI blocks messages sent from a medium to a high integrity
//! window. A file works in that direction — a medium process may write it,
//! and the high process may read it.
//!
//! Why the window lookup is by title + image name and not a named mutex: a
//! mutex created by the elevated instance cannot be opened by the
//! unelevated launcher (same integrity rules), while `EnumWindows`,
//! `ShowWindow` and `SetForegroundWindow` are not UIPI-blocked.

use std::path::PathBuf;

/// The running instance's window, as found by [`find_existing`].
pub struct Found {
    /// Top-level window handle (title bar = "MarinExplorer").
    pub hwnd: isize,
    /// Process that owns the window.
    #[allow(dead_code)]
    pub pid: u32,
    /// That process' image path (…\MarinExplorer_new.exe in practice).
    #[allow(dead_code)]
    pub image: PathBuf,
}

// ---------------------------------------------------------------------------
// Platform-independent: the inbox
// ---------------------------------------------------------------------------

/// Where handed-off paths wait for the running instance to pick them up.
/// Next to the store, so it travels with the exe (same rules as the cache).
pub fn inbox_dir() -> PathBuf {
    crate::cache::cache_dir().join("inbox")
}

/// Leave `path` for the running instance. Best effort: a failure here only
/// means the window activates without navigating.
pub fn write_inbox(path: &str) {
    let dir = inbox_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // Timestamp first: a burst of launches is then applied in launch order.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!("{nanos:016x}-{}.txt", std::process::id());
    std::fs::write(dir.join(name), path).ok();
}

/// Take everything the inbox holds: read each file, delete it, return the
/// paths in write order. Deleting on read keeps the box empty even when no
/// instance ever runs again.
pub fn take_inbox() -> Vec<PathBuf> {
    let dir = inbox_dir();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x.eq_ignore_ascii_case("txt")))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(&f).ok();
        std::fs::remove_file(&f).ok();
        if let Some(t) = text {
            let t = t.trim();
            if !t.is_empty() {
                out.push(PathBuf::from(t));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Windows: find / activate / replace
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod win {
    use super::Found;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    const WM_CLOSE: u32 = 0x0010;
    const GW_OWNER: u32 = 4;
    const SW_SHOW: i32 = 5;
    const SW_RESTORE: i32 = 9;
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const PROCESS_TERMINATE: u32 = 0x0001;
    const SYNCHRONIZE: u32 = 0x0010_0000;

    #[link(name = "user32")]
    extern "system" {
        fn EnumWindows(cb: extern "system" fn(isize, isize) -> i32, lparam: isize) -> i32;
        fn IsWindow(hwnd: isize) -> i32;
        fn IsWindowVisible(hwnd: isize) -> i32;
        fn IsIconic(hwnd: isize) -> i32;
        fn GetWindowTextW(hwnd: isize, buf: *mut u16, max: i32) -> i32;
        fn GetWindow(hwnd: isize, cmd: u32) -> isize;
        fn GetWindowThreadProcessId(hwnd: isize, pid: *mut u32) -> u32;
        fn GetForegroundWindow() -> isize;
        fn GetCurrentThreadId() -> u32;
        fn ShowWindow(hwnd: isize, cmd: i32) -> i32;
        fn SetForegroundWindow(hwnd: isize) -> i32;
        fn BringWindowToTop(hwnd: isize) -> i32;
        fn AttachThreadInput(id_attach: u32, id_attach_to: u32, attach: i32) -> i32;
        fn PostMessageW(hwnd: isize, msg: u32, wparam: usize, lparam: isize) -> i32;
    }

    #[link(name = "kernel32")]
    extern "system" {
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> isize;
        fn CloseHandle(h: isize) -> i32;
        fn GetCurrentProcessId() -> u32;
        fn QueryFullProcessImageNameW(h: isize, flags: u32, buf: *mut u16, size: *mut u32) -> i32;
        fn GetProcessTimes(
            h: isize,
            creation: *mut u64,
            exit: *mut u64,
            kernel: *mut u64,
            user: *mut u64,
        ) -> i32;
        fn TerminateProcess(h: isize, code: u32) -> i32;
    }

    /// True when `image` is *a* MarinExplorer build: the deployed copy is
    /// `MarinExplorer_new.exe`, the cargo product is `MarinExplorer.exe`, and
    /// both count as us. The title check in [`scan`] keeps lookalikes out.
    fn is_our_image(image: &PathBuf) -> bool {
        image
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
            && image
                .file_stem()
                .is_some_and(|s| s.to_string_lossy().to_lowercase().starts_with("marinexplorer"))
    }

    /// A process' image path, when we may ask. `PROCESS_QUERY_LIMITED_
    /// INFORMATION` is readable across integrity levels, so this works on
    /// the elevated instance from the unelevated launcher.
    fn image_of(pid: u32) -> Option<PathBuf> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return None;
            }
            let mut buf = [0u16; 1024];
            let mut size = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut size);
            CloseHandle(h);
            if ok == 0 {
                return None;
            }
            Some(PathBuf::from(String::from_utf16_lossy(
                &buf[..size as usize],
            )))
        }
    }

    /// EnumWindows callback: collect top-level windows titled exactly
    /// "MarinExplorer" that own no parent window (the main window; not a
    /// dialog, not the tray's hidden message window). Hidden windows count
    /// too — the app can sit hidden behind its tray icon.
    extern "system" fn scan(hwnd: isize, lparam: isize) -> i32 {
        unsafe {
            let hits = &mut *(lparam as *mut Vec<(isize, u32)>);
            if GetWindow(hwnd, GW_OWNER) != 0 {
                return 1;
            }
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
            if n == 0 {
                return 1;
            }
            if String::from_utf16_lossy(&buf[..n as usize]) != "MarinExplorer" {
                return 1;
            }
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid != 0 {
                hits.push((hwnd, pid));
            }
            1
        }
    }

    /// The running instance's window, if there is one. Title **and** image
    /// name must match, so nothing that merely looks like us is mistaken
    /// for us; our own window is excluded.
    pub fn find_existing() -> Option<Found> {
        let my_pid = unsafe { GetCurrentProcessId() };
        let mut hits: Vec<(isize, u32)> = Vec::new();
        unsafe {
            EnumWindows(scan, &mut hits as *mut Vec<(isize, u32)> as isize);
        }
        for (hwnd, pid) in hits {
            if pid == my_pid {
                continue;
            }
            if let Some(image) = image_of(pid) {
                if is_our_image(&image) {
                    return Some(Found { hwnd, pid, image });
                }
            }
        }
        None
    }

    /// Is the found instance running the exact binary we are?
    ///
    /// A reload replaces the exe on disk, so "same build" means the process
    /// started *after* the file was last written. Different image paths (the
    /// cargo product vs the deployed copy) count as different builds on
    /// purpose: the newer launch then takes over instead of activating.
    pub fn same_build(found: &Found) -> bool {
        let Ok(me) = std::env::current_exe() else {
            return false;
        };
        if found.image != me {
            return false;
        }
        let Ok(written) = std::fs::metadata(&me).and_then(|m| m.modified()) else {
            return false;
        };
        match process_start(found.pid) {
            Some(started) => started >= written,
            None => false,
        }
    }

    /// When `pid` started, as a `SystemTime`.
    fn process_start(pid: u32) -> Option<std::time::SystemTime> {
        unsafe {
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h == 0 {
                return None;
            }
            let mut creation: u64 = 0;
            let mut exit: u64 = 0;
            let mut kernel: u64 = 0;
            let mut user: u64 = 0;
            let ok = GetProcessTimes(h, &mut creation, &mut exit, &mut kernel, &mut user);
            CloseHandle(h);
            if ok == 0 {
                return None;
            }
            // FILETIME: 100ns ticks since 1601-01-01.
            const TICKS_PER_SEC: u64 = 10_000_000;
            const EPOCH_DIFF_SECS: u64 = 11_644_473_600;
            let secs = creation / TICKS_PER_SEC;
            let nanos = (creation % TICKS_PER_SEC) * 100;
            let unix = secs.checked_sub(EPOCH_DIFF_SECS)?;
            Some(std::time::UNIX_EPOCH + Duration::new(unix, nanos as u32))
        }
    }

    /// Bring the running instance's window to the front.
    ///
    /// A just-launched process is normally the foreground process, so the
    /// plain `SetForegroundWindow` usually succeeds on its own; the input-
    /// queue attach is the fallback for when Windows' foreground lock says
    /// no. None of these calls are UIPI-blocked, so this works from the
    /// unelevated launcher against the elevated instance's window.
    pub fn activate(hwnd: isize) {
        unsafe {
            if IsWindowVisible(hwnd) == 0 {
                ShowWindow(hwnd, SW_SHOW); // hidden behind the tray icon
            }
            if IsIconic(hwnd) != 0 {
                ShowWindow(hwnd, SW_RESTORE);
            }
            SetForegroundWindow(hwnd);
            if GetForegroundWindow() == hwnd {
                return;
            }
            let cur = GetCurrentThreadId();
            let fg = GetForegroundWindow();
            let mut fg_pid = 0u32;
            let fg_tid = if fg != 0 {
                GetWindowThreadProcessId(fg, &mut fg_pid)
            } else {
                0
            };
            if fg_tid != 0 && fg_tid != cur && AttachThreadInput(fg_tid, cur, 1) != 0 {
                SetForegroundWindow(hwnd);
                BringWindowToTop(hwnd);
                AttachThreadInput(fg_tid, cur, 0);
            }
        }
    }

    /// Ask the running instance to go away, then make sure it did.
    ///
    /// Graceful first — `WM_CLOSE` lets the app flush its store on the way
    /// out. Terminate is the last resort, and only after the grace period:
    /// a window that ignores `WM_CLOSE` for three seconds is hung, and a
    /// hung instance must not block the replacement forever.
    pub fn take_over(found: &Found) {
        unsafe {
            let signaled = PostMessageW(found.hwnd, WM_CLOSE, 0, 0) != 0;
            if signaled && wait_gone(found.hwnd, Duration::from_secs(3)) {
                return;
            }
            let h = OpenProcess(PROCESS_TERMINATE | SYNCHRONIZE, 0, found.pid);
            if h != 0 {
                TerminateProcess(h, 1);
                CloseHandle(h);
            }
            wait_gone(found.hwnd, Duration::from_secs(1));
        }
    }

    /// Poll until the window is gone or the budget runs out.
    fn wait_gone(hwnd: isize, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            unsafe {
                if IsWindow(hwnd) == 0 {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::{activate, find_existing, same_build, take_over};

#[cfg(not(target_os = "windows"))]
pub fn find_existing() -> Option<Found> {
    None
}

#[cfg(not(target_os = "windows"))]
pub fn activate(_hwnd: isize) {}

#[cfg(not(target_os = "windows"))]
pub fn same_build(_found: &Found) -> bool {
    true
}

#[cfg(not(target_os = "windows"))]
pub fn take_over(_found: &Found) {}
