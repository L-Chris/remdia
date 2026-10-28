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
    if matches!(args.command, Command::Plugins) {
        for c in &engine.registry().candidates {
            println!("{}", serde_json::to_string_pretty(&c.manifest)?);
        }
        return Ok(());
    }
    let source = match &args.command {
        Command::Probe { source }
        | Command::Plan { source, .. }
        | Command::Decode { source, .. }
        | Command::Thumbnail { source, .. }
        | Command::Extract { source, .. } => source,
        Command::Plugins => unreachable!(),
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
            let FrameInfo::Video {
                width,
                height,
                planes,
                format,
                ..
            } = frame.info()
            else {
                anyhow::bail!("expected video")
            };
            ensure!(format == "rgba", "thumbnail must be RGBA");
            let mut rgba = Vec::with_capacity((*width as usize) * (*height as usize) * 4);
            for row in frame
                .bytes()
                .chunks(planes[0].stride)
                .take(*height as usize)
            {
                rgba.extend_from_slice(&row[..*width as usize * 4]);
            }
            image::save_buffer_with_format(
                output,
                &rgba,
                *width,
                *height,
                image::ColorType::Rgba8,
                image::ImageFormat::Png,
            )?;
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
