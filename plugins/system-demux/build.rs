fn main() {
    println!("cargo:rerun-if-changed=src/apple.m");
    println!("cargo:rerun-if-changed=src/bridge.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/apple.m")
            .flag("-fobjc-arc")
            .flag("-Wno-deprecated-declarations")
            .compile("remdia_system_demux");
        for framework in [
            "AVFoundation",
            "AudioToolbox",
            "CoreMedia",
            "CoreFoundation",
            "Foundation",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
