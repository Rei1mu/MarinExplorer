//! Embed `logo.ico` as a native Windows icon resource (IDI_ICON1).
//!
//! This gives the compiled `.exe` its file icon in Explorer and lets Windows
//! pick the right resolution (16/24/32/48/64/128/256) for the taskbar and
//! title bar, instead of scaling a single image.
//!
//! The icon is shipped as a *prebuilt* resource object, `marin_icon.o`, which
//! is committed to the repo. It is produced from `logo.ico` with:
//!
//! ```text
//! windres -o marin_icon.o icon.rc      # icon.rc: IDI_ICON1 ICON DISCARDABLE "logo.ico"
//! ```
//!
//! Regenerate `marin_icon.o` (and commit it) whenever `logo.ico` changes. The
//! object is linked directly here so the build script performs no disk writes —
//! the build environment denies the build script filesystem access.
//!
//! On non-GNU-Windows targets this script is a no-op.

use std::env;
use std::path::PathBuf;

fn main() {
    let target = env::var("TARGET").unwrap_or_default();
    let is_gnu_windows = target.contains("windows") && target.contains("gnu");
    if !is_gnu_windows {
        return;
    }

    let obj = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("marin_icon.o");
    println!("cargo:rustc-link-arg={}", obj.display());
    println!("cargo:rerun-if-changed={}", obj.display());
}
