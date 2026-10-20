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

/// Is the verb currently registered?
#[cfg(target_os = "windows")]
pub fn is_installed() -> bool {
    win::key_exists("Software\\Classes\\*\\shell\\MarinExplorer")
}

#[cfg(not(target_os = "windows"))]
pub fn is_installed() -> bool {
    false
}
