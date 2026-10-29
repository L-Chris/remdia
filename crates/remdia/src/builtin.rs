//! Build-time composition; these are the same factories as the external packs.
use remdia_graph::ffi::BuiltinPlugin;

pub fn plugins() -> Vec<(&'static str, BuiltinPlugin)> {
    vec![
        #[cfg(feature = "builtin-system")]
        ("native", remdia_backend_native::builtin_plugin()),
        #[cfg(all(
            feature = "builtin-system",
            any(windows, target_os = "macos", target_os = "android")
        ))]
        ("image", remdia_backend_image::builtin_plugin()),
        #[cfg(all(feature = "builtin-system", any(windows, target_os = "macos")))]
        (
            "system_audio",
            remdia_backend_system_audio::builtin_plugin(),
        ),
        #[cfg(all(feature = "builtin-system", any(windows, target_os = "macos")))]
        (
            "system_demux",
            remdia_backend_system_demux::builtin_plugin(),
        ),
        #[cfg(all(feature = "builtin-system", windows))]
        ("windows", remdia_backend_windows::builtin_plugin()),
        #[cfg(all(feature = "builtin-system", target_os = "macos"))]
        ("macos", remdia_backend_macos::builtin_plugin()),
        #[cfg(all(feature = "builtin-system", target_os = "android"))]
        ("android", remdia_backend_android::builtin_plugin()),
        #[cfg(all(feature = "builtin-compat-demux", any(windows, target_os = "macos")))]
        (
            "fallback_demux",
            remdia_backend_fallback_demux::builtin_plugin(),
        ),
        #[cfg(all(feature = "builtin-src", target_os = "android"))]
        (
            "fallback_resample",
            remdia_backend_fallback_resample::builtin_plugin(),
        ),
    ]
}

pub fn descriptors() -> Vec<remdia_graph::ComponentDescriptor> {
    plugins()
        .into_iter()
        .flat_map(|(_, p)| (p.describe)())
        .collect()
}

pub fn create(
    config: &remdia_graph::NodeConfig,
    control: remdia_graph::Control,
) -> anyhow::Result<Box<dyn remdia_graph::Component>> {
    for (_, plugin) in plugins() {
        if (plugin.describe)().iter().any(|d| d.id == config.component) {
            return (plugin.create)(config, control);
        }
    }
    anyhow::bail!("built-in component is not enabled: {}", config.component)
}
