//! jay: a consented, local-first listening assistant.
//!
//! At this stage it does one thing: capture audio and prove it is real. The
//! transcript, the overlay and the agent all come later, and none of them are
//! worth building on a capture path nobody has watched produce a number.

use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use jay_audio::vad::{SpeechSegmenter, Utterance};
use jay_audio::{Channel, Frame, SAMPLE_RATE, mic};
use jay_stt::models::Model;
use jay_stt::whisper::Whisper;
use jay_stt::SpeechModel;

#[derive(Parser)]
#[command(name = "jay", version, about = "A consented, local-first listening assistant")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

/// Which capture paths to run.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum Source {
    /// The microphone only. You.
    Mic,
    /// System output only. Whoever else is talking.
    System,
    /// Both, kept on separate channels.
    Both,
}

impl Source {
    fn uses_mic(self) -> bool {
        matches!(self, Source::Mic | Source::Both)
    }

    fn uses_system(self) -> bool {
        matches!(self, Source::System | Source::Both)
    }
}

/// Which kind of help to ask for.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum AskMode {
    /// Mock interview practice: points to think with, and what you missed.
    Rehearsal,
    /// Live pairing: concrete, short, opinionated.
    Pairing,
    /// Something went wrong: what is likely responsible and what to check.
    Dev,
}

impl From<AskMode> for jay_agent::Mode {
    fn from(mode: AskMode) -> Self {
        match mode {
            AskMode::Rehearsal => jay_agent::Mode::Rehearsal,
            AskMode::Pairing => jay_agent::Mode::Pairing,
            AskMode::Dev => jay_agent::Mode::Dev,
        }
    }
}

#[derive(Subcommand)]
enum Command {
    /// List the audio input devices jay can see.
    Devices,
    /// Capture from the microphone and report signal levels.
    ///
    /// A smoke test for the capture path: if the levels move when you speak
    /// and the dropped-sample count stays at zero, the pipeline is sound.
    Listen {
        /// Which audio to listen to.
        #[arg(short = 'S', long, value_enum, default_value_t = Source::Mic)]
        source: Source,
        /// Input device name. Defaults to the system default input.
        #[arg(short, long)]
        device: Option<String>,
        /// How long to listen for, in seconds.
        #[arg(short, long, default_value_t = 10)]
        seconds: u64,
        /// Also write the summary here.
        ///
        /// Needed when jay is launched via `open -a`, which detaches it from
        /// the terminal and takes stdout with it.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    /// Ask jay for help with one question, without the audio pipeline.
    ///
    /// The quickest way to see what a suggestion actually looks like, and what
    /// it costs, before wiring it to a microphone.
    Ask {
        /// The question, as it would have been heard.
        question: String,
        /// Which kind of help to ask for.
        #[arg(short, long, value_enum, default_value_t = AskMode::Rehearsal)]
        mode: AskMode,
        /// Model for the reasoning step.
        #[arg(long, default_value = jay_agent::claude::DEFAULT_MODEL)]
        model: String,
        /// Also send what is on screen right now.
        ///
        /// Captures the focused window at the moment of asking, not
        /// continuously. Needs Screen Recording permission, which means
        /// launching jay from its .app bundle.
        #[arg(long)]
        screen: bool,
    },
    /// Transcribe a 16 kHz mono WAV file.
    ///
    /// Useful for working through a recording after the fact, and for checking
    /// the model end to end without having to talk to your laptop.
    File {
        /// Path to the WAV file.
        path: std::path::PathBuf,
        /// Whisper model: tiny, base or small.
        #[arg(short, long, default_value = "base")]
        model: Model,
    },
    /// Transcribe speech live, from the microphone and/or system audio.
    Transcribe {
        /// Which audio to listen to.
        #[arg(short = 'S', long, value_enum, default_value_t = Source::Mic)]
        source: Source,
        /// Input device name. Defaults to the system default input.
        #[arg(short, long)]
        device: Option<String>,
        /// Whisper model: tiny, base or small. Downloaded on first use.
        #[arg(short, long, default_value = "base")]
        model: Model,
        /// How long to run for, in seconds. Zero runs until interrupted.
        #[arg(short, long, default_value_t = 60)]
        seconds: u64,
        /// Show the transcript in a floating overlay instead of the terminal.
        #[arg(long)]
        overlay: bool,
        /// Offer suggestions when someone asks a question.
        ///
        /// Off by default: listening is free, suggesting is not.
        #[arg(long)]
        assist: bool,
        /// What kind of help to offer, when assisting.
        #[arg(long, value_enum, default_value_t = AskMode::Pairing)]
        mode: AskMode,
        /// Hard ceiling on what this session may spend, in dollars.
        #[arg(long, default_value_t = 2.0)]
        budget: f64,
        /// Minimum seconds between suggestions.
        #[arg(long, default_value_t = 30)]
        cooldown: u64,
    },
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jay=info,jay_audio=info".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Devices => devices(),
        Command::Listen {
            source,
            device,
            seconds,
            out,
        } => listen(source, device.as_deref(), seconds, out.as_deref()),
        Command::Transcribe {
            source,
            device,
            model,
            seconds,
            overlay,
            assist,
            mode,
            budget,
            cooldown,
        } => transcribe(
            source,
            device.map(Into::into),
            model,
            seconds,
            overlay,
            assist.then(|| Assist {
                mode: mode.into(),
                budget_usd: budget,
                cooldown: Duration::from_secs(cooldown),
            }),
        ),
        Command::File { path, model } => transcribe_file(&path, model),
        Command::Ask {
            question,
            mode,
            model,
            screen,
        } => ask(&question, mode.into(), &model, screen),
    }
}

/// Transcribe a WAV file, segmenting it exactly as the live path would.
///
/// Running the same VAD and the same model over a file means a bad live result
/// can be reproduced offline, which is the difference between debugging this
/// and guessing at it.
fn transcribe_file(path: &std::path::Path, model: Model) -> Result<()> {
    let mut reader = hound::WavReader::open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    let spec = reader.spec();

    if spec.sample_rate != SAMPLE_RATE || spec.channels != 1 {
        anyhow::bail!(
            "expected 16 kHz mono, got {} Hz with {} channel(s). Convert it first, e.g.\n  \
             afconvert -f WAVE -d LEI16@16000 -c 1 in.wav out.wav",
            spec.sample_rate,
            spec.channels
        );
    }

    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().filter_map(|s| s.ok()).collect(),
        hound::SampleFormat::Int => reader
            .samples::<i16>()
            .filter_map(|s| s.ok())
            .map(|s| f32::from(s) / 32768.0)
            .collect(),
    };

    let audio_seconds = samples.len() as f32 / SAMPLE_RATE as f32;
    println!("{}: {audio_seconds:.1}s of audio", path.display());

    let mut whisper = Whisper::load(model).context("loading whisper model")?;
    let mut segmenter = SpeechSegmenter::new(Channel::Mic)?;
    let started = Instant::now();

    let mut utterances = Vec::new();
    for chunk in samples.chunks(jay_audio::FRAME_SAMPLES) {
        if chunk.len() < jay_audio::FRAME_SAMPLES {
            break; // the VAD needs whole frames
        }
        let frame = Frame {
            channel: Channel::Mic,
            samples: chunk.to_vec(),
            captured_at: Instant::now(),
        };
        if let Some(utterance) = segmenter.push(&frame) {
            utterances.push(utterance);
        }
    }
    if let Some(tail) = segmenter.flush() {
        utterances.push(tail);
    }

    if utterances.is_empty() {
        println!("the VAD found no speech in this file");
        return Ok(());
    }

    for utterance in &utterances {
        let result = whisper.transcribe(&utterance.samples)?;
        println!(
            "  [{:.1}s] {}   ({:.0}ms inference)",
            utterance.duration().as_secs_f32(),
            result.text,
            result.inference.as_secs_f32() * 1000.0
        );
    }

    let wall = started.elapsed().as_secs_f32();
    println!(
        "\n{} utterance(s), {wall:.1}s wall for {audio_seconds:.1}s of audio ({:.1}x real time)",
        utterances.len(),
        audio_seconds / wall
    );
    Ok(())
}

/// Live transcription: capture, segment on speech, transcribe each utterance.
///
/// Whisper runs on its own thread. An utterance can be twenty seconds long and
/// take a second or two to decode, and blocking the capture loop on that would
/// back the frame channel up and start dropping audio.
#[allow(clippy::too_many_arguments)]
fn transcribe(
    source: Source,
    device: Option<String>,
    model: Model,
    seconds: u64,
    overlay: bool,
    assist: Option<Assist>,
) -> Result<()> {
    // Everything downstream reads one line channel, so the terminal and the
    // overlay are just two consumers of the same stream rather than two paths
    // through the pipeline.
    let (line_tx, line_rx) = crossbeam_channel::unbounded::<jay_ui::Line>();

    if !overlay {
        let printer = std::thread::Builder::new()
            .name("jay-printer".into())
            .spawn(move || {
                for line in line_rx {
                    match line.kind {
                        jay_ui::Kind::Suggestion => {
                            println!("\n  ┌─ jay suggests ─────");
                            for l in line.text.lines() {
                                println!("  │ {l}");
                            }
                            println!("  └───────────────────\n");
                        }
                        jay_ui::Kind::Notice => println!("  ({})", line.text),
                        jay_ui::Kind::Transcript => println!(
                            "[{}] {}   ({:.1}s behind)",
                            line.speaker,
                            line.text,
                            line.lag.as_secs_f32()
                        ),
                    }
                }
            })
            .context("spawning the printer")?;

        let result = run_pipeline(source, device.as_deref(), model, seconds, line_tx, assist);
        let _ = printer.join();
        return result;
    }

    // The whole pipeline moves to a background thread: on macOS the windowing
    // event loop insists on the main thread and will not negotiate.
    std::thread::Builder::new()
        .name("jay-pipeline".into())
        .spawn(move || {
            if let Err(e) =
                run_pipeline(source, device.as_deref(), model, seconds, line_tx, assist)
            {
                tracing::error!(%e, "capture pipeline stopped");
            }
        })
        .context("spawning the capture pipeline")?;

    jay_ui::run(line_rx, model.to_string()).map_err(|e| anyhow::anyhow!("overlay: {e}"))
}

/// Settings for the half of jay that costs money.
#[derive(Debug, Clone, Copy)]
struct Assist {
    mode: jay_agent::Mode,
    /// Hard ceiling on what one session may spend, in dollars.
    budget_usd: f64,
    /// Minimum gap between suggestions.
    ///
    /// Without this, three questions in quick succession are three
    /// simultaneous escalations and roughly sixty cents.
    cooldown: Duration,
}

/// Runs suggestions on their own thread, and refuses to overspend.
fn spawn_assistant(
    assist: Assist,
    model: &str,
    questions: crossbeam_channel::Receiver<String>,
    lines: crossbeam_channel::Sender<jay_ui::Line>,
) -> Result<std::thread::JoinHandle<()>> {
    let claude = jay_agent::claude::Claude::new(model);
    std::thread::Builder::new()
        .name("jay-assist".into())
        .spawn(move || {
            let mut spent = 0.0f64;
            for question in questions {
                if spent >= assist.budget_usd {
                    // Said once, then the loop keeps draining so the sender
                    // never blocks on a full channel.
                    continue;
                }

                let started = Instant::now();
                match claude.suggest(assist.mode, &question, &[]) {
                    Ok(suggestion) => {
                        spent += suggestion.cost_usd;
                        let _ = lines.send(jay_ui::Line::suggestion(
                            suggestion.text,
                            started.elapsed(),
                        ));
                        let _ = lines.send(jay_ui::Line::notice(format!(
                            "{:.1}s · ${:.3} · ${:.2} of ${:.2} spent",
                            suggestion.latency.as_secs_f32(),
                            suggestion.cost_usd,
                            spent,
                            assist.budget_usd
                        )));
                        if spent >= assist.budget_usd {
                            let _ = lines.send(jay_ui::Line::notice(
                                "session budget reached. jay is listening but will not \
                                 suggest again until you restart it."
                                    .to_string(),
                            ));
                        }
                    }
                    Err(e) => {
                        tracing::error!(%e, "suggestion failed");
                        let _ = lines.send(jay_ui::Line::notice(format!("suggestion failed: {e}")));
                    }
                }
            }
        })
        .context("spawning the assistant")
}

fn run_pipeline(
    source: Source,
    device: Option<&str>,
    model: Model,
    seconds: u64,
    lines: crossbeam_channel::Sender<jay_ui::Line>,
    assist: Option<Assist>,
) -> Result<()> {
    let mut whisper = Whisper::load(model).context("loading whisper model")?;

    let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Frame>(512);
    let (utterance_tx, utterance_rx) = crossbeam_channel::bounded::<Utterance>(16);

    // Both captures feed the one frame channel; the channel tag on each frame
    // is what keeps the two speakers apart downstream.
    let _mic = if source.uses_mic() {
        Some(mic::start(device, frame_tx.clone()).context("starting microphone capture")?)
    } else {
        None
    };

    let _system = if source.uses_system() {
        Some(jay_audio::system::start(frame_tx.clone()).context(
            "starting the system audio tap. macOS asks for permission the first \
             time; if it was refused, grant it in System Settings > Privacy & \
             Security > Screen & System Audio Recording",
        )?)
    } else {
        None
    };
    drop(frame_tx);

    // The assistant, if this session is paying for one.
    let (question_tx, assistant) = match assist {
        Some(settings) => {
            let (tx, rx) = crossbeam_channel::bounded::<String>(4);
            let handle = spawn_assistant(
                settings,
                jay_agent::claude::DEFAULT_MODEL,
                rx,
                lines.clone(),
            )?;
            let _ = lines.send(jay_ui::Line::notice(format!(
                "assisting in {:?} mode, up to ${:.2} this session",
                settings.mode, settings.budget_usd
            )));
            (Some(tx), Some(handle))
        }
        None => (None, None),
    };
    let cooldown = assist.map(|a| a.cooldown).unwrap_or_default();

    let worker = std::thread::Builder::new()
        .name("jay-stt".into())
        .spawn(move || {
            let mut last_suggestion: Option<Instant> = None;
            for utterance in utterance_rx {
                let spoken = utterance.duration();
                let result = match whisper.transcribe(&utterance.samples) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(%e, "transcription failed");
                        continue;
                    }
                };

                if jay_stt::is_hallucination(&result.text) {
                    tracing::debug!(text = %result.text, "dropped a whisper artefact");
                    continue;
                }

                // Lag measured from the first sample of the utterance, which
                // is the number a listener would actually notice.
                let lag = utterance.started_at.elapsed();

                tracing::debug!(
                    spoken = spoken.as_secs_f32(),
                    inference_ms = result.inference.as_secs_f32() * 1000.0,
                    "transcribed"
                );

                // The gate runs before the line is even displayed, so a
                // question starts its (slow) escalation as early as possible.
                if let Some(tx) = &question_tx
                    && let Some(trigger) = jay_agent::gate::classify(&result.text)
                {
                    let asked = match trigger {
                        jay_agent::gate::Trigger::Question(q)
                        | jay_agent::gate::Trigger::Addressed(q)
                        | jay_agent::gate::Trigger::Event(q) => q,
                    };
                    let ready = last_suggestion
                        .map(|at: Instant| at.elapsed() >= cooldown)
                        .unwrap_or(true);
                    if ready {
                        last_suggestion = Some(Instant::now());
                        let _ = tx.send(asked);
                    } else {
                        tracing::debug!("gate fired but the cooldown has not elapsed");
                    }
                }

                if lines
                    .send(jay_ui::Line::transcript(
                        utterance.channel.label(),
                        result.text,
                        lag,
                    ))
                    .is_err()
                {
                    return; // nobody is listening any more
                }
            }
        })
        .context("spawning the transcription thread")?;

    // One segmenter per channel. The VAD carries recurrent state, so running
    // two speakers through a single instance would corrupt both.
    let mut mic_segmenter = SpeechSegmenter::new(Channel::Mic)?;
    let mut system_segmenter = SpeechSegmenter::new(Channel::System)?;

    let started = Instant::now();
    let unlimited = seconds == 0;

    println!("transcribing {source:?} audio with {model}.\n");

    while unlimited || started.elapsed() < Duration::from_secs(seconds) {
        let Ok(frame) = frame_rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        let segmenter = match frame.channel {
            Channel::Mic => &mut mic_segmenter,
            Channel::System => &mut system_segmenter,
        };
        if let Some(utterance) = segmenter.push(&frame)
            && utterance_tx.send(utterance).is_err()
        {
            break;
        }
    }

    // Whatever was mid-sentence when time ran out is still worth having.
    for tail in [mic_segmenter.flush(), system_segmenter.flush()]
        .into_iter()
        .flatten()
    {
        let _ = utterance_tx.send(tail);
    }
    drop(utterance_tx);
    let _ = worker.join();
    if let Some(handle) = assistant {
        // The worker owned the only question sender, so the assistant's loop
        // ends on its own now that the worker has finished.
        let _ = handle.join();
    }

    if let Some(capture) = &_mic {
        println!("\nmic dropped samples: {}", capture.dropped_samples());
    }
    if let Some(capture) = &_system {
        println!(
            "system tap: {} Hz, dropped samples: {}",
            capture.device_sample_rate(),
            capture.dropped_samples()
        );
    }
    Ok(())
}

/// One-shot suggestion, no audio involved.
fn ask(question: &str, mode: jay_agent::Mode, model: &str, screen: bool) -> Result<()> {
    // The gate runs first even here, so the thing being demonstrated is the
    // actual decision path and not a shortcut around it.
    let Some(trigger) = jay_agent::gate::classify(question) else {
        println!("the gate declined to escalate this, so it costs nothing.");
        println!("it only wakes on questions, wake phrases, or events.");
        return Ok(());
    };

    let asked = match &trigger {
        jay_agent::gate::Trigger::Question(q)
        | jay_agent::gate::Trigger::Addressed(q)
        | jay_agent::gate::Trigger::Event(q) => q.as_str(),
    };

    // Captured here, at the moment of asking, and deleted immediately after.
    let shot = if screen {
        let path = jay_agent::screen::capture(
            jay_agent::screen::Target::FocusedWindow,
            std::path::Path::new("/tmp"),
        )
        .context("capturing the screen")?;
        println!("captured {} to send with the question", path.display());
        Some(path)
    } else {
        None
    };

    let suggestion = jay_agent::claude::Claude::new(model)
        .suggest_with(mode, asked, &[], shot.as_deref())
        .context("asking claude");

    // The screenshot is somebody's work in progress. It does not linger.
    if let Some(path) = &shot {
        let _ = std::fs::remove_file(path);
    }
    let suggestion = suggestion?;

    println!("{}\n", suggestion.text);
    println!(
        "— {} · {:.1}s · ${:.4}",
        suggestion.model,
        suggestion.latency.as_secs_f32(),
        suggestion.cost_usd
    );
    Ok(())
}

fn devices() -> Result<()> {
    let names = mic::input_devices().context("enumerating input devices")?;
    if names.is_empty() {
        println!("no input devices found");
        return Ok(());
    }
    println!("input devices:");
    for name in names {
        println!("  {name}");
    }
    Ok(())
}

fn listen(
    source: Source,
    device: Option<&str>,
    seconds: u64,
    out: Option<&std::path::Path>,
) -> Result<()> {
    let (tx, rx) = crossbeam_channel::bounded::<Frame>(512);

    let mic_capture = if source.uses_mic() {
        Some(mic::start(device, tx.clone()).context("starting microphone capture")?)
    } else {
        None
    };
    let system_capture = if source.uses_system() {
        Some(jay_audio::system::start(tx.clone()).context("starting the system audio tap")?)
    } else {
        None
    };
    drop(tx);

    let started = Instant::now();
    let deadline = Duration::from_secs(seconds);
    let mut frames = 0u64;
    let mut peak_rms = 0.0f32;
    let mut worst_lag = Duration::ZERO;
    let mut last_report = Instant::now();

    println!("listening for {seconds}s. Say something.");

    while started.elapsed() < deadline {
        let Ok(frame) = rx.recv_timeout(Duration::from_millis(500)) else {
            continue;
        };
        frames += 1;
        let rms = frame.rms();
        peak_rms = peak_rms.max(rms);
        worst_lag = worst_lag.max(frame.captured_at.elapsed());

        if last_report.elapsed() >= Duration::from_millis(500) {
            last_report = Instant::now();
            let bar = "#".repeat(((rms * 200.0) as usize).min(40));
            println!("  [{}] rms {rms:>7.5}  {bar}", frame.channel.label());
        }
    }

    let expected = seconds * u64::from(SAMPLE_RATE) / jay_audio::FRAME_SAMPLES as u64;
    println!();
    println!("frames delivered : {frames} (expected roughly {expected})");
    println!("peak rms         : {peak_rms:.5}");
    println!("worst queue lag  : {worst_lag:?}");
    if let Some(capture) = &mic_capture {
        println!("mic dropped      : {}", capture.dropped_samples());
    }
    if let Some(capture) = &system_capture {
        println!(
            "system tap       : {} Hz, dropped {}",
            capture.device_sample_rate(),
            capture.dropped_samples()
        );
    }

    // Digital silence looks exactly like a working pipeline pointed at a muted
    // device, so say so rather than let a tidy frame count imply success.
    let silent = peak_rms < 1e-6;
    if silent {
        println!();
        println!("WARNING: every frame was silent. The device is delivering zeros,");
        println!("which usually means recording permission was denied or the wrong");
        println!("input is selected. Check System Settings > Privacy & Security.");
    }

    if let Some(path) = out {
        let summary = format!(
            "frames {frames} (expected ~{expected})\npeak_rms {peak_rms:.6}\n\
             worst_lag {worst_lag:?}\nsystem_rate {}\nsilent {silent}\n",
            system_capture
                .as_ref()
                .map(|c| c.device_sample_rate())
                .unwrap_or(0),
        );
        std::fs::write(path, summary)
            .with_context(|| format!("writing the summary to {}", path.display()))?;
    }

    Ok(())
}
