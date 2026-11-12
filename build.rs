//! Embeds Windows version information and an application manifest in
//! `wowaddons.exe`.
//!
//! An executable with no version resource and no manifest scores worse with
//! antivirus heuristics (Windows Defender machine-learning detections). The
//! product name and version set here are also what the SignPath artifact
//! configuration checks before signing (`.signpath/artifact-configuration.xml`).

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=windows/wowaddons.manifest");

    // Only when building for Windows; `build.rs` also runs for Linux targets.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let version = env!("CARGO_PKG_VERSION");
    let mut res = winresource::WindowsResource::new();
    res.set("ProductName", "WoW Addons Manager")
        .set("FileDescription", "WoW Addons Manager")
        .set("ProductVersion", version)
        .set("FileVersion", version)
        .set("InternalName", "wowaddons")
        .set("OriginalFilename", "wowaddons.exe")
        .set("CompanyName", "lucaspouchot")
        .set(
            "LegalCopyright",
            "Copyright (C) 2026 lucaspouchot. Licensed under the GNU GPL v2.",
        )
        .set(
            "Comments",
            "https://github.com/lucaspouchot/wow-addons-manager",
        )
        .set_manifest_file("windows/wowaddons.manifest");

    if let Err(err) = res.compile() {
        panic!("failed to embed Windows resources: {err}");
    }
}
