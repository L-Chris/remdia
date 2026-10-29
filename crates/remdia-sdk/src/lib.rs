//! One native library: the SDK engine, statically registered components and C/JNI facades.
#[cfg(target_os = "android")]
#[path = "../../../plugins/android-host/src/host.rs"]
mod host;
mod session;
#[cfg(target_os = "android")]
remdia_android_platform::jni_on_load!();
remdia_graph::export_plugin!(@dynamic remdia::builtin::descriptors, remdia::builtin::create);
