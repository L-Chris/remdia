fn main() {
    println!("cargo:rerun-if-changed=src/vt_bridge.c");
    println!("cargo:rerun-if-changed=src/vt_bridge.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("src/vt_bridge.c")
            .flag("-std=c11")
            .warnings(true)
            .flag("-Werror")
            .compile("remdia_vt_bridge");
        for framework in ["VideoToolbox", "CoreMedia", "CoreVideo", "CoreFoundation"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
    }
}
