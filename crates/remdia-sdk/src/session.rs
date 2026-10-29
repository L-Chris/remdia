use anyhow::{Context, Result, ensure};
use remdia::{Cancellation, Engine, Graph, GraphEvent, JobSession, MediaRequest, Session};
use remdia_graph::{
    Control, MediaBuffer,
    ffi::{Reply, respond},
};
use remdia_plugin_abi as abi;
use std::{
    ffi::c_void,
    thread::ThreadId,
    time::{Duration, Instant},
};

enum Job {
    Media(Box<Session>),
    Image(JobSession),
    Graph(Graph),
}
struct Handle {
    job: Job,
    worker: ThreadId,
}

unsafe fn json(view: abi::View) -> Result<serde_json::Value> {
    ensure!(
        view.len <= 1024 * 1024 && (view.len == 0 || !view.ptr.is_null()),
        "invalid SDK JSON view"
    );
    Ok(serde_json::from_slice(unsafe { view.as_slice() })?)
}
unsafe fn handle<'a>(raw: *mut c_void) -> Result<&'a mut Handle> {
    ensure!(!raw.is_null(), "null SDK handle");
    let handle = unsafe { &mut *raw.cast::<Handle>() };
    ensure!(
        handle.worker == std::thread::current().id(),
        "SDK session must stay on its creating worker"
    );
    Ok(handle)
}
/// # Safety
/// Views and output obey remdia.h; cancellation stays live until job close.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn remdia_sdk_open(
    config: abi::View,
    input: abi::View,
    control: abi::Control,
    destination: *mut *mut c_void,
    out: *mut abi::Output,
) -> i32 {
    unsafe {
        respond(out, || {
            ensure!(!destination.is_null(), "null SDK handle output");
            *destination = std::ptr::null_mut();
            let config = json(config)?;
            let budget =
                usize::try_from(config["budget_bytes"].as_u64().unwrap_or(96 * 1024 * 1024))?;
            ensure!(
                (1024 * 1024..=1024 * 1024 * 1024).contains(&budget),
                "invalid managed budget"
            );
            let engine = if let Some(path) = config["plugins"].as_str() {
                Engine::bundled_with_plugins(path, budget)?
            } else {
                Engine::bundled(budget)?
            };
            let cancel = Cancellation::from_control(Control(control));
            let (job, info) = match config["operation"].as_str().unwrap_or("media") {
                "media" => {
                    let session =
                        engine.open(serde_json::from_value(config["request"].clone())?, cancel)?;
                    let info = serde_json::json!({"media":session.info(), "plan":session.plan()});
                    (Job::Media(Box::new(session)), info)
                }
                "image" => {
                    ensure!(
                        input.len <= 40 * 1024 * 1024 && (input.len == 0 || !input.ptr.is_null()),
                        "invalid encoded image view"
                    );
                    let source = if let Some(path) = config["source"].as_str() {
                        remdia::image::ImageInput::File(path.into())
                    } else {
                        remdia::image::ImageInput::Bytes(input.as_slice().into())
                    };
                    let ops = serde_json::from_value(config["operations"].clone())?;
                    let request =
                        MediaRequest::Image(remdia::image::ImageRequest::new(source, ops));
                    let info = serde_json::to_value(engine.plan_job(&request)?)?;
                    (Job::Image(engine.open_job(request, cancel)?), info)
                }
                "graph" => {
                    let spec = serde_json::from_value(config["graph"].clone())?;
                    (
                        Job::Graph(engine.graph(&spec, cancel)?),
                        serde_json::json!({"graph":spec}),
                    )
                }
                _ => anyhow::bail!("unknown SDK operation"),
            };
            let output = Reply::Json(info);
            *destination = Box::into_raw(Box::new(Handle {
                job,
                worker: std::thread::current().id(),
            }))
            .cast();
            Ok((abi::OK, output))
        })
    }
}
/// # Safety
/// `raw` is a live SDK handle used serially on its creating worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn remdia_sdk_read(raw: *mut c_void, out: *mut abi::Output) -> i32 {
    unsafe {
        respond(out, || {
            let value = match &mut handle(raw)?.job {
                Job::Media(session) => session.read()?.map(|f| f.buffer().clone()),
                Job::Image(job) => job.read()?.map(|f| f.buffer().clone()),
                Job::Graph(graph) => {
                    let start = Instant::now();
                    loop {
                        ensure!(
                            start.elapsed() < Duration::from_secs(30),
                            "SDK graph read timed out"
                        );
                        match graph.poll()? {
                            GraphEvent::Output { buffer, .. } => break Some(buffer),
                            GraphEvent::End => break None,
                            GraphEvent::Pending => std::thread::sleep(Duration::from_millis(1)),
                        }
                    }
                }
            };
            Ok(
                value.map_or((abi::EOF, Reply::Empty), |buffer: MediaBuffer| {
                    (abi::OK, Reply::Buffer(buffer))
                }),
            )
        })
    }
}
/// # Safety
/// `raw` is a live SDK media handle on its creating worker.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn remdia_sdk_seek(
    raw: *mut c_void,
    target: i64,
    out: *mut abi::Output,
) -> i32 {
    unsafe {
        respond(out, || {
            let Job::Media(session) = &mut handle(raw)?.job else {
                anyhow::bail!("only media supports seeking");
            };
            Ok((
                abi::OK,
                Reply::Json(serde_json::json!({"generation":session.seek(target)?})),
            ))
        })
    }
}
/// # Safety
/// `raw` is live, used on its creating worker and must not be used after success.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn remdia_sdk_close(raw: *mut c_void, out: *mut abi::Output) -> i32 {
    unsafe {
        respond(out, || {
            let _ = handle(raw).context("close SDK job")?;
            drop(Box::from_raw(raw.cast::<Handle>()));
            Ok((abi::OK, Reply::Empty))
        })
    }
}
