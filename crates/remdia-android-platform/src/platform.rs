use anyhow::{Context, Result, ensure};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JByteArray, JClass, JObject, JString, JValue},
};
use std::{ffi::c_void, sync::OnceLock};

struct Runtime {
    vm: JavaVM,
    bridge: GlobalRef,
}
static RUNTIME: OnceLock<Runtime> = OnceLock::new();

/// # Safety
/// Called only from JNI_OnLoad with a live process JavaVM.
pub unsafe fn initialize(raw: *mut c_void) -> i32 {
    let result = (|| -> Result<()> {
        let vm = unsafe { JavaVM::from_raw(raw.cast()) }?;
        let bridge = {
            let mut env = vm.get_env()?;
            let class = env.find_class("dev/remdia/Bridge")?;
            env.new_global_ref(class)?
        };
        let _ = RUNTIME.set(Runtime { vm, bridge });
        Ok(())
    })();
    if result.is_ok() {
        jni::sys::JNI_VERSION_1_6
    } else {
        jni::sys::JNI_ERR
    }
}

pub fn with_env<T>(f: impl FnOnce(&mut JNIEnv<'_>, &JClass<'_>) -> Result<T>) -> Result<T> {
    let runtime = RUNTIME
        .get()
        .context("Android bridge not initialized; call Remdia.initialize first")?;
    let mut env = runtime.vm.attach_current_thread()?;
    env.with_local_frame(48, |env| {
        let class: &JClass<'_> = runtime.bridge.as_obj().into();
        let result = f(env, class);
        if env.exception_check()? {
            let exception = env.exception_occurred()?;
            env.exception_clear()?;
            let message = env
                .call_method(&exception, "toString", "()Ljava/lang/String;", &[])
                .ok()
                .and_then(|v| v.l().ok())
                .and_then(|s| env.get_string((&s).into()).ok().map(String::from));
            // Do not leave a pending Java exception on a reusable native worker.
            if env.exception_check()? {
                env.exception_clear()?;
            }
            anyhow::bail!(
                "Android framework: {}",
                message.as_deref().unwrap_or("operation failed")
            );
        }
        result
    })
}

pub fn text(env: &mut JNIEnv<'_>, object: JObject<'_>) -> Result<String> {
    Ok(env.get_string(&JString::from(object))?.into())
}

pub fn static_json(method: &str, input: &serde_json::Value) -> Result<serde_json::Value> {
    with_env(|env, class| {
        let input = env.new_string(input.to_string())?;
        let result = env
            .call_static_method(
                class,
                method,
                "(Ljava/lang/String;)Ljava/lang/String;",
                &[JValue::Object(&input)],
            )?
            .l()?;
        Ok(serde_json::from_str(&text(env, result)?)?)
    })
}

pub struct Node(GlobalRef);
impl Node {
    pub fn open(component: &str, settings: &serde_json::Value) -> Result<Self> {
        with_env(|env, class| {
            let component = env.new_string(component)?;
            let settings = env.new_string(settings.to_string())?;
            let object = env
                .call_static_method(
                    class,
                    "open",
                    "(Ljava/lang/String;Ljava/lang/String;)Ldev/remdia/Bridge$Node;",
                    &[JValue::Object(&component), JValue::Object(&settings)],
                )?
                .l()?;
            ensure!(!object.is_null(), "missing Android component");
            Ok(Self(env.new_global_ref(object)?))
        })
    }
    pub fn info(&self) -> Result<serde_json::Value> {
        with_env(|env, _| {
            let info = env
                .call_method(self.0.as_obj(), "info", "()Ljava/lang/String;", &[])?
                .l()?;
            Ok(serde_json::from_str(&text(env, info)?)?)
        })
    }
    pub fn submit(&self, header: &str, data: &[u8]) -> Result<bool> {
        with_env(|env, _| {
            let header = env.new_string(header)?;
            // Framework bridge must consume/copy this borrowed memory before return.
            let data = unsafe { env.new_direct_byte_buffer(data.as_ptr().cast_mut(), data.len()) }?;
            Ok(env
                .call_method(
                    self.0.as_obj(),
                    "submit",
                    "(Ljava/lang/String;Ljava/nio/ByteBuffer;)Z",
                    &[JValue::Object(&header), JValue::Object(&data)],
                )?
                .z()?)
        })
    }
    pub fn receive(&self, max: usize) -> Result<Option<(serde_json::Value, Vec<u8>)>> {
        with_env(|env, _| {
            let output = env
                .call_method(
                    self.0.as_obj(),
                    "receive",
                    "()Ldev/remdia/Bridge$Output;",
                    &[],
                )?
                .l()?;
            if output.is_null() {
                return Ok(None);
            }
            let header = env
                .get_field(&output, "header", "Ljava/lang/String;")?
                .l()?;
            let bytes = JByteArray::from(env.get_field(&output, "bytes", "[B")?.l()?);
            ensure!(
                env.get_array_length(&bytes)? as usize <= max,
                "Android component exceeded output bound"
            );
            Ok(Some((
                serde_json::from_str(&text(env, header)?)?,
                env.convert_byte_array(bytes)?,
            )))
        })
    }
    pub fn ended(&self) -> Result<bool> {
        with_env(|env, _| Ok(env.call_method(self.0.as_obj(), "ended", "()Z", &[])?.z()?))
    }
    pub fn command(
        &self,
        command: &serde_json::Value,
        max: usize,
    ) -> Result<Option<(serde_json::Value, Vec<u8>)>> {
        with_env(|env, _| {
            let command = env.new_string(command.to_string())?;
            let output = env
                .call_method(
                    self.0.as_obj(),
                    "command",
                    "(Ljava/lang/String;)Ldev/remdia/Bridge$Output;",
                    &[JValue::Object(&command)],
                )?
                .l()?;
            if output.is_null() {
                return Ok(None);
            }
            let header = env
                .get_field(&output, "header", "Ljava/lang/String;")?
                .l()?;
            let bytes = JByteArray::from(env.get_field(&output, "bytes", "[B")?.l()?);
            ensure!(
                env.get_array_length(&bytes)? as usize <= max,
                "Android command output exceeds limit"
            );
            Ok(Some((
                serde_json::from_str(&text(env, header)?)?,
                env.convert_byte_array(bytes)?,
            )))
        })
    }
    pub fn drain(&self) -> Result<()> {
        with_env(|env, _| {
            env.call_method(self.0.as_obj(), "drain", "()V", &[])?;
            Ok(())
        })
    }
    pub fn reset(&self, generation: u64, target: i64) -> Result<()> {
        with_env(|env, _| {
            env.call_method(
                self.0.as_obj(),
                "reset",
                "(JJ)V",
                &[JValue::Long(generation as i64), JValue::Long(target)],
            )?;
            Ok(())
        })
    }
}
impl Drop for Node {
    fn drop(&mut self) {
        let _ = with_env(|env, _| {
            env.call_method(self.0.as_obj(), "close", "()V", &[])?;
            Ok(())
        });
    }
}
