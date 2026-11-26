fn main() {
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo:rerun-if-changed=native/macos_display.m");
            println!("cargo:rerun-if-changed=native/macos_cursor.m");
            println!("cargo:rerun-if-changed=native/macos_audio.m");
            println!("cargo:rerun-if-changed=native/macos_gamepad.m");
            println!("cargo:rerun-if-changed=native/macos_privacy.m");
            cc::Build::new()
                .flag("-fobjc-arc")
                .file("native/macos_display.m")
                .file("native/macos_cursor.m")
                .file("native/macos_audio.m")
                .file("native/macos_gamepad.m")
                .file("native/macos_privacy.m")
                .compile("remoters_macos_display");
            println!("cargo:rustc-link-lib=framework=Foundation");
            println!("cargo:rustc-link-lib=framework=CoreGraphics");
            println!("cargo:rustc-link-lib=framework=AppKit");
            println!("cargo:rustc-link-lib=framework=AudioToolbox");
            println!("cargo:rustc-link-lib=framework=CoreMedia");
            println!("cargo:rustc-link-lib=framework=GameController");
            // ScreenCaptureKit uses Swift concurrency from the macOS runtime.
            println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        }
        Ok("windows") => {
            println!("cargo:rerun-if-changed=native/windows_display.cpp");
            println!("cargo:rerun-if-changed=native/windows_audio.cpp");
            println!("cargo:rerun-if-changed=native/windows_gamepad.cpp");
            cc::Build::new()
                .cpp(true)
                .std("c++17")
                .flag("/EHsc")
                .file("native/windows_display.cpp")
                .file("native/windows_audio.cpp")
                .file("native/windows_gamepad.cpp")
                .compile("remoters_windows_display");
            for library in ["ole32", "user32", "gdi32", "setupapi"] {
                println!("cargo:rustc-link-lib={library}");
            }
        }
        Ok("linux") => {
            println!("cargo:rerun-if-changed=native/linux_gamepad.c");
            println!("cargo:rerun-if-changed=native/linux_pipewire.c");
            cc::Build::new()
                .file("native/linux_gamepad.c")
                .compile("remoters_linux_gamepad");

            let mut pw_build = cc::Build::new();
            pw_build.file("native/linux_pipewire.c");
            let cflags = std::process::Command::new("pkg-config")
                .args(["--cflags", "libpipewire-0.3"])
                .output();
            if let Ok(output) = cflags
                && output.status.success()
            {
                for flag in String::from_utf8_lossy(&output.stdout).split_whitespace() {
                    pw_build.flag(flag);
                }
            } else {
                pw_build.include("/usr/include/pipewire-0.3");
                pw_build.include("/usr/include/spa-0.2");
            }
            pw_build.compile("remoters_linux_pipewire");
            println!("cargo:rustc-link-lib=pipewire-0.3");
        }
        _ => {}
    }
}
