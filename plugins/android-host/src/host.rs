use anyhow::{Context, Result, ensure};
use jni::{
    JNIEnv,
    objects::{JByteArray, JClass, JObject, JString, JValue},
    sys::{jlong, jobject, jstring},
};
use remdia::{
    Cancellation, Engine, Frame, JobSession, MediaRequest, Session,
    graph::{Graph, GraphEvent, GraphSpec},
};
use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicI64, Ordering},
    },
    time::{Duration, Instant},
};
static NEXT: AtomicI64 = AtomicI64::new(1);
static MANIFESTS: OnceLock<String> = OnceLock::new();
static CANCEL: OnceLock<Mutex<HashMap<i64, Cancellation>>> = OnceLock::new();
static FRAMES: OnceLock<Mutex<HashMap<i64, Frame>>> = OnceLock::new();
enum Job {
    Media(Box<Session>),
    Image(JobSession),
    Graph(Graph),
}
thread_local! { static JOBS: RefCell<HashMap<i64, Job>> = RefCell::new(HashMap::new()); }
fn failure<T: Default>(env: &mut JNIEnv<'_>, result: Result<T>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => {
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            let _ = env.throw_new("java/lang/IllegalStateException", format!("{error:#}"));
            T::default()
        }
    }
}
fn string(env: &mut JNIEnv<'_>, value: JString<'_>) -> Result<String> {
    Ok(env.get_string(&value)?.into())
}
fn engine(budget: jlong) -> Result<Engine> {
    ensure!(
        (1024 * 1024..=1024 * 1024 * 1024).contains(&budget),
        "invalid managed budget"
    );
    let json = MANIFESTS.get().context("Remdia is not initialized")?;
    #[cfg(feature = "bundled")]
    {
        unsafe { Engine::bundled_with_preloaded(serde_json::from_str(json)?, budget as usize) }
    }
    #[cfg(not(feature = "bundled"))]
    {
        let manifests = serde_json::from_str(json)?;
        unsafe { Engine::from_preloaded(manifests, budget as usize) }
    }
}
fn cancellation(id: jlong) -> Result<Cancellation> {
    CANCEL
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("cancel lock poisoned"))?
        .get(&id)
        .cloned()
        .context("invalid cancellation handle")
}
fn register(id: jlong, job: Job) -> Result<()> {
    JOBS.with(|jobs| {
        let mut jobs = jobs.borrow_mut();
        ensure!(
            !jobs.contains_key(&id),
            "cancellation handle already in use"
        );
        jobs.insert(id, job);
        Ok(())
    })
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeInit(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    manifests: JString<'_>,
) {
    let result = (|| -> Result<()> {
        let json = string(&mut env, manifests)?;
        let _ = unsafe { Engine::from_preloaded(serde_json::from_str(&json)?, 96 * 1024 * 1024)? };
        if let Some(existing) = MANIFESTS.get() {
            ensure!(
                existing == &json,
                "Android engine already initialized with different packs"
            );
        } else {
            MANIFESTS
                .set(json)
                .map_err(|_| anyhow::anyhow!("concurrent initialization"))?;
        }
        Ok(())
    })();
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeCancelCreate(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
) -> jlong {
    let result = (|| -> Result<jlong> {
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        CANCEL
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("cancel lock poisoned"))?
            .insert(id, Cancellation::default());
        Ok(id)
    })();
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeCancel(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) {
    let result = (|| -> Result<()> {
        if let Some(cancel) = CANCEL
            .get_or_init(Mutex::default)
            .lock()
            .map_err(|_| anyhow::anyhow!("cancel lock poisoned"))?
            .get(&id)
        {
            cancel.cancel();
        }
        Ok(())
    })();
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeReleaseCancellation(
    _: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) {
    if let Ok(mut tokens) = CANCEL.get_or_init(Mutex::default).lock()
        && let Some(cancel) = tokens.remove(&id)
    {
        cancel.cancel();
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeOpen(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
    request: JString<'_>,
    budget: jlong,
) {
    let result = (|| -> Result<()> {
        let request = serde_json::from_str(&string(&mut env, request)?)?;
        let session = unsafe { engine(budget)?.open(request, cancellation(id)?)? };
        register(id, Job::Media(Box::new(session)))
    })();
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeImage(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
    bytes: JByteArray<'_>,
    operations: JString<'_>,
    budget: jlong,
) {
    let result = (|| -> Result<()> {
        ensure!(
            env.get_array_length(&bytes)? <= 40 * 1024 * 1024,
            "image input exceeds limit"
        );
        let input = remdia::image::ImageInput::Bytes(env.convert_byte_array(bytes)?.into());
        let ops = serde_json::from_str(&string(&mut env, operations)?)?;
        let job = unsafe {
            engine(budget)?.open_job(
                MediaRequest::Image(remdia::image::ImageRequest::new(input, ops)),
                cancellation(id)?,
            )?
        };
        register(id, Job::Image(job))
    })();
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeGraph(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
    spec: JString<'_>,
    budget: jlong,
) {
    let result = (|| -> Result<()> {
        let spec: GraphSpec = serde_json::from_str(&string(&mut env, spec)?)?;
        let graph = unsafe { engine(budget)?.graph(&spec, cancellation(id)?)? };
        register(id, Job::Graph(graph))
    })();
    failure(&mut env, result)
}
fn frame<'local>(env: &mut JNIEnv<'local>, frame: Frame) -> Result<jobject> {
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let info = env.new_string(serde_json::to_string(&frame.buffer().header)?)?;
    let count = (0..4)
        .take_while(|&i| frame.plane(i).is_some_and(|p| !p.is_empty()))
        .count();
    let planes = env.new_object_array(count as i32, "java/nio/ByteBuffer", JObject::null())?;
    for i in 0..count {
        let bytes = frame.plane(i).unwrap();
        // Frame stays pinned until Java Frame.close; views are read-only in Java.
        let buffer = unsafe { env.new_direct_byte_buffer(bytes.as_ptr().cast_mut(), bytes.len()) }?;
        env.set_object_array_element(&planes, i as i32, buffer)?;
    }
    let result = env.new_object(
        "dev/remdia/Remdia$Frame",
        "(JLjava/lang/String;[Ljava/nio/ByteBuffer;)V",
        &[
            JValue::Long(id),
            JValue::Object(&info),
            JValue::Object(&planes),
        ],
    )?;
    FRAMES
        .get_or_init(Mutex::default)
        .lock()
        .map_err(|_| anyhow::anyhow!("frame lock poisoned"))?
        .insert(id, frame);
    Ok(result.into_raw())
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeArtwork(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) -> jobject {
    let result = JOBS.with(|jobs| -> Result<jobject> {
        let mut jobs = jobs.borrow_mut();
        let job = jobs
            .get_mut(&id)
            .context("Session must be used on its creating worker")?;
        let Job::Media(session) = job else {
            anyhow::bail!("Only media sessions expose artwork");
        };
        session
            .artwork()?
            .map_or(Ok(std::ptr::null_mut()), |f| frame(&mut env, f))
    });
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeInfo(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) -> jstring {
    let result = JOBS.with(|jobs| -> Result<jstring> {
        let jobs = jobs.borrow();
        let job = jobs
            .get(&id)
            .context("Session must be used on its creating worker")?;
        let Job::Media(session) = job else {
            anyhow::bail!("Only media sessions expose track metadata");
        };
        Ok(env
            .new_string(
                serde_json::json!({"media":session.info(),"plan":session.plan()}).to_string(),
            )?
            .into_raw())
    });
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeRead(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) -> jobject {
    let result = JOBS.with(|jobs| -> Result<jobject> {
        let mut jobs = jobs.borrow_mut();
        let job = jobs
            .get_mut(&id)
            .context("Session must be used on its creating worker")?;
        let output = match job {
            Job::Media(session) => session.read()?,
            Job::Image(job) => job.read()?,
            Job::Graph(graph) => {
                let start = Instant::now();
                loop {
                    ensure!(
                        start.elapsed() < Duration::from_secs(30),
                        "graph read timed out"
                    );
                    match graph.poll()? {
                        GraphEvent::Output { buffer, .. } => {
                            break Some(Frame::from_buffer(buffer)?);
                        }
                        GraphEvent::End => break None,
                        GraphEvent::Pending => std::thread::sleep(Duration::from_millis(1)),
                    }
                }
            }
        };
        output.map_or(Ok(std::ptr::null_mut()), |f| frame(&mut env, f))
    });
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeSeek(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
    target: jlong,
) {
    let result = JOBS.with(|jobs| -> Result<()> {
        ensure!(target >= 0, "invalid seek target");
        let mut jobs = jobs.borrow_mut();
        let job = jobs
            .get_mut(&id)
            .context("Session must be used on its creating worker")?;
        match job {
            Job::Media(session) => session.seek(target).map(|_| ()),
            _ => anyhow::bail!("Only media sessions support seek"),
        }
    });
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeClose(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) {
    let result = JOBS.with(|jobs| -> Result<()> {
        jobs.borrow_mut()
            .remove(&id)
            .context("Session must close on its creating worker")?;
        Ok(())
    });
    if result.is_ok()
        && let Ok(mut tokens) = CANCEL.get_or_init(Mutex::default).lock()
    {
        tokens.remove(&id);
    }
    failure(&mut env, result)
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeReleaseFrame(
    _: JNIEnv<'_>,
    _: JClass<'_>,
    id: jlong,
) {
    if let Ok(mut frames) = FRAMES.get_or_init(Mutex::default).lock() {
        frames.remove(&id);
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_remdia_Remdia_nativeCapabilities(
    mut env: JNIEnv<'_>,
    _: JClass<'_>,
    budget: jlong,
) -> jstring {
    let result = (|| -> Result<jstring> {
        let engine = engine(budget)?;
        let info = unsafe {
            serde_json::json!({"audio":engine.audio_capabilities()?,"images":engine.image_capabilities()?,"demux":engine.demux_capabilities()?})
        };
        Ok(env.new_string(info.to_string())?.into_raw())
    })();
    failure(&mut env, result)
}
