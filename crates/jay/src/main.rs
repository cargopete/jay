//! jay: a consented, local-first listening assistant.
//!
//! At this stage it does one thing: capture audio and prove it is real. The
//! transcript, the overlay and the agent all come later, and none of them are
//! worth building on a capture path nobody has watched produce a number.

use std::sync::atomic::Ordering::Relaxed;
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
    /// Live algorithmic interview: approach, complexity, edge case. Terse.
    Coding,
    /// Live system design interview: the missing component or tradeoff.
    SystemDesign,
    /// Mock interview debrief: what you missed, then the full worked answer.
    Rehearsal,
    /// Live pairing: concrete, short, opinionated.
    Pairing,
    /// Something went wrong: what is likely responsible and what to check.
    Dev,
}

impl From<AskMode> for jay_agent::Mode {
    fn from(mode: AskMode) -> Self {
        match mode {
            AskMode::Coding => jay_agent::Mode::Coding,
            AskMode::SystemDesign => jay_agent::Mode::SystemDesign,
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
        /// Standing context for the session: a job spec, a CV, an RFC.
        #[arg(long)]
        brief: Option<std::path::PathBuf>,
        /// Read the surrounding conversation from a file, one line per turn.
        ///
        /// Lets a recorded transcript be replayed through the real prompt
        /// path, which is the only honest way to tell whether a change to the
        /// prompts actually helps.
        #[arg(long)]
        context: Option<std::path::PathBuf>,
        /// Nudge me instead of answering: approach and complexity, no code.
        #[arg(long)]
        hint: bool,
        /// Also send what is on screen right now.
        ///
        /// Captures the focused window at the moment of asking, not
        /// continuously. Needs Screen Recording permission, which means
        /// launching jay from its .app bundle.
        #[arg(long)]
        screen: bool,
    },
    /// Open the panel with sample content and no audio.
    ///
    /// `--state` picks which moment to draw: `empty` is what you see when the
    /// session starts, `writing` is an answer mid-stream, `answered` is one
    /// finished. All three are worth looking at; only the last was, for a while.
    ///
    /// The panel is the one part of jay with no other way to check it. The
    /// tests can assert that an answer splits into prose and code; they cannot
    /// tell you the switch bank came out reading "NUDGE ANSWER GIVES", which
    /// is a thing that shipped and was obvious the moment somebody looked.
    Demo {
        /// Which moment to draw: empty, writing, or answered.
        #[arg(long, default_value = "answered")]
        state: String,
    },
    /// Check everything jay needs before a session.
    ///
    /// Run this once from the .app bundle before you sit down. Permissions on
    /// macOS fail silently — an ungranted audio tap returns silence rather
    /// than an error — so the only way to know is to try each one and look.
    Check {
        /// Write the results here as well as printing them.
        ///
        /// Needed when launched via `open -a`, which takes stdout with it.
        #[arg(short, long, default_value = "jay-check.txt")]
        out: std::path::PathBuf,
    },
    /// Assemble standing context from your memory index.
    ///
    /// Writes a brief you then edit: the generator cannot know which projects
    /// matter for the conversation you are about to have, and deleting the
    /// irrelevant ones sharpens every suggestion.
    Brief {
        /// Where to write it.
        #[arg(short, long, default_value = "jay-brief.md")]
        out: std::path::PathBuf,
        /// Root of the memory tree. Defaults to the claude-skills checkout.
        #[arg(long)]
        from: Option<std::path::PathBuf>,
        /// Keep only entries mentioning these words. Repeatable.
        ///
        /// Strongly recommended. Measured on one interview question, a
        /// six-line brief beat the full 181-project dump: the dump lost a
        /// specific point and cost two and a half times as much.
        #[arg(long = "match")]
        matches: Vec<String>,
    },
    /// Transcribe a 16 kHz mono WAV file.
    ///
    /// Useful for working through a recording after the fact, and for checking
    /// the model end to end without having to talk to your laptop.
    File {
        /// Path to the WAV file.
        path: std::path::PathBuf,
        /// Whisper model: tiny, base, small, medium or turbo.
        #[arg(short, long, default_value = "medium")]
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
        /// Whisper model: tiny, base, small, medium or turbo. Downloaded on first use.
        #[arg(short, long, default_value = "medium")]
        model: Model,
        /// How long to run for, in seconds. Zero runs until interrupted.
        #[arg(short, long, default_value_t = 60)]
        seconds: u64,
        /// Show the transcript in a floating overlay instead of the terminal.
        #[arg(long)]
        overlay: bool,
        /// What kind of help to offer when you press the button.
        #[arg(long, value_enum, default_value_t = AskMode::Pairing)]
        mode: AskMode,
        /// Stop suggesting after this many dollars. Unlimited by default.
        ///
        /// There was a $2.00 default here, on the reasoning that a runaway
        /// agent is the expensive failure mode of this whole idea. It is not
        /// the failure mode of *this* design: jay only ever spends when the
        /// button is pressed, so the ceiling is your finger. A limit that can
        /// only ever interrupt a real session mid-question is a limit worth
        /// removing.
        #[arg(long)]
        budget: Option<f64>,
        /// Standing context for the session: a job spec, a CV, an RFC.
        ///
        /// Read once at startup and sent with every suggestion. The single
        /// cheapest way to stop suggestions reading generic.
        #[arg(long)]
        brief: Option<std::path::PathBuf>,
        /// Where to write the session. Defaults to a timestamped file under
        /// the sessions directory; every run is archived either way.
        #[arg(long)]
        save: Option<std::path::PathBuf>,
        /// Extra words to expect, comma separated.
        ///
        /// Primes the transcriber. Names, product names and the jargon of this
        /// particular round are worth adding: whisper decodes conditioned on
        /// what it is told to expect, and the problem statement is the one
        /// sentence spoken exactly once.
        #[arg(long)]
        vocab: Option<String>,
    },
}

/// Leave without running the C++ static destructors.
///
/// Every exit of jay used to abort. All three crash reports on this machine are
/// identical and the assertion says exactly what it means:
///
/// ```text
/// abort <- ggml_abort <- ggml_metal_rsets_free <- ggml_metal_device_free
///       <- __cxa_finalize_ranges <- exit
/// // note: if you hit this assert, most likely you haven't deallocated all
/// // Metal resources before exiting
/// ```
///
/// ggml frees its Metal device from a static destructor at `exit`, and asserts
/// if any Metal resource is still outstanding. jay's whisper context is one:
/// under the panel it lives on a pipeline thread that is never joined, because
/// the windowing event loop owns the main thread and returns only when the
/// window closes.
///
/// Joining it is not the fix, and that is worth knowing before someone tries.
/// The terminal path *does* join its transcription thread and drop the model,
/// and it aborted too. Something in the whisper context outlives the Rust value
/// that appears to own it, so no amount of tidying on our side clears the
/// assert.
///
/// So: do not run the finalisers. Nothing of jay's depends on them. The session
/// archive is written with unbuffered `write` calls as each line arrives, and
/// the only other output is the two streams flushed immediately below.
fn quit(code: i32) -> ! {
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
    // SAFETY: `_exit` is async-signal-safe and always valid to call. It ends
    // the process without unwinding, running atexit handlers, or finalising
    // shared libraries, which is the entire point.
    unsafe { libc::_exit(code) }
}

fn main() -> ! {
    let code = match run() {
        Ok(()) => 0,
        Err(e) => {
            // Printed here rather than returned, because `quit` never gives
            // the runtime a chance to report it.
            eprintln!("Error: {e:?}");
            1
        }
    };
    quit(code)
}

fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jay=info,jay_audio=info".into()),
        )
        .with_target(false)
        .init();

    match Cli::parse().command {
        Command::Demo { state } => demo(&state),
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
            mode,
            budget,
            brief,
            save,
            vocab,
        } => transcribe(
            source,
            device,
            model,
            seconds,
            overlay,
            Assist {
                mode: mode.into(),
                budget_usd: budget,
            },
            match brief {
                Some(path) => Some(
                    std::fs::read_to_string(&path)
                        .with_context(|| format!("reading the brief at {}", path.display()))?,
                ),
                None => None,
            },
            save,
            vocab,
        ),
        Command::File { path, model } => transcribe_file(&path, model),
        Command::Brief { out, from, matches } => brief(&out, from.as_deref(), &matches),
        Command::Check { out } => check(&out),
        Command::Ask {
            question,
            mode,
            model,
            brief,
            context,
            hint,
            screen,
        } => ask(
            &question,
            mode.into(),
            &model,
            brief.as_deref(),
            context.as_deref(),
            hint,
            screen,
        ),
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
    assist: Assist,
    brief: Option<String>,
    save: Option<std::path::PathBuf>,
    vocab: Option<String>,
) -> Result<()> {
    // Everything downstream reads one line channel, so the terminal and the
    // overlay are just two consumers of the same stream rather than two paths
    // through the pipeline.
    let (line_tx, line_rx) = crossbeam_channel::unbounded::<jay_ui::Line>();
    let (request_tx, request_rx) = crossbeam_channel::bounded::<jay_ui::Request>(2);

    // Every session is archived, without being asked to. That is the whole
    // feedback loop: the most useful input this project has had was a recording
    // of a real interview, and a loop that depends on remembering a flag is a
    // loop that does not run.
    let path = save.unwrap_or_else(jay_agent::archive::new_session_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    println!("session → {}", path.display());

    // Tee the line stream to disk. Its own consumer rather than a branch in
    // each renderer, so the transcript is identical whether you ran with the
    // panel or without.
    let (line_tx, saver) = {
        {
            let (save_tx, save_rx) = crossbeam_channel::unbounded::<jay_ui::Line>();
            // Moved, not cloned. A clone would leave the original sender alive
            // in this scope — shadowed by the binding below but never dropped
            // — so the renderer's receiver would never close and the join at
            // the end would wait forever. Shadowing is not dropping.
            let forward = line_tx;
            let handle = std::thread::Builder::new()
                .name("jay-save".into())
                .spawn(move || {
                    use std::io::Write;
                    let mut file = match std::fs::File::create(&path) {
                        Ok(f) => f,
                        Err(e) => {
                            tracing::error!(%e, path = %path.display(), "could not open the transcript");
                            return;
                        }
                    };
                    let _ = writeln!(
                        file,
                        "# jay session\n\nTimes are minutes:seconds from the start.\n\n\
                         Replay any moment through the real prompt path with:\n\
                         `jay ask --mode rehearsal --context <this file> \"<the question>\"`\n"
                    );

                    // Stamped here rather than at each of the three places a
                    // line is created: every line passes through this thread,
                    // so one clock serves them all.
                    let session_started = Instant::now();

                    for line in save_rx {
                        let at = if line.at.is_zero() {
                            session_started.elapsed()
                        } else {
                            line.at
                        };
                        let clock =
                            format!("{:02}:{:02}", at.as_secs() / 60, at.as_secs() % 60);
                        let written = match line.kind {
                            jay_ui::Kind::Transcript => {
                                writeln!(file, "[{clock}] {}: {}", line.speaker, line.text)
                            }
                            jay_ui::Kind::Suggestion => writeln!(
                                file,
                                "\n[{clock}] --- jay ---\n{}\n-----------\n",
                                line.text
                            ),
                            jay_ui::Kind::Notice => writeln!(file, "[{clock}] ({})", line.text),
                            // The finished suggestion is archived; the forty
                            // drafts of it on the way there are not.
                            jay_ui::Kind::Partial => Ok(()),
                        };
                        if written.is_err() {
                            return;
                        }
                        let _ = file.flush();
                        if forward.send(line).is_err() {
                            return;
                        }
                    }
                })
                .context("spawning the transcript writer")?;
            (save_tx, Some(handle))
        }
    };

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
                        // Partials are for the panel; a terminal would just
                        // print the answer forty times as it grew.
                        jay_ui::Kind::Partial => {}
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

        // No panel means no button, so no request channel.
        let result = run_pipeline(
            source,
            device.as_deref(),
            model,
            seconds,
            line_tx,
            assist,
            None,
            brief,
            std::sync::Arc::new(jay_audio::Levels::default()),
            vocab,
        );
        if let Some(handle) = saver {
            let _ = handle.join();
        }
        let _ = printer.join();
        return result;
    }

    // Input levels, written by the capture loop and read by the panel. The
    // panel needs a reading that does not depend on whisper: between a sound
    // arriving and a sentence appearing there are about ten seconds, and for
    // those ten seconds a dead microphone looks exactly like a quiet room.
    let levels = std::sync::Arc::new(jay_audio::Levels::default());

    // The whole pipeline moves to a background thread: on macOS the windowing
    // event loop insists on the main thread and will not negotiate.
    std::thread::Builder::new()
        .name("jay-pipeline".into())
        .spawn({
            let levels = std::sync::Arc::clone(&levels);
            // Kept back so a dying pipeline can still reach the panel. This
            // used to log and nothing more, and the app bundle has no terminal
            // to log to, so the failure mode was a panel that sat there
            // looking ready for as long as you cared to watch it.
            let complaints = line_tx.clone();
            move || {
            if let Err(e) = run_pipeline(
                source,
                device.as_deref(),
                model,
                seconds,
                line_tx,
                assist,
                Some(request_rx),
                brief,
                levels,
                vocab,
            ) {
                tracing::error!(%e, "capture pipeline stopped");
                let _ = complaints.send(jay_ui::Line::notice(format!(
                    "capture stopped: {e}. Nothing more will be heard this session."
                )));
            }
            }
        })
        .context("spawning the capture pipeline")?;

    jay_ui::run(
        line_rx,
        request_tx,
        model.to_string(),
        assist.mode,
        jay_agent::Depth::default(),
        levels,
        [source.uses_mic(), source.uses_system()],
    )
        .map_err(|e| anyhow::anyhow!("overlay: {e}"))
}

/// Transcript lines retained for the session.
///
/// Generous on purpose: this is the raw record, and it is cheap. What actually
/// gets sent is chosen from it at ask time by
/// [`jay_agent::context::window`], which spends a word budget on the lines
/// that carry meaning and drops "Okay. Yeah. Right."
const CONTEXT_LINES: usize = 600;

/// Recent transcript, shared between the transcriber and the "ask" button.
type SharedHistory = std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<String>>>;

/// The problem statement, pinned for the session.
///
/// It is spoken once, at the start, and then scrolls out of a rolling window
/// while the discussion runs on for twenty minutes. jay heard it; without this
/// it forgets it exactly when the questions get specific.
type PinnedProblem = std::sync::Arc<std::sync::Mutex<Option<String>>>;

/// Words before an interviewer's utterance is taken to be the problem.
///
/// "Right, shall we start?" is not the problem. "Given a two-dimensional grid
/// of ones and zeros, count the number of islands" is.
const PROBLEM_MIN_WORDS: usize = 9;

/// How often a part-written answer is repainted.
///
/// Fast enough to read as live, slow enough that the panel is not redrawn once
/// per token for ten seconds.
const PARTIAL_PAINT_INTERVAL: Duration = Duration::from_millis(120);

/// The opening of a line, for a notice that has to identify it without
/// repeating the whole thing back.
fn first_words(text: &str, count: usize) -> String {
    let words: Vec<&str> = text.split_whitespace().take(count).collect();
    let joined = words.join(" ");
    if text.split_whitespace().count() > count {
        format!("{joined}…")
    } else {
        joined
    }
}

/// Settings for the half of jay that costs money.
#[derive(Debug, Clone, Copy)]
struct Assist {
    mode: jay_agent::Mode,
    /// Stop suggesting after this many dollars, if set at all.
    ///
    /// Soft in any case: checked before a call rather than during one, so a
    /// session overshoots by whatever was in flight.
    budget_usd: Option<f64>,
}

/// Decrements the in-flight count however the loop body exits.
///
/// There are five `continue`s in that loop and each one is an utterance that
/// will never reach the transcript. A press waiting on the count must not wait
/// for those, and a guard is the only way to be sure a branch added later does
/// not quietly leak one.
struct Departing<'a>(&'a Drain);

impl Drop for Departing<'_> {
    fn drop(&mut self) {
        self.0
            .in_flight
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Coordination between the lever and the capture loop.
///
/// `flush` is raised by the press and lowered by the capture loop once it has
/// closed its segmenters. `in_flight` counts utterances that have been queued
/// for transcription but have not yet reached the transcript.
#[derive(Debug, Default)]
struct DrainState {
    flush: std::sync::atomic::AtomicBool,
    in_flight: std::sync::atomic::AtomicUsize,
}

type Drain = std::sync::Arc<DrainState>;

/// Longest a press will wait for speech already spoken to be transcribed.
///
/// Spent only when there is something to wait for. A second of latency to
/// answer the right question beats none to answer the previous one.
const DRAIN_TIMEOUT: Duration = Duration::from_millis(1500);

/// A question plus the conversation around it.
///
/// Sending the surrounding lines is the single biggest lever on suggestion
/// quality. Given only the triggering sentence, the model has nothing to
/// ground itself in and improvises.
struct Ask {
    question: String,
    context: Vec<String>,
    /// What the switches said at the moment the lever was thrown.
    ///
    /// Carried on the question rather than read from shared state, so a mode
    /// changed while an answer is in flight cannot retroactively alter what
    /// that answer was asked for.
    mode: jay_agent::Mode,
    depth: jay_agent::Depth,
    /// Present only for hand-asked requests, where the screen at the moment of
    /// the click is very likely the thing being discussed.
    screenshot: Option<std::path::PathBuf>,
}

/// Runs suggestions on their own thread, and refuses to overspend.
fn spawn_assistant(
    assist: Assist,
    model: &str,
    brief: Option<String>,
    questions: crossbeam_channel::Receiver<Ask>,
    lines: crossbeam_channel::Sender<jay_ui::Line>,
) -> Result<std::thread::JoinHandle<()>> {
    let claude = match brief {
        Some(brief) => jay_agent::claude::Claude::new(model).with_brief(brief),
        None => jay_agent::claude::Claude::new(model),
    };
    std::thread::Builder::new()
        .name("jay-assist".into())
        .spawn(move || {
            // One process for the whole session. The first press pays the
            // ~4.7s spawn-and-preamble toll; every press after it pays about
            // 1.2s, and each one is asked of something that heard the last.
            let mut session =
                jay_agent::claude::Session::new(&claude, assist.mode, jay_agent::Depth::default());
            let mut running = (assist.mode, jay_agent::Depth::default());
            let mut spent = 0.0f64;
            for Ask {
                question,
                context,
                mode,
                depth,
                screenshot,
            } in questions
            {
                // The system prompt is fixed when the process spawns, so a
                // change of round means a new process. That costs the ~4.7s
                // startup again on the next press, which is the correct price
                // for not having to quit jay in the middle of an interview.
                if running != (mode, depth) {
                    tracing::info!(?mode, ?depth, "switches moved; new session");
                    session = jay_agent::claude::Session::new(&claude, mode, depth);
                    running = (mode, depth);
                }
                if assist.budget_usd.is_some_and(|cap| spent >= cap) {
                    // Said once, then the loop keeps draining so the sender
                    // never blocks on a full channel.
                    continue;
                }

                let started = Instant::now();
                // Throttled: the model emits deltas far faster than anyone
                // reads, and a repaint per token would spend more time drawing
                // the answer than generating it.
                let mut last_paint = Instant::now();
                let outcome = session.ask(
                    &question,
                    &context,
                    screenshot.as_deref(),
                    &mut |so_far: &str| {
                        if last_paint.elapsed() >= PARTIAL_PAINT_INTERVAL {
                            last_paint = Instant::now();
                            let _ = lines.send(jay_ui::Line::partial(so_far.to_string()));
                        }
                    },
                );
                // Somebody's work in progress. It does not linger.
                if let Some(path) = &screenshot {
                    let _ = std::fs::remove_file(path);
                }
                match outcome {
                    Ok(suggestion) => {
                        spent += suggestion.cost_usd;
                        let _ = lines.send(jay_ui::Line::suggestion(
                            suggestion.text,
                            started.elapsed(),
                        ));
                        let _ = lines.send(jay_ui::Line::notice(match assist.budget_usd {
                            Some(cap) => format!(
                                "{:.1}s · ${:.3} · ${:.2} of ${cap:.2} spent",
                                suggestion.latency.as_secs_f32(),
                                suggestion.cost_usd,
                                spent,
                            ),
                            None => format!(
                                "{:.1}s · ${:.3} · ${:.2} this session",
                                suggestion.latency.as_secs_f32(),
                                suggestion.cost_usd,
                                spent,
                            ),
                        }));
                        if assist.budget_usd.is_some_and(|cap| spent >= cap) {
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

// Ten arguments, all of them separate decisions made at the call site. They
// were briefly a struct, which moved the list somewhere else without shortening
// it.
#[allow(clippy::too_many_arguments)]
fn run_pipeline(
    source: Source,
    device: Option<&str>,
    model: Model,
    seconds: u64,
    lines: crossbeam_channel::Sender<jay_ui::Line>,
    assist: Assist,
    requests: Option<crossbeam_channel::Receiver<jay_ui::Request>>,
    brief: Option<String>,
    levels: std::sync::Arc<jay_audio::Levels>,
    vocab: Option<String>,
) -> Result<()> {
    let mut whisper = Whisper::load(model).context("loading whisper model")?;
    if let Some(vocab) = &vocab {
        // Commas are how a person writes a word list; whisper wants prose.
        whisper.prime(&vocab.replace(',', " "));
    }

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

    // Say which devices this session is actually on, in the panel and in the
    // archive, before anything else happens.
    //
    // Both of these are decided at start and never revisited: the microphone is
    // opened once, and the tap builds its aggregate device around whichever
    // output device is default at this instant. A session where the call is in
    // your headphones and the tap is on the laptop speakers hears nothing on
    // `them` and has no other way of telling you. Naming them costs one line
    // and turns that into something you can read.
    {
        let mut on = Vec::new();
        if source.uses_mic() {
            on.push(format!(
                "you: {}",
                device.unwrap_or("the default input device")
            ));
        }
        if source.uses_system() {
            on.push(format!(
                "them: whatever plays through {}",
                jay_audio::system::default_output_name()
                    .unwrap_or_else(|| "the default output device".to_string())
            ));
        }
        let _ = lines.send(jay_ui::Line::notice(format!(
            "listening on {}. Both are fixed for this session — change either \
             one and restart jay.",
            on.join(", ")
        )));
    }

    // Rolling context handed to every suggestion. Long enough to carry a
    // thread of conversation, short enough not to bloat the prompt. Shared,
    // because the "ask jay" button reads what the transcriber writes.
    let history: SharedHistory = std::sync::Arc::new(std::sync::Mutex::new(
        std::collections::VecDeque::with_capacity(CONTEXT_LINES),
    ));
    let problem: PinnedProblem = std::sync::Arc::new(std::sync::Mutex::new(None));

    // Pressing the lever asks the capture loop to close whatever utterance is
    // open, and the hand-ask thread then waits for the backlog to clear before
    // reading the transcript.
    //
    // The reason is a timing accident that would otherwise bite at the worst
    // moment: an utterance is not emitted until 600 ms of silence have passed,
    // and whisper takes another half second on top. So the sentence most
    // likely to be missing from the transcript is the one just spoken, which
    // is precisely the one being asked about.
    let drain: Drain = std::sync::Arc::new(DrainState::default());

    // jay never volunteers. Suggestions happen when you press the button and
    // at no other time: nothing is spent that you did not ask for, there are no
    // false positives to tune away, and a panel that only speaks when spoken to
    // is a panel you can forget is running.
    let settings = assist;

    let (question_tx, assistant) = match requests.is_some().then_some(settings) {
        Some(settings) => {
            let (tx, rx) = crossbeam_channel::bounded::<Ask>(4);
            let handle = spawn_assistant(
                settings,
                jay_agent::claude::DEFAULT_MODEL,
                brief.clone(),
                rx,
                lines.clone(),
            )?;
            let _ = lines.send(jay_ui::Line::notice(match settings.budget_usd {
                Some(cap) => format!(
                    "ready in {:?} mode, up to ${cap:.2} this session. I will \
                     not say anything until you press ask jay.",
                    settings.mode
                ),
                None => format!(
                    "ready in {:?} mode. I will not say anything until you \
                     press ask jay.",
                    settings.mode
                ),
            }));
            (Some(tx), Some(handle))
        }
        None => (None, None),
    };
    // Speaker attribution only works when the two voices arrive on different
    // channels. In a room they share the microphone and we cannot tell, which
    // costs the focus-picking below some accuracy but nothing else.
    let attribute_speakers = source.uses_system() && source.uses_mic();

    // Hand-asked suggestions. The most valuable trigger there is: nothing is
    // spent that was not asked for, and the screen at the moment of the click
    // is almost always the thing being discussed.
    if let (Some(tx), Some(rx)) = (question_tx.clone(), requests) {
        let settings_mode = settings.mode;
        let drain = std::sync::Arc::clone(&drain);
        let history = std::sync::Arc::clone(&history);
        let problem = std::sync::Arc::clone(&problem);
        let lines = lines.clone();
        std::thread::Builder::new()
            .name("jay-hand-ask".into())
            .spawn(move || {
                let mut mode = settings_mode;
                let mut depth = jay_agent::Depth::default();
                for request in rx {
                    match request {
                        jay_ui::Request::SetMode(next) => {
                            mode = next;
                            let _ = lines.send(jay_ui::Line::notice(format!(
                                "round: {}",
                                next.label()
                            )));
                            continue;
                        }
                        jay_ui::Request::SetDepth(next) => {
                            depth = next;
                            let _ = lines.send(jay_ui::Line::notice(format!(
                                "gives: {}",
                                next.label()
                            )));
                            continue;
                        }
                        jay_ui::Request::Suggest => {}
                    }

                    // Close anything mid-sentence and let the backlog clear,
                    // so the question being asked about is actually in the
                    // transcript by the time it is read.
                    drain.flush.store(true, Relaxed);
                    let waiting_since = Instant::now();
                    while drain.in_flight.load(Relaxed) > 0
                        && waiting_since.elapsed() < DRAIN_TIMEOUT
                    {
                        std::thread::sleep(Duration::from_millis(25));
                    }

                    let context = with_problem(&problem, &history);

                    let screenshot = match jay_agent::screen::capture(
                        jay_agent::screen::Target::default(),
                        &std::env::temp_dir(),
                    ) {
                        Ok(path) => Some(path),
                        Err(e) => {
                            // Not fatal: a suggestion from the conversation
                            // alone is worth more than no suggestion.
                            tracing::warn!(%e, "asking without a screenshot");
                            let _ = lines.send(jay_ui::Line::notice(
                                "could not capture the screen; asking from the \
                                 conversation alone"
                                    .to_string(),
                            ));
                            None
                        }
                    };

                    // Pick what to answer. The last line is often you
                    // mid-sentence, so walk back for the most recent thing the
                    // interviewer actually asked; the gate already knows how to
                    // tell a real question from "do you see the invitation?".
                    let question = context
                        .iter()
                        .rev()
                        .find_map(|line| {
                            let body = line.split_once(": ").map_or(line.as_str(), |(_, r)| r);
                            let asked_by_them = line.starts_with("them: ")
                                || line.starts_with("PROBLEM: ");
                            asked_by_them
                                .then(|| jay_agent::gate::classify(body))
                                .flatten()
                                .map(|_| body.to_string())
                        })
                        .or_else(|| context.last().cloned())
                        .unwrap_or_else(|| "(nothing has been said yet)".to_string());
                    tracing::info!(question = %question, "hand-asked");

                    // Blocking here would be less catastrophic than in the
                    // transcriber, but the button should say so rather than
                    // silently queue behind a suggestion already in flight.
                    if let Err(crossbeam_channel::TrySendError::Full(_)) = tx.try_send(Ask {
                        question,
                        context,
                        mode,
                        depth,
                        screenshot,
                    }) {
                        let _ = lines.send(jay_ui::Line::notice(
                            "already working on the last one".to_string(),
                        ));
                    }
                }
            })
            .context("spawning the hand-ask handler")?;
    }

    // Kept back before the transcription thread takes ownership, so the
    // capture loop can still reach the panel.
    let notices = lines.clone();

    let worker = std::thread::Builder::new()
        .name("jay-stt".into())
        .spawn({
            let history = std::sync::Arc::clone(&history);
            let problem = std::sync::Arc::clone(&problem);
            let drain = std::sync::Arc::clone(&drain);
            // Lives with the transcription thread, which is the only place that
            // sees both channels in the order they were transcribed.
            let mut echo = jay_agent::echo::EchoGuard::new();
            move || {
            for utterance in utterance_rx {
                // Whatever happens below — transcribed, filtered, or failed —
                // this one is no longer something a press should wait for.
                let _leaving = Departing(&drain);
                let spoken = utterance.duration();
                let result = match whisper.transcribe(&utterance.samples) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::error!(%e, "transcription failed");
                        continue;
                    }
                };

                // Lag measured from the first sample of the utterance, which
                // is the number a listener would actually notice.
                let lag = utterance.started_at.elapsed();

                tracing::debug!(
                    spoken = spoken.as_secs_f32(),
                    inference_ms = result.inference.as_secs_f32() * 1000.0,
                    rms = utterance.rms(),
                    confidence = result.confidence,
                    "transcribed"
                );

                // Every reason a transcript is binned lives in one place, so a
                // reason added later cannot be silently skipped by a branch.
                // Said out loud rather than logged: a session where everything
                // is dropped must not look like a session where nobody spoke.
                if let Some(rejected) = jay_stt::judge(&result, utterance.speech_peak) {
                    tracing::debug!(
                        text = %result.text,
                        peak = utterance.speech_peak,
                        confidence = result.confidence,
                        ?rejected,
                        "dropped a transcript"
                    );
                    let _ = lines.send(jay_ui::Line::notice(rejected.notice(spoken)));
                    continue;
                }

                // The other person's voice, back through your microphone: the
                // same sentence twice, with one copy blamed on you, spent as
                // context if nobody stops it.
                //
                // Which copy arrives first is not a fact anyone can rely on —
                // it depends on utterance length rather than on when the sound
                // happened, and both orders have been observed on this machine
                // an hour apart. So the *channel* decides, never the order. The
                // system tap did not cross a room, so it is always the one kept
                // and the microphone copy is always the one that goes.
                let label = utterance.channel.label();
                match utterance.channel {
                    // Microphone copy, interviewer's voice already recorded.
                    Channel::Mic if echo.is_echo(utterance.started_at, label, &result.text) => {
                        tracing::debug!(text = %result.text, "dropped an echo of the system channel");
                        let _ = lines.send(jay_ui::Line::notice(
                            "dropped an echo of the other channel — wear headphones".to_string(),
                        ));
                        continue;
                    }
                    // System copy, arriving after the microphone won the race.
                    // Keep this one and retract the copy already recorded.
                    Channel::System => {
                        let mut history = history.lock().expect("transcript history poisoned");
                        // `make_contiguous` rather than `as_slices().0`, which
                        // searches only up to the ring buffer's wrap point and
                        // would quietly stop finding things after 600 lines.
                        let lines_so_far = history.make_contiguous();
                        if let Some(index) =
                            echo.stale_copy(lines_so_far, Channel::Mic.label(), &result.text)
                        {
                            history.remove(index);
                            // Named rather than merely announced, because the
                            // archive is written as lines arrive and cannot be
                            // edited afterwards: the `you:` copy stays on disk
                            // even though it has left the context. Whoever
                            // reads the file back — including `jay ask --mode
                            // rehearsal` — needs to be told which line it was.
                            let _ = lines.send(jay_ui::Line::notice(format!(
                                "the earlier \"you\" copy of \"{}\" was this room, not you; \
                                 dropped from context",
                                first_words(&result.text, 8)
                            )));
                        }
                    }
                    Channel::Mic => {}
                }
                echo.remember(utterance.started_at, label, &result.text);

                // The first substantial thing the interviewer says is the
                // problem. Captured once and kept for the whole session.
                if (utterance.channel == Channel::System || !attribute_speakers)
                    && result.text.split_whitespace().count() >= PROBLEM_MIN_WORDS
                {
                    let mut pinned = problem.lock().expect("pinned problem poisoned");
                    if pinned.is_none() {
                        tracing::info!(problem = %result.text, "pinned the problem statement");
                        *pinned = Some(result.text.clone());
                    }
                }

                {
                    let line = format!("{}: {}", utterance.channel.label(), result.text);
                    let mut history = history.lock().expect("transcript history poisoned");
                    if history.len() == CONTEXT_LINES {
                        history.pop_front();
                    }
                    history.push_back(line);
                }

                // The gate runs before the line is even displayed, so a
                // question starts its (slow) escalation as early as possible.
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
            }
        })
        .context("spawning the transcription thread")?;

    // One segmenter per channel. The VAD carries recurrent state, so running
    // two speakers through a single instance would corrupt both.
    let mut mic_segmenter = SpeechSegmenter::new(Channel::Mic)?;
    let mut system_segmenter = SpeechSegmenter::new(Channel::System)?;

    let started = Instant::now();
    let unlimited = seconds == 0;
    let mut skipped_utterances = 0u64;

    // Both voices arriving on the microphone is not a fault jay can fix, and it
    // ruins everything downstream: attribution collapses, the problem statement
    // never gets pinned because pinning only fires on the system channel, and
    // the interviewer's questions are archived as things the candidate said. It
    // happens when the two people are in the same room rather than on a call.
    //
    // The meters have always shown it, reading QUIET beside a busy `you`. That
    // was not enough — a whole session was lost to it — so jay now says it in
    // words, once, after long enough that a genuine pause cannot trigger it.
    let mut system_frames = 0u64;
    let mut warned_about_one_channel = false;
    const LONELY_MIC_AFTER: Duration = Duration::from_secs(45);

    println!("transcribing {source:?} audio with {model}.\n");

    while unlimited || started.elapsed() < Duration::from_secs(seconds) {
        // Before the receive, not after it. Below this point the loop body only
        // runs when a frame actually arrives, and "no frames at all" is
        // precisely one of the states worth complaining about — a check that
        // needs a frame in order to report the absence of frames reports
        // nothing, which is how the first two versions of this failed.
        //
        // Not one frame from the tap, ever, on a session that asked for it.
        //
        // The first version of this also required thirty seconds of speech on
        // the microphone, on the reasoning that a silent session proves
        // nothing. That reasoning cost a second session: the two of them were
        // not on a call through this Mac at all, so there was nothing on the
        // tap *and* almost nothing on the mic, and the warning stayed quiet
        // through the only run where it mattered.
        //
        // Zero frames after 45 seconds is worth saying on its own. A tap
        // delivers no callbacks at all on an idle output, so this is not
        // evidence of a fault — but somebody who asked for `--source both` is
        // expecting a second person, and silence is the one thing they cannot
        // distinguish from working.
        if !warned_about_one_channel
            && source.uses_system()
            && system_frames == 0
            && started.elapsed() >= LONELY_MIC_AFTER
        {
            warned_about_one_channel = true;
            let _ = notices.send(jay_ui::Line::notice(
                "nothing has played through this Mac in 45 seconds, so the \
                 'them' channel is empty. If the other person is on a call, it \
                 is not running on this machine; if they are in the room, jay \
                 cannot tell you apart and their questions will be archived as \
                 yours."
                    .to_string(),
            ));
        }

        let Ok(frame) = frame_rx.recv_timeout(Duration::from_millis(200)) else {
            continue;
        };
        // Take the reading before segmenting, so the meter reports what
        // arrived rather than what survived the VAD.
        let meter = levels.meter(frame.channel);
        meter.record(frame.rms());

        if frame.channel == Channel::System {
            system_frames += 1;
        }

        let segmenter = match frame.channel {
            Channel::Mic => &mut mic_segmenter,
            Channel::System => &mut system_segmenter,
        };
        let utterance = segmenter.push(&frame);
        meter.set_speaking(segmenter.is_speaking());

        // A press asks for whatever is mid-sentence, now, rather than after
        // the VAD's 600 ms of silence has elapsed.
        if drain.flush.swap(false, Relaxed) {
            for pending in [mic_segmenter.flush(), system_segmenter.flush()]
                .into_iter()
                .flatten()
            {
                drain.in_flight.fetch_add(1, Relaxed);
                if utterance_tx.try_send(pending).is_err() {
                    drain.in_flight.fetch_sub(1, Relaxed);
                    skipped_utterances += 1;
                }
            }
        }

        if let Some(utterance) = utterance {
            // Same rule one layer up: whisper is slower than speech in the
            // worst case, and a stalled capture loop loses audio outright,
            // whereas a skipped utterance loses one sentence and says so.
            drain.in_flight.fetch_add(1, Relaxed);
            match utterance_tx.try_send(utterance) {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    drain.in_flight.fetch_sub(1, Relaxed);
                    skipped_utterances += 1;
                    tracing::warn!("transcription is behind; dropped an utterance");
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    drain.in_flight.fetch_sub(1, Relaxed);
                    break;
                }
            }
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
        // Drop our sender *before* joining. The assistant's loop ends when the
        // question channel closes, and this scope holds one of the senders —
        // joining first waits on a channel that can never close. That was a
        // real deadlock: `transcribe --seconds 5` ran until it was killed.
        drop(question_tx);
        let _ = handle.join();
    }

    if skipped_utterances > 0 {
        println!(
            "\n{skipped_utterances} utterance(s) skipped: transcription could not keep up"
        );
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
fn ask(
    question: &str,
    mode: jay_agent::Mode,
    model: &str,
    brief: Option<&std::path::Path>,
    context: Option<&std::path::Path>,
    hint: bool,
    screen: bool,
) -> Result<()> {
    // No gate here. The gate exists to filter speech jay merely overheard;
    // typing `jay ask` is an explicit request, exactly like pressing the
    // button in the panel. Second-guessing a direct instruction would be
    // maddening, and it declined "Count the number of islands in a grid"
    // for the entirely correct reason that it is not a question.
    let asked = question.trim();

    // Captured here, at the moment of asking, and deleted immediately after.
    let shot = if screen {
        let path = jay_agent::screen::capture(
            jay_agent::screen::Target::default(),
            std::path::Path::new("/tmp"),
        )
        .context("capturing the screen")?;
        println!("captured {} to send with the question", path.display());
        Some(path)
    } else {
        None
    };

    let history: Vec<String> = match context {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(str::to_string)
            .collect(),
        None => Vec::new(),
    };

    let mut claude = jay_agent::claude::Claude::new(model).with_depth(if hint {
        jay_agent::Depth::Hint
    } else {
        jay_agent::Depth::Full
    });
    if let Some(path) = brief {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading the brief at {}", path.display()))?;
        claude = claude.with_brief(text);
    }

    let suggestion = claude
        .suggest_with(mode, asked, &history, shot.as_deref())
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

/// Pre-flight. Tries each capability for real rather than asking macOS.
/// Peak RMS a normal speaking voice reaches at a normal distance, measured on
/// this machine. Used only to tell "working but nobody spoke" from "working".
const SPEECH_FLOOR: f32 = 0.02;

/// Collect frames for a fixed window and report how many arrived and how loud
/// the loudest was.
///
/// Deliberately returns the count as well as the level: no frames at all is a
/// different fault from frames full of zeros, and the two have different fixes.
fn drain(rx: &crossbeam_channel::Receiver<Frame>, window: Duration) -> (u64, f32) {
    let deadline = Instant::now() + window;
    let (mut frames, mut peak) = (0u64, 0.0f32);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left.min(Duration::from_millis(250))) {
            Ok(frame) => {
                frames += 1;
                peak = peak.max(frame.rms());
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }
    }
    (frames, peak)
}

/// Open the panel with one of everything in it.
///
/// No capture, no model, no spending. Meters are driven by a thread writing
/// plausible levels so the four states can be seen rather than reasoned about.
fn demo(state: &str) -> Result<()> {
    let (line_tx, line_rx) = crossbeam_channel::unbounded::<jay_ui::Line>();
    let (request_tx, request_rx) = crossbeam_channel::bounded::<jay_ui::Request>(8);
    let levels = std::sync::Arc::new(jay_audio::Levels::default());

    // Drain requests so the lever and the switches do not block on a full
    // channel, and report them, since this is also how the switches get
    // checked without a session.
    std::thread::spawn(move || {
        for request in request_rx {
            println!("  panel sent {request:?}");
        }
    });

    // Plausible levels: the microphone alive and speaking, the tap alive and
    // silent, which is what a headphoned session looks like between questions.
    std::thread::spawn({
        let levels = std::sync::Arc::clone(&levels);
        move || {
            let mut tick = 0.0f32;
            loop {
                tick += 0.12;
                levels.mic.record(0.06 + 0.05 * tick.sin().abs());
                levels.mic.set_speaking(tick.sin() > 0.0);
                levels.system.record(0.0015);
                levels.system.set_speaking(false);
                std::thread::sleep(Duration::from_millis(32));
            }
        }
    });

    // The meters tell a different story in each state, and "no frames" in
    // ember is the one that matters most, so it gets shown.
    if state == "empty" {
        let _ = line_tx.send(jay_ui::Line::notice(
            "ready in Coding mode. I will not say anything until you press ask jay."
                .to_string(),
        ));
        return jay_ui::run(
            line_rx,
            request_tx,
            "medium.en".to_string(),
            jay_agent::Mode::Coding,
            jay_agent::Depth::default(),
            levels,
            [true, true],
        )
        .map_err(|e| anyhow::anyhow!("overlay: {e}"));
    }

    for line in [
        jay_ui::Line::notice(
            "ready in Coding mode. I will not say anything until you press ask jay."
                .to_string(),
        ),
        jay_ui::Line::transcript(
            "them",
            "Given a two-dimensional grid of ones and zeros, find the maximum \
             area of an island.",
            Duration::from_millis(2400),
        ),
        jay_ui::Line::transcript(
            "you",
            "Okay. So it's connected components. I think flood fill.",
            Duration::from_millis(1900),
        ),
        jay_ui::Line::notice("dropped a known whisper artefact".to_string()),
        jay_ui::Line::transcript(
            "them",
            "How are you going to avoid counting the same island twice?",
            Duration::from_millis(2100),
        ),
    ] {
        let _ = line_tx.send(line);
    }

    if state == "writing" {
        // Half an answer, which is what the panel spends most of its ten
        // seconds drawing.
        let cut = DEMO_ANSWER.len() * 3 / 5;
        let _ = line_tx.send(jay_ui::Line::partial(DEMO_ANSWER[..cut].to_string()));
    } else {
        let _ = line_tx.send(jay_ui::Line::suggestion(
            DEMO_ANSWER.to_string(),
            Duration::from_secs(10),
        ));
        let _ = line_tx.send(jay_ui::Line::notice(
            "10.3s · $0.196 · $0.20 this session".to_string(),
        ));
    }

    jay_ui::run(
        line_rx,
        request_tx,
        "medium.en".to_string(),
        jay_agent::Mode::Coding,
        jay_agent::Depth::default(),
        levels,
        [true, true],
    )
    .map_err(|e| anyhow::anyhow!("overlay: {e}"))
}

/// A real answer, verbatim, so the panel is checked against what it will
/// actually be asked to draw rather than against a convenient short string.
const DEMO_ANSWER: &str = r#"**Approach:** a `visited` grid, or mutate the input in place: when you count a cell, zero it out so it can never be reached again.

```rust
fn max_area_of_island(mut grid: Vec<Vec<i32>>) -> i32 {
    let mut best = 0;
    for r in 0..grid.len() {
        for c in 0..grid[0].len() {
            best = best.max(fill(&mut grid, r as i32, c as i32));
        }
    }
    best
}
```

**Complexity:** O(rows x cols) time, O(rows x cols) stack in the worst case.

- Empty grid: `grid[0]` panics, guard with `if grid.is_empty()`.
- All water: every fill returns 0, so `best` stays 0.
- A snaking island fills the grid: recursion depth equals the cell count."#;

fn check(out: &std::path::Path) -> Result<()> {
    use std::fmt::Write as _;
    let mut report = String::new();

    let mut note = |line: String| {
        println!("{line}");
        let _ = writeln!(report, "{line}");
    };

    note(format!("jay {} preflight", env!("CARGO_PKG_VERSION")));
    note(String::new());

    // Microphone. This used to print OK on the strength of the device merely
    // existing, which is worthless: macOS answers a refused microphone with
    // perfect digital silence rather than an error, so the device is present
    // and named and delivers nothing. A session was lost to exactly that.
    //
    // So listen for real, and judge on the samples.
    match mic::input_devices() {
        Ok(names) if !names.is_empty() => {
            note(format!("  mic       …    listening to {}", names.join(", ")));
            let (tx, rx) = crossbeam_channel::bounded::<Frame>(256);
            match mic::start(None, tx) {
                Ok(capture) => {
                    let (frames, peak) = drain(&rx, Duration::from_secs(3));
                    let dropped = capture.dropped_samples();
                    capture.stop();
                    if frames == 0 {
                        note("  mic       FAIL device opened but delivered no frames".to_string());
                    } else if peak == 0.0 {
                        // Not a quiet room. A room has a noise floor; exact
                        // zeros across three seconds is the system saying no.
                        note(format!(
                            "  mic       FAIL {frames} frames of pure digital silence"
                        ));
                        note("            → this is a refused permission, not a quiet room".to_string());
                        note("            → System Settings › Privacy & Security › Microphone".to_string());
                    } else {
                        note(format!(
                            "  mic       OK   {frames} frames, peak {peak:.4} RMS{}",
                            if dropped > 0 {
                                format!(", {dropped} samples dropped")
                            } else {
                                String::new()
                            }
                        ));
                        if peak < SPEECH_FLOOR {
                            note(format!(
                                "            → very quiet. Speech reads about {SPEECH_FLOOR:.2};                                  say something during the check"
                            ));
                        }
                    }
                }
                Err(e) => note(format!("  mic       FAIL {e}")),
            }
        }
        Ok(_) => note("  mic       FAIL no input devices".to_string()),
        Err(e) => note(format!("  mic       FAIL {e}")),
    }

    // System audio. This one really does need the LaunchServices launch, and
    // failing here before a call is worth ten minutes of confusion during one.
    //
    // Unlike the microphone, silence here is not evidence of anything: nothing
    // may be playing. The level is reported rather than judged.
    let (tx, rx) = crossbeam_channel::bounded::<Frame>(256);
    match jay_audio::system::start(tx) {
        Ok(capture) => {
            let rate = capture.device_sample_rate();
            let (frames, peak) = drain(&rx, Duration::from_secs(3));
            note(format!(
                "  system    OK   tap at {rate} Hz, {frames} frames, peak {peak:.4} RMS"
            ));
            if peak == 0.0 {
                note("            → silent, which is expected unless something was playing".to_string());
            }
        }
        Err(e) => {
            note(format!("  system    FAIL {e}"));
            note("            → launch from the .app bundle: open -a …/jay.app --args check".to_string());
        }
    }

    // Screen. A separate permission from audio, and just as silent.
    match jay_agent::screen::capture(jay_agent::screen::Target::default(), &std::env::temp_dir()) {
        Ok(path) => {
            let bytes = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let _ = std::fs::remove_file(&path);
            note(format!("  screen    OK   captured {} KB", bytes / 1024));
        }
        Err(e) => {
            note(format!("  screen    FAIL {e}"));
            note("            → System Settings › Privacy › Screen & System Audio Recording".to_string());
        }
    }

    // Whisper weights: a 466 MB download is not something to discover at the
    // start of a forty-minute session.
    match jay_stt::models::ensure(Model::default()) {
        Ok(path) => note(format!("  whisper   OK   {}", path.display())),
        Err(e) => note(format!("  whisper   FAIL {e}")),
    }

    // The subscription. Cheap, and proves the CLI is authenticated.
    note("  claude    …    asking for one word".to_string());
    match jay_agent::claude::Claude::new("claude-haiku-4-5")
        .with_depth(jay_agent::Depth::Hint)
        .suggest(jay_agent::Mode::Coding, "Reply with the single word: ready", &[])
    {
        Ok(s) => note(format!(
            "  claude    OK   {:.1}s, ${:.4} — cache is now warm",
            s.latency.as_secs_f32(),
            s.cost_usd
        )),
        Err(e) => note(format!("  claude    FAIL {e}")),
    }

    std::fs::write(out, &report)
        .with_context(|| format!("writing {}", out.display()))?;
    Ok(())
}

/// Assemble a starting brief from the memory index.
fn brief(
    out: &std::path::Path,
    from: Option<&std::path::Path>,
    matches: &[String],
) -> Result<()> {
    let root = match from {
        Some(path) => path.to_path_buf(),
        None => jay_agent::brief::default_memory_root().context(
            "could not find a memory tree. Pass --from <dir>, pointing at a \
             directory containing MEMORY.md index files",
        )?,
    };

    let (markdown, count) = jay_agent::brief::assemble(&root, matches)?;
    std::fs::write(out, &markdown)
        .with_context(|| format!("writing {}", out.display()))?;

    println!("{count} projects from {} → {}", root.display(), out.display());
    let words = markdown.split_whitespace().count();
    println!("roughly {words} words.");
    if matches.is_empty() && count > 30 {
        println!();
        println!("That is almost certainly too much. On one measured interview");
        println!("question a six-line brief beat the full dump: the dump lost a");
        println!("specific point and cost 2.5x as much. Narrow it, e.g.");
        println!("  jay brief --match indexer --match gateway --match rust");
    }
    println!("Then fill in the 'Who you are' section — that part changes the");
    println!("answers most, and no generator can write it for you.");
    Ok(())
}

/// The rolling window, with the pinned problem at its head.
fn with_problem(problem: &PinnedProblem, history: &SharedHistory) -> Vec<String> {
    let mut context = Vec::with_capacity(CONTEXT_LINES + 1);
    if let Some(text) = problem.lock().expect("pinned problem poisoned").as_ref() {
        context.push(format!("PROBLEM: {text}"));
    }
    let all: Vec<String> = history
        .lock()
        .expect("transcript history poisoned")
        .iter()
        .cloned()
        .collect();
    context.extend(jay_agent::context::window(
        &all,
        jay_agent::context::WORD_BUDGET,
    ));
    context
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

    // Once per channel. Counting frames across both while expecting one
    // channel's worth made a perfectly healthy `--source both` run report a 75%
    // over-delivery, which reads as a fault rather than as arithmetic.
    let channels = u64::from(mic_capture.is_some()) + u64::from(system_capture.is_some());
    let expected =
        channels * seconds * u64::from(SAMPLE_RATE) / jay_audio::FRAME_SAMPLES as u64;
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The transcription loop has five `continue`s, and every one of them is
    /// an utterance that will never reach the transcript. A press waiting on
    /// the backlog must not wait for those. This asserts the guard covers
    /// every exit path, including ones added later.
    #[test]
    fn every_utterance_leaves_the_backlog_however_the_loop_exits() {
        let drain: Drain = std::sync::Arc::new(DrainState::default());
        drain.in_flight.fetch_add(4, Relaxed);

        for reason in 0..4 {
            let _leaving = Departing(&drain);
            match reason {
                0 => continue,          // filtered as an artefact
                1 => continue,          // dropped as too quiet
                2 => {}                 // transcribed normally
                _ => continue,          // whichever branch comes next
            }
        }

        assert_eq!(
            drain.in_flight.load(Relaxed),
            0,
            "a press would wait {}ms for utterances that are never coming",
            DRAIN_TIMEOUT.as_millis()
        );
    }

    /// A press with nothing in flight must not pay the drain timeout.
    #[test]
    fn an_empty_backlog_costs_a_press_nothing() {
        let drain: Drain = std::sync::Arc::new(DrainState::default());
        let started = Instant::now();
        while drain.in_flight.load(Relaxed) > 0 && started.elapsed() < DRAIN_TIMEOUT {
            std::thread::sleep(Duration::from_millis(25));
        }
        assert!(started.elapsed() < Duration::from_millis(50));
    }
}
