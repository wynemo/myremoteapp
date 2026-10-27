fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rerun-if-changed=native/preview_macos.m");
        cc::Build::new()
            .file("native/preview_macos.m")
            .flag("-fobjc-arc")
            .compile("remote_preview_macos");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
        // ScreenCaptureKit's bridge uses the Swift concurrency runtime shipped
        // with macOS. Resolve it without requiring Xcode on the user's machine.
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
}
