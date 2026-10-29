//! Thin optional Java facade; the Engine itself has no JNI dependency.
#[cfg(target_os = "android")]
mod host;
#[cfg(target_os = "android")]
remdia_android_platform::jni_on_load!();
