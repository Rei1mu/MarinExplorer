//! Embed `logo.ico` as a native Windows icon resource (IDI_ICON1).
//!
//! This gives the compiled `.exe` its file icon in Explorer and lets Windows
//! pick the right resolution (16/24/32/48/64/128/256) for the taskbar and
//! title bar.
//!
//! The resource is generated *at build time* from `icon.rc` + `logo.ico`, so
//! nothing platform-specific is committed (the repo's `.gitignore` excludes
//! `*.o`). `icon.rc` is `IDI_ICON1 ICON DISCARDABLE "logo.ico"`.
//!
//! Two resource compilers, one per toolchain:
//!   - `windres` (MinGW-w64)  -> `x86_64-pc-windows-gnu`,      emits a `.o`
//!   - `rc.exe`  (Windows SDK) -> the `*-pc-windows-msvc` targets, emits a `.res`
//!
//! Both choke on paths containing spaces, so `icon.rc` and `logo.ico` are
//! staged into a temp directory and compiled there (that path has no spaces).
//! If neither compiler is available the build still succeeds — it just ships
//! without an embedded file icon.
//!
//! Covered targets: `x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc` and
//! `aarch64-pc-windows-msvc`. Anything else returns early and is untouched.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    let gnu = target == "x86_64-pc-windows-gnu";
    let msvc = target == "x86_64-pc-windows-msvc" || target == "aarch64-pc-windows-msvc";
    if !(gnu || msvc) {
        return;
    }

    let dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let rc = dir.join("icon.rc");
    if !rc.exists() {
        // Nothing to embed; let the build proceed without a file icon.
        return;
    }

    // Stage inputs in a space-free temp directory, then compile there.
    let work = std::env::temp_dir().join("marin_icon_build");
    if fs::create_dir_all(&work).is_err() {
        return;
    }
    let tmp_rc = work.join("icon.rc");
    let tmp_ico = work.join("logo.ico");
    if fs::copy(&rc, &tmp_rc).is_err() || fs::copy(dir.join("logo.ico"), &tmp_ico).is_err() {
        return;
    }
    // NOTE: do not add an RT_MANIFEST resource here. rustc already embeds its
    // own manifest (id 1) — `asInvoker` + `longPathAware` — and a second one
    // with the same id loses to it, silently. Elevation is handled at startup
    // instead (see `shell::relaunch_elevated`).

    let out = if gnu {
        // MinGW: windres -> COFF object, handed straight to the linker.
        let obj = work.join("marin_icon.o");
        let ok = [
            "x86_64-w64-mingw32-windres",
            "x86_64-w64-mingw32-windres.exe",
            "llvm-windres",
            "windres",
        ]
        .iter()
        .any(|tool| {
            Command::new(tool)
                .arg("-o")
                .arg(&obj)
                .arg(&tmp_rc)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        });
        if ok && obj.exists() {
            Some(obj)
        } else {
            None
        }
    } else {
        // MSVC: rc.exe (Windows SDK) -> .res, which link.exe consumes directly.
        match find_rc() {
            Some(rcexe) => {
                let res = work.join("marin_icon.res");
                let ok = Command::new(&rcexe)
                    .current_dir(&work)
                    .args(["/nologo", "/fo"])
                    .arg(&res)
                    .arg("icon.rc")
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false);
                if ok && res.exists() {
                    Some(res)
                } else {
                    None
                }
            }
            None => None,
        }
    };

    if let Some(out) = out {
        println!("cargo:rustc-link-arg={}", out.display());
        println!("cargo:rerun-if-changed={}", rc.display());
        println!("cargo:rerun-if-changed={}", dir.join("logo.ico").display());
    }
}

/// Locate `rc.exe` from the Windows SDK (newest 10.x kit wins).
fn find_rc() -> Option<PathBuf> {
    if let Ok(p) = env::var("WindowsSdkDir") {
        let direct = PathBuf::from(&p).join("bin").join("x64").join("rc.exe");
        if direct.exists() {
            return Some(direct);
        }
    }

    let kits = PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10\bin");
    let mut best: Option<(String, PathBuf)> = None;
    if let Ok(entries) = fs::read_dir(&kits) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("10.") {
                continue;
            }
            for sub in ["x64", "x86"] {
                let cand = entry.path().join(sub).join("rc.exe");
                if cand.exists() && best.as_ref().map_or(true, |(v, _)| name > *v) {
                    best = Some((name.clone(), cand));
                    break;
                }
            }
        }
    }
    best.map(|(_, p)| p)
}
