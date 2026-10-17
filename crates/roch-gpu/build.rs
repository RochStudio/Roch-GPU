//! Embeds the icon, version information and application manifest into RochGPU.exe, so Explorer
//! shows the logo and the process gets per-monitor DPI awareness and modern common controls.

fn main() {
    println!("cargo:rerun-if-changed=assets/RochGPU.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Load opengl32.dll on first use instead of at process start. Only the window needs it, and
    // merely having it mapped costs every process ~55 ms on exit (its DLL_PROCESS_DETACH), which
    // was 90% of a CLI run such as `info --mock`. glutin loads it explicitly before any wgl call.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=RochGPU=/DELAYLOAD:opengl32.dll");
        println!("cargo:rustc-link-arg-bin=RochGPU=delayimp.lib");
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
