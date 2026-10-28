fn main() {
    for file in ["src/image_bridge.h", "src/apple.c", "src/windows.cpp"] {
        println!("cargo:rerun-if-changed={file}");
    }
    match std::env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            cc::Build::new()
                .file("src/apple.c")
                .flag("-std=c11")
                .warnings(true)
                .flag("-Werror")
                .compile("remdia_image");
            for framework in ["ImageIO", "CoreGraphics", "CoreFoundation"] {
                println!("cargo:rustc-link-lib=framework={framework}");
            }
        }
        Ok("windows") => {
            cc::Build::new()
                .cpp(true)
                .file("src/windows.cpp")
                .warnings(true)
                .compile("remdia_image");
            for library in ["windowscodecs", "ole32", "oleaut32", "uuid"] {
                println!("cargo:rustc-link-lib={library}");
            }
        }
        _ => {}
    }
}
