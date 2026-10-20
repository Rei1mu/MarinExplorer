//! Embed `logo.ico` as a native Windows icon resource (IDI_ICON1).
//!
//! This gives the compiled `.exe` its file icon in Explorer and lets Windows
//! pick the right resolution (16/24/32/48/64/128/256) for the taskbar and
//! title bar, instead of scaling a single image.
//!
//! The icon is shipped as a *prebuilt* resource object that is committed to the
//! repo. It is produced from `logo.ico` + `icon.rc` with:
//!
//! ```text
//! x86_64-w64-mingw32-windres -o marin_icon.o icon.rc
//! ```
//!
//! (`icon.rc`: `IDI_ICON1 ICON DISCARDABLE "logo.ico"`)
//!
//! Regenerate the object (and commit it) whenever `logo.ico` changes. The
//! object is linked directly here so the build script performs no disk writes
//! — the build environment denies the build script filesystem access.
//!
//! Only the `x86_64-pc-windows-gnu` build gets this bundled file icon. The
//! ARM64 (MSVC) build links without it (no resource object is produced for
//! that target), which is fine — the executable still runs normally.
//!
//! On non-Windows or non-x86_64-GNU targets this script is a no-op.

use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    if target != "x86_64-pc-windows-gnu" {
        return;
    }

    let obj = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("marin_icon.o");
    println!("cargo:rustc-link-arg={}", obj.display());
    println!("cargo:rerun-if-changed={}", obj.display());
}
