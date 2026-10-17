//! Embeds the icon, version information and application manifest into RochGPU.exe, so Explorer
//! shows the logo and the process gets per-monitor DPI awareness and modern common controls.

fn main() {
    println!("cargo:rerun-if-changed=assets/RochGPU.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/RochGPU.ico")
        .set_manifest_file("assets/app.manifest")
        .set("ProductName", "Roch GPU")
        .set("FileDescription", "Roch GPU")
        .set("LegalCopyright", "Copyright (c) 2026 Roch Studio. GPL-3.0-or-later.")
        .set("OriginalFilename", "RochGPU.exe");
    if let Err(e) = res.compile() {
        // A missing resource compiler should not block a build; the exe just has no icon.
        println!("cargo:warning=could not embed Windows resources: {e}");
    }
}
