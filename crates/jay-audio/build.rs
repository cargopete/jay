fn main() {
    if !cfg!(target_os = "macos") {
        return;
    }

    println!("cargo:rerun-if-changed=macos/system_tap.m");
    println!("cargo:rerun-if-changed=macos/voice_mic.m");

    cc::Build::new()
        .file("macos/system_tap.m")
        .file("macos/voice_mic.m")
        .flag("-fobjc-arc")
        // The tap API arrived in 14.4. The runtime check in the shim handles
        // older systems; this keeps the compiler from warning about it.
        .flag("-mmacosx-version-min=14.4")
        .compile("jay_system_tap");

    for framework in ["Foundation", "CoreAudio", "AudioToolbox", "CoreGraphics", "ImageIO"] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
