fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rerun-if-changed=native/macos_display.m");
            cc::Build::new()
                .flag("-fobjc-arc")
                .file("native/macos_display.m")
                .compile("remoters_macos_display");
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
            // ScreenCaptureKit uses Swift concurrency from the macOS runtime.
            println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        }
        Ok("windows") => {
            println!("cargo:rerun-if-changed=native/windows_display.cpp");
            cc::Build::new()
                .cpp(true)
                .std("c++17")
                .flag("/EHsc")
                .file("native/windows_display.cpp")
                .compile("remoters_windows_display");
            for library in ["ole32", "user32", "setupapi"] {
                println!("cargo:rustc-link-lib={library}");
            }
        }
        _ => {}
    }
}
