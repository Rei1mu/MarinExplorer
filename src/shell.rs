//! Windows shell integration — the "Open with MarinExplorer" context menu.
//!
//! Every key lives under `HKCU\Software\Classes`, the **per-user** hive:
//!
//!   * no administrator rights, no UAC prompt, no elevation
//!   * changes affect only the current Windows account, never the machine
//!
//! We deliberately do **not** touch `HKLM` / `HKCR` (machine-wide), which is
//! what would require elevation and a signed installer.
//!
//! The label is written at install time in the user's current UI language, so
//! switching language in Settings simply rewrites the same keys.

#[cfg(target_os = "windows")]
mod win {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    const HKEY_CURRENT_USER: usize = 0x8000_0001;
    const KEY_READ: u32 = 0x2001_9;
    const KEY_WRITE: u32 = 0x2_0006;
    const REG_SZ: u32 = 1;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegCreateKeyExW(
            hkey: usize,
            subkey: *const u16,
            reserved: u32,
            class: *const u16,
            options: u32,
            sam_desired: u32,
            security: *const u8,
            result: *mut usize,
            disposition: *mut u32,
        ) -> i32;
        fn RegOpenKeyExW(
            hkey: usize,
            subkey: *const u16,
            options: u32,
            sam_desired: u32,
            result: *mut usize,
        ) -> i32;
        fn RegSetValueExW(
            hkey: usize,
            name: *const u16,
            reserved: u32,
            ty: u32,
            data: *const u8,
            cb_data: u32,
        ) -> i32;
        fn RegDeleteTreeW(hkey: usize, subkey: *const u16) -> i32;
        fn RegCloseKey(hkey: usize) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Write one `REG_SZ` value under `subkey` (creating the key if needed).
    fn set_sz(subkey: &str, name: &str, value: &str) -> Result<(), i32> {
        let sub = wide(subkey);
        let mut hkey: usize = 0;
        let mut _disp: u32 = 0;
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                sub.as_ptr(),
                0,
                std::ptr::null(),
                0,
                KEY_WRITE,
                std::ptr::null(),
                &mut hkey,
                &mut _disp,
            )
        };
        if rc != 0 {
            return Err(rc);
        }
        let name_w = wide(name);
        // REG_SZ: the byte count must include the terminating NUL.
        let val_w = wide(value);
        let bytes = val_w.len() * 2;
        let out = unsafe {
            RegSetValueExW(
                hkey,
                name_w.as_ptr(),
                0,
                REG_SZ,
                val_w.as_ptr() as *const u8,
                bytes as u32,
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        if out != 0 {
            Err(out)
        } else {
            Ok(())
        }
    }

    pub fn key_exists(subkey: &str) -> bool {
        let sub = wide(subkey);
        let mut hkey: usize = 0;
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, sub.as_ptr(), 0, KEY_READ, &mut hkey)
        };
        if rc != 0 {
            return false;
        }
        unsafe {
            RegCloseKey(hkey);
        }
        true
    }

    pub fn delete_tree(subkey: &str) -> Result<(), i32> {
        let sub = wide(subkey);
        let rc = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, sub.as_ptr()) };
        // Deleting something that is not there is success as far as the caller
        // is concerned — we only want a clean end state.
        match rc {
            0 => Ok(()),
            2 => Ok(()), // ERROR_FILE_NOT_FOUND
            e => Err(e),
        }
    }

    const SW_SHOWNORMAL: i32 = 1;

    #[link(name = "shell32")]
    extern "system" {
        fn IsUserAnAdmin() -> i32;
        fn ShellExecuteW(
            hwnd: *mut (),
            verb: *const u16,
            file: *const u16,
            params: *const u16,
            dir: *const u16,
            show: i32,
        ) -> isize;
    }

    pub fn is_elevated() -> bool {
        unsafe { IsUserAnAdmin() != 0 }
    }

    /// Relaunch with the `runas` verb. The `--elevated` marker tells the new
    /// instance that it is the result of an elevation attempt, so it must not
    /// ask again (otherwise a declined prompt would loop forever).
    pub fn relaunch_elevated() -> bool {
        relaunch_elevated_with(&[])
    }

    /// Relaunch with the `runas` verb plus `extra` flags. The flags travel
    /// with the command line (only the `--elevated` marker itself is
    /// filtered out), so the elevated copy knows what it was spawned for —
    /// `--take-over`, meaning "replace a running instance".
    ///
    /// The `--elevated` marker tells the new instance that it is the result
    /// of an elevation attempt, so it must not ask again (otherwise a
    /// declined prompt would loop forever).
    pub fn relaunch_elevated_with(extra: &[&str]) -> bool {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        let verb = wide("runas");
        let file = wide(&exe.to_string_lossy());
        // Keep whatever the shell passed (e.g. the folder behind a right-click
        // "open here"), so the elevated copy opens the same place.
        let mut line = String::from("--elevated");
        for f in extra {
            line.push(' ');
            line.push_str(f);
        }
        for a in std::env::args().skip(1).filter(|a| a != "--elevated") {
            line.push(' ');
            if a.contains(' ') {
                line.push('"');
                line.push_str(&a);
                line.push('"');
            } else {
                line.push_str(&a);
            }
        }
        let params = wide(&line);
        // > 32 means the shell launched something; anything else is an error
        // (1223 = the user cancelled the UAC prompt).
        let rc = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                verb.as_ptr(),
                file.as_ptr(),
                params.as_ptr(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        rc > 32
    }

    const SHERB_NOCONFIRMATION: u32 = 0x0000_0001;
    const SHERB_NOPROGRESSUI: u32 = 0x0000_0002;
    const SHERB_NOSOUND: u32 = 0x0000_0004;

    #[link(name = "shell32")]
    extern "system" {
        fn SHEmptyRecycleBinW(hwnd: *mut (), root: *const u16, flags: u32) -> i32;
    }

    /// `SHEmptyRecycleBin`, scoped to `root` (a drive like `C:\`) when given.
    ///
    /// Passing NULL asks for *every* drive at once, which fails wholesale if
    /// any of them is unreachable — an ejected card, an offline network drive,
    /// a BitLocker-locked volume — even though the bin we actually care about
    /// would have emptied fine. So callers pass the drive they are looking at.
    ///
    /// `SHERB_NOPROGRESSUI` *is* set: the shell's own modal progress dialog is
    /// suppressed. For a bin with tens of thousands of items that dialog can
    /// stall and is outside our control, so we never show it. Instead the
    /// caller runs this on a background thread and renders its *own*
    /// indeterminate progress in-app (see `Explorer::spawn_empty_recycle`),
    /// which keeps the UI responsive and shows activity without a system modal.
    /// Confirmation and sound stay suppressed.
    ///
    /// Returns false on any failure, so the caller can fall back to deleting
    /// the `$I` / `$R` pairs itself.
    pub fn empty_recycle_bin(root: Option<&str>) -> bool {
        let flags = SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND;
        let root_wide = root.map(wide);
        let root_ptr = root_wide
            .as_ref()
            .map(|v| v.as_ptr())
            .unwrap_or(std::ptr::null());
        let hr = unsafe { SHEmptyRecycleBinW(std::ptr::null_mut(), root_ptr, flags) };
        // HRESULT: negative means failure. S_OK also covers "already empty".
        hr >= 0
    }

    pub fn write_menu(exe: &str, label: &str) -> Result<(), i32> {
        for t in super::TARGETS {
            let base = format!("Software\\Classes\\{}\\MarinExplorer", t.key);
            // The menu text itself.
            set_sz(&base, "", label)?;
            // Show our own app icon next to the entry.
            set_sz(&base, "Icon", &format!("\"{}\",0", exe))?;
            // `%V` inside a folder's empty area, `%1` everywhere else.
            set_sz(
                &format!("{}\\command", base),
                "",
                &format!("\"{}\" {}", exe, t.arg),
            )?;
        }
        // Also register as a known application so it shows up under
        // "打开方式 / Open with → 选择其他应用".
        let app = format!("Software\\Classes\\Applications\\MarinExplorer.exe");
        set_sz(&app, "FriendlyAppName", "MarinExplorer")?;
        set_sz(
            &format!("{}\\shell\\open\\command", app),
            "",
            &format!("\"{}\" \"%1\"", exe),
        )?;
        Ok(())
    }

    pub fn remove_menu() -> Result<(), i32> {
        for t in super::TARGETS {
            delete_tree(&format!("Software\\Classes\\{}\\MarinExplorer", t.key))?;
        }
        delete_tree("Software\\Classes\\Applications\\MarinExplorer.exe")
    }
}

/// Where the verb is registered, and which placeholder Windows hands us:
/// `%1` = the clicked item, `%V` = the current folder (background click).
#[cfg(target_os = "windows")]
struct Target {
    key: &'static str,
    arg: &'static str,
}

#[cfg(target_os = "windows")]
const TARGETS: &[Target] = &[
    Target {
        key: "*\\shell",
        arg: "\"%1\"",
    }, // any file
    Target {
        key: "Directory\\shell",
        arg: "\"%1\"",
    }, // a folder
    Target {
        key: "Directory\\Background\\shell",
        arg: "\"%V\"",
    }, // empty area of a folder
    Target {
        key: "Drive\\shell",
        arg: "\"%1\"",
    }, // a disk
];

/// Register the context menu for the running executable.
///
/// `label` is the text Windows shows; pass the localised string so Chinese
/// users see 通过 MarinExplorer 打开 and English users see the English text.
#[cfg(target_os = "windows")]
pub fn install(label: &str) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .to_string_lossy()
        .to_string();
    win::write_menu(&exe, label).map_err(|rc| format!("RegError({})", rc))
}

#[cfg(not(target_os = "windows"))]
pub fn install(_label: &str) -> Result<(), String> {
    Err("context menu is Windows-only".into())
}

/// Remove every key we created.
#[cfg(target_os = "windows")]
pub fn uninstall() -> Result<(), String> {
    win::remove_menu().map_err(|rc| format!("RegError({})", rc))
}

#[cfg(not(target_os = "windows"))]
pub fn uninstall() -> Result<(), String> {
    Err("context menu is Windows-only".into())
}

/// True when this process already runs elevated.
///
/// `IsUserAnAdmin` is the simple, documented check: it is true for an
/// elevated member of the Administrators group, false both for a standard
/// user and for an admin whose process was not elevated (split token).
#[cfg(target_os = "windows")]
pub fn is_elevated() -> bool {
    win::is_elevated()
}

#[cfg(not(target_os = "windows"))]
pub fn is_elevated() -> bool {
    true // nothing to elevate on other platforms
}

/// Restart ourselves with `runas`, i.e. one UAC prompt and a process that
/// stays elevated for the whole session.
///
/// Why not a `requireAdministrator` manifest: rustc already embeds its own
/// manifest (resource id 1, `asInvoker`), and a second manifest with the same
/// id silently loses to it — the app would start unelevated with no clue why.
/// Relaunching through `ShellExecute` works on both toolchains and fails
/// *soft*: if the user declines the prompt the caller simply keeps running
/// unelevated instead of dying in a relaunch loop.
///
/// Returns false when nothing was launched (declined, cancelled, or the
/// platform has no UAC).
#[cfg(target_os = "windows")]
pub fn relaunch_elevated() -> bool {
    win::relaunch_elevated()
}

#[cfg(not(target_os = "windows"))]
pub fn relaunch_elevated() -> bool {
    false
}

/// Like `relaunch_elevated`, but the elevated copy also carries `extra`
/// flags — `--take-over` makes it replace a running instance instead of
/// activating it.
#[cfg(target_os = "windows")]
pub fn relaunch_elevated_with(extra: &[&str]) -> bool {
    win::relaunch_elevated_with(extra)
}

#[cfg(not(target_os = "windows"))]
pub fn relaunch_elevated_with(_extra: &[&str]) -> bool {
    false
}

/// Is the verb currently registered?
#[cfg(target_os = "windows")]
pub fn is_installed() -> bool {
    win::key_exists("Software\\Classes\\*\\shell\\MarinExplorer")
}

#[cfg(not(target_os = "windows"))]
pub fn is_installed() -> bool {
    false
}

/// Empty the Recycle Bin **through the shell**.
///
/// Deleting the `$I` / `$R` pairs by hand looks equivalent but is not: the
/// shell is the only thing that reliably gets rid of read-only entries (a
/// plain `DeleteFile` refuses those), names that grew past `MAX_PATH`, and
/// items that came from another account or a protected folder. Those were
/// exactly the cases where "empty" appeared to do nothing.
///
/// `root` is the drive whose bin should be emptied (`"C:\"`); pass `None` for
/// every drive.
#[cfg(target_os = "windows")]
pub fn empty_recycle_bin(root: Option<&str>) -> bool {
    win::empty_recycle_bin(root)
}

#[cfg(not(target_os = "windows"))]
pub fn empty_recycle_bin(_root: Option<&str>) -> bool {
    false
}
