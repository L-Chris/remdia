//! Private Android framework bridge. No codec algorithms or core SDK linkage.
#[cfg(target_os = "android")]
mod platform;
#[cfg(target_os = "android")]
pub use platform::*;

/// Each independently loaded capability library receives its own VM/class ref.
#[macro_export]
macro_rules! jni_on_load {
    () => {
        #[cfg(target_os = "android")]
        #[unsafe(no_mangle)]
        pub unsafe extern "system" fn JNI_OnLoad(
            vm: *mut std::ffi::c_void,
            _: *mut std::ffi::c_void,
        ) -> i32 {
            // SAFETY: Android calls this with the process JavaVM.
            unsafe { $crate::initialize(vm) }
        }
    };
}
