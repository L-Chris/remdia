use anyhow::{Context, Result, ensure};
use clap::{Parser, Subcommand};
use remdia::{Cancellation, Engine, FrameInfo, Request, Task};
use std::path::PathBuf;
#[derive(Parser)]
#[command(version, about = "Windows-first modular media SDK and diagnostic CLI")]
struct Args {
    #[arg(long, global = true, env = "REMDIA_PLUGIN_DIR")]
    plugins: Option<PathBuf>,
    #[arg(long, global = true, default_value_t = 256)]
    budget_mib: usize,
    #[arg(long, global = true, default_value_t = 30_000)]
    timeout_ms: u64,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Plugins,
    /// Enumerate actual installed system image read/write capabilities.
    Capabilities,
    /// Run system image components through the shared media graph.
    Image {
        source: PathBuf,
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        width: Option<u32>,
        #[arg(long)]
        height: Option<u32>,
        #[arg(long)]
        probe: bool,
        #[arg(long)]
        plan: bool,
    },
    /// Execute a user-defined DAG of compatible component factories.
    Graph {
        file: PathBuf,
    },
    Probe {
        source: String,
    },
    Plan {
        source: String,
        #[arg(long,default_value="playback",value_parser=["probe","playback","audio","thumbnail"])]
        task: String,
    },
    /// Decode without presentation, reporting native frame counts and resources.
    Decode {
        source: String,
        #[arg(long, default_value_t = 0)]
        start_us: i64,
        #[arg(long, default_value_t = 300)]
        frames: usize,
        #[arg(long)]
        software: bool,
    },
    Thumbnail {
        source: String,
        output: PathBuf,
        #[arg(long, default_value_t = 0)]
        at_us: i64,
    },
    Extract {
        source: String,
        output: PathBuf,
        #[arg(long, default_value_t = 0)]
        start_us: i64,
        #[arg(long, default_value_t = 30_000_000)]
        duration_us: i64,
    },
}
fn main() -> Result<()> {
    let args = Args::parse();
    let directory = args.plugins.unwrap_or(
        std::env::current_exe()?
            .parent()
            .context("executable directory")?
            .join("plugins"),
    );
    let engine = Engine::discover(
        directory,
        args.budget_mib
            .checked_mul(1024 * 1024)
            .context("budget overflow")?,
    )?;
    if matches!(args.command, Command::Capabilities) {
        let spec = remdia::GraphSpec {
            nodes: vec![remdia::graph::NodeSpec {
                id: "capabilities".into(),
                config: remdia::NodeConfig {
                    component: format!("{}.image.capabilities", std::env::consts::OS),
                    settings: serde_json::json!({}),
                },
            }],
            edges: vec![],
        };
        let mut graph = unsafe { engine.graph(&spec, Cancellation::default())? };
        loop {
            match graph.poll()? {
                remdia::GraphEvent::Output { buffer, .. } => {
                    println!("{}", std::str::from_utf8(buffer.bytes())?)
                }
                remdia::GraphEvent::End => break,
                remdia::GraphEvent::Pending => {}
            }
        }
        return Ok(());
    }
    if let Command::Image {
        source,
        output,
        width,
        height,
        probe,
        plan,
    } = &args.command
    {
        use remdia::image::{ImageInput, ImageOperation as Op, ImageRequest};
        let mut operations = vec![Op::new(
            if *probe { "probe" } else { "decode" },
            serde_json::json!({"max_width":width.unwrap_or(16384),"max_height":height.unwrap_or(65536)}),
        )];
        if let Some(output) = output {
            ensure!(!probe, "probe cannot encode an output image");
            let jpeg = output
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"));
            ensure!(
                jpeg || output
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("png")),
                "output must be PNG/JPEG"
            );
            if jpeg {
                operations.push(Op::new(
                    "alpha",
                    serde_json::json!({"background":[255,255,255]}),
                ))
            }
            operations.push(Op::new(
                "encode",
                serde_json::json!({"format":if jpeg {"jpeg"} else {"png"},"quality":0.95}),
            ));
        }
        let request = remdia::MediaRequest::Image(ImageRequest::new(
            ImageInput::File(source.clone()),
            operations,
        ));
        if *plan {
            println!(
                "{}",
                serde_json::to_string_pretty(&unsafe { engine.explain_job(&request)? })?
            );
            return Ok(());
        }
        let mut job = unsafe { engine.open_job(request, Cancellation::default())? };
        let frame = job.read()?.context("no image output")?;
        if let Some(output) = output {
            write_new(output, frame.bytes())?
        } else if *probe {
            println!("{}", std::str::from_utf8(frame.bytes())?)
        } else {
            println!("{}", serde_json::to_string(frame.info())?)
        }
        return Ok(());
    }
    if matches!(args.command, Command::Plugins) {
        for c in &engine.registry().candidates {
            println!("{}", serde_json::to_string_pretty(&c.manifest)?);
        }
        return Ok(());
    }
    if let Command::Graph { file } = args.command {
        let spec: remdia::GraphSpec = serde_json::from_slice(&std::fs::read(file)?)?;
        let mut graph = unsafe { engine.graph(&spec, Cancellation::default()) }?;
        let mut outputs = 0;
        let started = std::time::Instant::now();
        loop {
            anyhow::ensure!(
                started.elapsed().as_millis() < args.timeout_ms as u128,
                "graph execution timed out"
            );
            match graph.poll()? {
                remdia::GraphEvent::End => break,
                remdia::GraphEvent::Pending => {}
                remdia::GraphEvent::Output { node, buffer } => {
                    outputs += 1;
                    println!(
                        "{}",
                        serde_json::json!({"node":node,"header":buffer.header,"bytes":buffer.len()})
                    );
                }
            }
        }
        eprintln!(
            "graph complete: {outputs} outputs; peak reservation {} bytes",
            engine.budget().peak()
        );
        return Ok(());
    }
    let source = match &args.command {
        Command::Probe { source }
        | Command::Plan { source, .. }
        | Command::Decode { source, .. }
        | Command::Thumbnail { source, .. }
        | Command::Extract { source, .. } => source,
        Command::Capabilities
        | Command::Image { .. }
        | Command::Plugins
        | Command::Graph { .. } => unreachable!(),
    };
    let mut request = Request::playback(source);
    request.timeout_ms = args.timeout_ms;
    match &args.command {
        Command::Probe { .. } => request.task = Task::Probe,
        Command::Plan { task, .. } => {
            request.task = match task.as_str() {
                "probe" => Task::Probe,
                "audio" => Task::Audio,
                "thumbnail" => Task::Thumbnail,
                _ => Task::Playback,
            }
        }
        Command::Thumbnail { .. } => request.task = Task::Thumbnail,
        Command::Extract { .. } => {
            request.task = Task::Audio;
            request.audio_rate = 16000;
            request.audio_channels = 1;
        }
        Command::Decode { software, .. } => request.hardware = !*software,
        _ => {}
    }
    if matches!(args.command, Command::Plan { .. }) {
        println!("{}", serde_json::to_string_pretty(&engine.plan(&request)?)?);
        return Ok(());
    }
    // The user explicitly selects the installed native capability pack directory.
    let mut session = unsafe { engine.open(request, Cancellation::default()) }?;
    match args.command {
        Command::Probe { .. } => println!("{}", serde_json::to_string_pretty(session.info())?),
        Command::Decode {
            start_us, frames, ..
        } => {
            if start_us > 0 {
                session.seek(start_us)?;
            }
            let (mut audio, mut video, mut hardware) = (0, 0, 0);
            let mut eof = false;
            for _ in 0..frames {
                let Some(frame) = session.read()? else {
                    eof = true;
                    break;
                };
                match frame.info() {
                    FrameInfo::Video {
                        hardware_decoded, ..
                    } => {
                        video += 1;
                        hardware += usize::from(*hardware_decoded);
                    }
                    FrameInfo::Audio { .. } => audio += 1,
                    _ => {}
                }
            }
            println!(
                "{}",
                serde_json::json!({"video_frames":video,"audio_frames":audio,"hardware_frames":hardware,"eof":eof,"managed_peak_bytes":engine.budget().peak()})
            );
        }
        Command::Thumbnail { output, at_us, .. } => {
            if at_us > 0 {
                session.seek(at_us)?;
            }
            let frame = session.read()?.context("no thumbnail frame")?;
            use remdia::image::{Image, ImageInput, ImageOperation, ImageRequest};
            let image = Image::from_buffer(frame.buffer().clone())?;
            let request = remdia::MediaRequest::Image(ImageRequest::new(
                ImageInput::Pixels(image),
                vec![ImageOperation::new(
                    "encode",
                    serde_json::json!({"format":"png"}),
                )],
            ));
            let mut encoder = unsafe { engine.open_job(request, Cancellation::default())? };
            let encoded = encoder.read()?.context("no encoded thumbnail")?;
            write_new(&output, encoded.bytes())?;
        }
        Command::Extract {
            output,
            start_us,
            duration_us,
            ..
        } => {
            let chunk = session.audio_chunk(start_us, duration_us)?;
            if output.extension().is_some_and(|e| e == "mp3") {
                std::fs::write(output, session.encode_mp3(&chunk)?.bytes())?;
            } else {
                ensure!(
                    output.extension().is_some_and(|e| e == "wav"),
                    "output must end in .wav or .mp3"
                );
                std::fs::write(output, chunk.wav())?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn write_new(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}
