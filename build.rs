//! Embed `logo.ico` as a native Windows icon resource (IDI_ICON1).
//!
//! This gives the compiled `.exe` its file icon in Explorer and lets Windows
//! pick the right resolution (16/24/32/48/64/128/256) for the taskbar and
//! title bar.
//!
//! The resource object is generated *at build time* from `icon.rc` + `logo.ico`
//! with `windres`, so nothing platform-specific is committed (the repo's
//! `.gitignore` excludes `*.o`). `icon.rc` is `IDI_ICON1 ICON DISCARDABLE
//! "logo.ico"`.
//!
//! `windres` chokes on paths that contain spaces, so `icon.rc` and `logo.ico`
//! are staged in a temp directory, compiled there, and linked via the temp
//! path (which has no spaces). If `windres` is unavailable the build still
//! succeeds — it just ships without an embedded file icon.
//!
//! Only the `x86_64-pc-windows-gnu` build embeds the icon. Other targets
//! (including `aarch64-pc-windows-msvc`) are unaffected; the executable still
//! runs, it just uses the runtime window icon instead of a file icon.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    if target != "x86_64-pc-windows-gnu" {
        return;
    }

    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let rc = dir.join("icon.rc");
    if !rc.exists() {
        // Nothing to embed; let the build proceed without a file icon.
        return;
    }

    let work = std::env::temp_dir().join("marin_icon_build");
    let _ = fs::create_dir_all(&work);
    let tmp_rc = work.join("icon.rc");
    let tmp_ico = work.join("logo.ico");
    let tmp_obj = work.join("marin_icon.o");
    let _ = fs::copy(&rc, &tmp_rc);
    let _ = fs::copy(dir.join("logo.ico"), &tmp_ico);

    let mut made = false;
    for tool in [
        "x86_64-w64-mingw32-windres",
        "x86_64-w64-mingw32-windres.exe",
        "llvm-windres",
        "windres",
    ] {
        if Command::new(tool)
            .arg("-o")
            .arg(&tmp_obj)
            .arg(&tmp_rc)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            made = true;
            break;
        }
    }

    if made && tmp_obj.exists() {
        println!("cargo:rustc-link-arg={}", tmp_obj.display());
        println!("cargo:rerun-if-changed={}", rc.display());
        println!("cargo:rerun-if-changed={}", dir.join("logo.ico").display());
    }
}
