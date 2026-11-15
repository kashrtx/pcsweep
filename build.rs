// Embeds the app icon and version details into PCSweep.exe, so Explorer, the taskbar
// and the file's Properties dialog show them. Windows release builds require
// resources; a cross-platform type check can proceed without a resource compiler.
fn main() {
    for key in ["GITHUB_RUN_NUMBER", "GITHUB_SHA"] {
        println!("cargo:rerun-if-env-changed={key}");
    }
    let revision = std::env::var("GITHUB_RUN_NUMBER").unwrap_or_else(|_| "0".into());
    let commit = std::env::var("GITHUB_SHA").unwrap_or_else(|_| "local".into());
    println!("cargo:rustc-env=PCSWEEP_BUILD_REVISION={revision}");
    println!("cargo:rustc-env=PCSWEEP_BUILD_COMMIT={commit}");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon("assets/icon.ico")
        .set("ProductName", "PCSweep")
        .set(
            "FileDescription",
            "PCSweep: safe cleanup and PC health for Windows",
        )
        .set("CompanyName", "Kaushal Bhingaradia")
        .set(
            "LegalCopyright",
            "Copyright 2026 Kaushal Bhingaradia. Apache License 2.0",
        );
    if let Err(e) = res.compile() {
        if std::env::var("TARGET").is_ok_and(|target| target.ends_with("windows-msvc")) {
            panic!("Could not embed Windows app resources: {e}");
        }
        println!("cargo:warning=Could not embed the app icon: {e}");
    }
}
