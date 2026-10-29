fn main() {
    for file in ["resample.c", "arch.h", "speex_resampler.h"] {
        println!("cargo:rerun-if-changed=vendor/{file}");
    }
    cc::Build::new()
        .file("vendor/resample.c")
        .include("vendor")
        .define("OUTSIDE_SPEEX", None)
        .define("FLOATING_POINT", None)
        .define("RANDOM_PREFIX", "remdia_src")
        .warnings(true)
        .flag_if_supported("-Wno-sign-compare")
        .compile("remdia_src");
}
