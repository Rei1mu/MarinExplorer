//! Embed `logo.ico` as a native Windows icon resource (IDI_ICON1), and stamp
//! the build's version into the `.exe` (VERSIONINFO).
//!
//! Icon: gives the compiled `.exe` its file icon in Explorer and lets Windows
//! pick the right resolution (16/24/32/48/64/128/256) for the taskbar and
//! title bar.
//!
//! Version: without a VERSIONINFO resource the exe's Properties → Details
//! shows an empty version, so "which build am I running?" can only be answered
//! by file date or by the installer's own page. The version comes from
//! `CARGO_PKG_VERSION` (i.e. `Cargo.toml`), so it can never drift from the
//! crate version.
//!
//! The resource is generated *at build time* from `icon.rc` + `logo.ico` +
//! a generated `version.rc`, so nothing platform-specific is committed (the
//! repo's `.gitignore` excludes `*.o`). `icon.rc` is
//! `IDI_ICON1 ICON DISCARDABLE "logo.ico"`.
//!
//! Two resource compilers, one per toolchain:
//!   - `windres` (MinGW-w64)  -> `x86_64-pc-windows-gnu`,      emits a `.o`
//!   - `rc.exe`  (Windows SDK) -> the `*-pc-windows-msvc` targets, emits a `.res`
//!
//! Both choke on paths containing spaces, so `icon.rc` and `logo.ico` are
//! staged into a temp directory and compiled there (that path has no spaces).
//! If neither compiler is available the build still succeeds — it just ships
//! without an embedded file icon / version stamp.
//!
//! Covered targets: `x86_64-pc-windows-gnu`, `x86_64-pc-windows-msvc` and
//! `aarch64-pc-windows-msvc`. Anything else returns early and is untouched.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
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
    // The version resource is generated, not committed: it must always match
    // the crate version.
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_string());
    let tmp_ver = work.join("version.rc");
    if fs::write(&tmp_ver, version_rc(&version)).is_err() {
        return;
    }
    // NOTE: do not add an RT_MANIFEST resource here. rustc already embeds its
    // own manifest (id 1) — `asInvoker` + `longPathAware` — and a second one
    // with the same id loses to it, silently. Elevation is handled at startup
    // instead (see `shell::relaunch_elevated`).

    let mut link_args: Vec<PathBuf> = Vec::new();

    // Concatenate the icon script and the version script into ONE .rc.
    // windres takes a single input, and the MSVC linker does not merge the
    // resource *directories* of two separate GNU resource objects — the
    // version data lands in .rsrc but stays unreachable, so the exe ends up
    // with no version at all (and no visible symptom to explain it).
    let icon_text = fs::read_to_string(&tmp_rc).unwrap_or_default();
    let ver_text = fs::read_to_string(&tmp_ver).unwrap_or_default();
    let all_rc = work.join("all.rc");
    if fs::write(&all_rc, format!("{icon_text}\n{ver_text}")).is_err() {
        return;
    }

    if gnu {
        // MinGW: windres -> COFF object, handed straight to the linker.
        let out = work.join("marin_res.o");
        if windres_into(&out, &all_rc) && out.exists() {
            link_args.push(out);
        }
    } else {
        // MSVC: rc.exe (Windows SDK) -> .res, which link.exe consumes directly.
        // rc.exe compiles several .rc files into one .res, so the icon and the
        // version end up in a single object.
        let mut compiled = false;
        if let Some(rcexe) = find_rc() {
            let res = work.join("marin_icon.res");
            let ok = Command::new(&rcexe)
                .current_dir(&work)
                .args(["/nologo", "/fo"])
                .arg(&res)
                .arg("icon.rc")
                .arg("version.rc")
                .status()
                .map(|s| s.success())
                .unwrap_or(false);
            if ok && res.exists() {
                link_args.push(res);
                compiled = true;
            }
        }
        if !compiled {
            // No Windows SDK on this machine — and its `rc.exe` is the only
            // resource compiler that is guaranteed to produce an object the
            // MSVC linker takes. MinGW's `windres` emits a plain COFF object,
            // which link.exe does accept, so use it as the fallback: without
            // this the exe would ship with neither a file icon nor a version
            // stamp (and the build would silently look "fine").
            let out = work.join("marin_res.o");
            if windres_into(&out, &all_rc) && out.exists() {
                link_args.push(out);
            }
        }
    }

    for out in &link_args {
        println!("cargo:rustc-link-arg={}", out.display());
    }
    println!("cargo:rerun-if-changed={}", rc.display());
    println!("cargo:rerun-if-changed={}", dir.join("logo.ico").display());
    // The version lives in Cargo.toml — re-run the script when it changes.
    println!("cargo:rerun-if-changed={}", dir.join("Cargo.toml").display());
}

/// Compile one `.rc` into a COFF object with MinGW's `windres`. Returns
/// whether it worked. The compiler prints its own diagnostics.
fn windres_into(out: &PathBuf, src: &PathBuf) -> bool {
    [
        "x86_64-w64-mingw32-windres",
        "x86_64-w64-mingw32-windres.exe",
        "llvm-windres",
        "windres",
    ]
    .iter()
    .any(|tool| {
        Command::new(tool)
            // Run next to the .rc so relative includes (the .ico) resolve.
            .current_dir(src.parent().unwrap_or(Path::new(".")))
            .arg("-i")
            .arg(src)
            .arg("-o")
            .arg(out)
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

/// `0.1.17` -> `[0, 1, 17, 0]`. Non-numeric parts become 0; missing parts 0.
fn version_numbers(version: &str) -> [u16; 4] {
    let mut out = [0u16; 4];
    for (i, part) in version.split('.').take(4).enumerate() {
        // Strip any pre-release suffix ("1.2.3-beta" -> "3").
        let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
        out[i] = digits.parse::<u16>().unwrap_or(0);
    }
    out
}

/// The VERSIONINFO script. `080404b0` = language 0x0804 (Chinese, Simplified) and
/// charset 1200 (UTF-16), matching the installer, which is Chinese-first.
fn version_rc(version: &str) -> String {
    let [a, b, c, d] = version_numbers(version);
    format!(
        r#"1 VERSIONINFO
FILEVERSION {a},{b},{c},{d}
PRODUCTVERSION {a},{b},{c},{d}
FILEFLAGSMASK 0x3fL
FILEFLAGS 0x0L
FILEOS 0x40004L
FILETYPE 0x1L
FILESUBTYPE 0x0L
BEGIN
    BLOCK "StringFileInfo"
    BEGIN
        BLOCK "080404b0"
        BEGIN
            VALUE "CompanyName", "Rei1mu"
            VALUE "FileDescription", "MarinExplorer"
            VALUE "FileVersion", "{version}"
            VALUE "InternalName", "MarinExplorer"
            VALUE "OriginalFilename", "MarinExplorer.exe"
            VALUE "ProductName", "MarinExplorer"
            VALUE "ProductVersion", "{version}"
        END
    END
    BLOCK "VarFileInfo"
    BEGIN
        VALUE "Translation", 0x0804, 1200
    END
END
"#
    )
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
