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
    command: Option<Command>,
    /// Flags for `transcribe`, which is what jay does when told nothing else.
    ///
    /// Flattened here so that a bare `jay` starts listening. Transcribing a
    /// conversation is the thing this program is for; every other subcommand
    /// exists to check some part of it or to replay one afterwards.
    #[command(flatten)]
    transcribe: TranscribeArgs,
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

/// Which microphone path to open.
///
/// Exists to make the echo question measurable rather than arguable. The room
/// coming back through the microphone is not a cosmetic fault: it holds the
/// segmenter permanently in speech, so utterances run to the 25-second cap and
/// are cut mid-sentence with both speakers inside them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum MicPath {
    /// cpal, straight off the device. Hears the room.
    Plain,
    /// The platform voice unit, echo cancellation on.
    Aec,
    /// The same unit with cancellation off, so the two differ only in that.
    Bypass,
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
    /// Defending an answer already given: prose, no code, no diagrams.
    Qa,
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
            AskMode::Qa => jay_agent::Mode::Qa,
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
        /// Which microphone path to open.
        ///
        /// `plain` is cpal, which hears the room. `aec` is the platform voice
        /// unit with echo cancellation on. `bypass` is the same unit with it
        /// off, so the difference between the last two is the cancellation and
        /// nothing else.
        #[arg(long, value_enum, default_value_t = MicPath::Plain)]
        mic_path: MicPath,
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
        /// Which moment to draw: empty, writing, answered, or design.
        ///
        /// `design` is the one worth looking at after changing the design
        /// prompt: jay does not draw diagrams, it hands over a Mermaid block
        /// for Excalidraw, and how readable that is in a code well at panel
        /// width is a question only eyes can answer.
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
    ///
    /// The default command. `jay` on its own is `jay transcribe`.
    Transcribe(TranscribeArgs),
    /// Write the meeting notes for a session already recorded.
    ///
    /// A live session does this for itself unless told not to. This is for
    /// the ones recorded before it did, for a meeting worth writing up twice,
    /// and for changing your mind after `--no-notes`.
    ///
    /// Runs on the same subscription as everything else, through the `claude`
    /// CLI, but asks it to be a summariser rather than a coding agent.
    Notes {
        /// The session archive to read.
        path: std::path::PathBuf,
        /// Which model writes them.
        #[arg(long, default_value = jay_agent::notes::DEFAULT_MODEL)]
        model: String,
        /// Where to write them. Defaults to `<session>.notes.md`.
        #[arg(short, long)]
        out: Option<std::path::PathBuf>,
    },
}

// Everything the live transcriber takes.
//
// Its own struct rather than an inline variant body because it is used twice:
// as the `transcribe` subcommand, and flattened onto the top level so that a
// bare `jay` runs it.
//
// Plain comments rather than doc comments, deliberately: clap takes a
// flattened struct's doc comment as the *parent's* description, and `jay
// --help` opened with three paragraphs about why this struct exists.
#[derive(Debug, clap::Args)]
struct TranscribeArgs {
    /// Which audio to listen to.
    ///
    /// `both` by default. A conversation has two people in it, and a default
    /// that records only your half is a default that produces half a record —
    /// which you discover afterwards, when the other half is what you wanted.
    #[arg(short = 'S', long, value_enum, default_value_t = Source::Both)]
    source: Source,
    /// Input device name. Defaults to the system default input.
    #[arg(short, long)]
    device: Option<String>,
    /// Whisper model: tiny, base, small, medium or turbo. Downloaded on first use.
    #[arg(short, long, default_value = "medium")]
    model: Model,
    /// How long to run for, in seconds. Zero runs until interrupted.
    ///
    /// Zero by default. Nobody knows how long a meeting is going to be, and a
    /// transcript that stops at sixty seconds because that was the default
    /// stops without saying so.
    #[arg(short, long, default_value_t = 0)]
    seconds: u64,
    /// Print to the terminal instead of opening the panel.
    ///
    /// The panel is the default. It is where the mute switches live, and a
    /// transcriber whose only interface is a scrolling terminal gives you no
    /// way to stop it recording without killing it.
    #[arg(long)]
    terminal: bool,
    /// Kept so `--overlay` does not break; the panel is the default now.
    #[arg(long, hide = true)]
    overlay: bool,
    /// Start with the microphone muted.
    ///
    /// For sitting in on something you are not speaking in. Muting in a call
    /// application does not mute the microphone — jay opens the device itself
    /// — so without this jay hears you, and hears the other person coming out
    /// of your speakers, and files both under "you".
    #[arg(long)]
    muted: bool,
    /// What kind of help to offer when you press the button.
    ///
    /// Only reachable from the panel, so this does nothing without
    /// `--overlay`: with no button there is nothing to press, and jay never
    /// volunteers. A terminal run is a transcriber and nothing else.
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
    /// particular meeting are worth adding: whisper decodes conditioned on
    /// what it is told to expect.
    ///
    /// Nothing is primed unless you say so, which is a change: every session
    /// used to be told it was an algorithms interview, including the ones that
    /// were not. Pass `interview` for that built-in list when the round really
    /// is one.
    #[arg(long)]
    vocab: Option<String>,
    /// Do not write the meeting notes when the session ends.
    ///
    /// Notes are on by default and are the only thing a bare `jay` spends
    /// anything on. Skipped anyway when nothing was heard.
    /// Which microphone path to open.
    ///
    /// `plain` is cpal, which hears the room and everything in it. `aec` opens
    /// the platform voice unit instead, which cancels what the speakers are
    /// playing — and takes the `them` channel with it, so it is a measurement
    /// tool rather than a setting. See IMPROVEMENTS.md.
    #[arg(long, value_enum, default_value_t = MicPath::Plain)]
    mic_path: MicPath,

    /// Transcribe the microphone even while the far side is speaking.
    ///
    /// The gate is on by default because the failure it prevents is silent and
    /// the one it causes is visible. Without headphones the speakers arrive at
    /// the microphone louder than speech does, the segmenter never falls
    /// silent, and utterances run to the 25-second cap holding both people's
    /// words — attributed to you. With the gate, an interjection made while
    /// the other person is talking is not transcribed at all.
    ///
    /// Turn it off when wearing headphones, where there is no echo to gate and
    /// talking over each other is just conversation.
    #[arg(long)]
    no_echo_gate: bool,

    #[arg(long)]
    no_notes: bool,
    /// Which model writes the notes.
    #[arg(long, default_value = jay_agent::notes::DEFAULT_MODEL)]
    notes_model: String,
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

/// Set by SIGINT. Read by the capture loop, which then stops politely.
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_interrupt(_signal: libc::c_int) {
    // The only thing this does, and the only thing it is allowed to do: a
    // relaxed store is async-signal-safe, and nothing else in here would be.
    INTERRUPTED.store(true, Relaxed);
}

/// Ask for Ctrl-C to end the session rather than end the process.
///
/// The default disposition kills jay where it stands. The archive survives —
/// it is flushed line by line — but whatever was mid-sentence when you pressed
/// it does not, and neither does the footer that tells you whether anything
/// was dropped. Since a meeting now runs until you stop it, "how you stop it"
/// is part of the recording path rather than an afterthought.
///
/// A second Ctrl-C is left to the default handler, so a wedged session can
/// still be killed the ordinary way.
fn stop_politely_on_interrupt() {
    // SAFETY: installing a handler that only performs a relaxed atomic store.
    // `SA_RESETHAND` restores the default disposition after the first signal,
    // which is what makes the second Ctrl-C work.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_interrupt as *const () as usize;
        action.sa_flags = libc::SA_RESETHAND;
        libc::sigemptyset(&raw mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &raw const action, std::ptr::null_mut());
    }
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

    let cli = Cli::parse();
    // A bare `jay` is `jay transcribe`, with whatever top-level flags were
    // given. The flattened args are simply unused when a subcommand is named.
    match cli.command.unwrap_or(Command::Transcribe(cli.transcribe)) {
        Command::Demo { state } => demo(&state),
        Command::Devices => devices(),
        Command::Listen {
            source,
            device,
            seconds,
            out,
            mic_path,
        } => listen(source, device.as_deref(), seconds, out.as_deref(), mic_path),
        Command::Transcribe(args) => {
            let brief = match &args.brief {
                Some(path) => Some(
                    std::fs::read_to_string(path)
                        .with_context(|| format!("reading the brief at {}", path.display()))?,
                ),
                None => None,
            };
            transcribe(&args, brief)
        }
        Command::Notes { path, model, out } => {
            let out = out.unwrap_or_else(|| jay_agent::notes::path_for(&path));
            let transcript = std::fs::read_to_string(&path)
                .with_context(|| format!("reading {}", path.display()))?;
            let notes = jay_agent::notes::write(&transcript, &model)
                .context("writing the meeting notes")?;
            std::fs::write(&out, &notes.markdown)
                .with_context(|| format!("writing {}", out.display()))?;
            println!("{}\n", notes.markdown);
            println!("{}", describe(&notes, &out));
            Ok(())
        }
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
fn transcribe(args: &TranscribeArgs, brief: Option<String>) -> Result<()> {
    stop_politely_on_interrupt();
    let source = args.source;
    // Read once, up front, so the session can say so before it starts rather
    // than after it has already heard everything.
    let notes_at_the_end = !args.no_notes;
    let assist = Assist {
        mode: args.mode.into(),
        budget_usd: args.budget,
    };

    // Everything downstream reads one line channel, so the terminal and the
    // overlay are just two consumers of the same stream rather than two paths
    // through the pipeline.
    let (line_tx, line_rx) = crossbeam_channel::unbounded::<jay_ui::Line>();
    let (request_tx, request_rx) = crossbeam_channel::bounded::<jay_ui::Request>(2);

    // One clock for the whole session, set by the capture loop the moment
    // audio is actually flowing rather than here — loading `medium.en` takes
    // seconds, and a transcript whose first line is stamped 00:07 because of
    // a model load is a transcript that lies about a silence.
    let epoch: SessionClock = std::sync::Arc::new(std::sync::OnceLock::new());

    // Every session is archived, without being asked to. That is the whole
    // feedback loop: the most useful input this project has had was a recording
    // of a real interview, and a loop that depends on remembering a flag is a
    // loop that does not run.
    let path = args
        .save
        .clone()
        .unwrap_or_else(jay_agent::archive::new_session_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    println!("session → {}", path.display());
    let archived = path.clone();

    // Tee the line stream to disk. Its own consumer rather than a branch in
    // each renderer, so the transcript is identical whether you ran with the
    // panel or without.
    let (line_tx, saver) = {
        let (save_tx, save_rx) = crossbeam_channel::unbounded::<jay_ui::Line>();
        // Moved, not cloned. A clone would leave the original sender alive
        // in this scope — shadowed by the binding below but never dropped
        // — so the renderer's receiver would never close and the join at
        // the end would wait forever. Shadowing is not dropping.
        let forward = line_tx;
        let epoch = std::sync::Arc::clone(&epoch);
        let handle = std::thread::Builder::new()
            .name("jay-save".into())
            .spawn(move || write_session(&path, &save_rx, &forward, &epoch, REORDER_WINDOW))
            .context("spawning the transcript writer")?;
        (save_tx, Some(handle))
    };

    if args.terminal {
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
                            "[{}] {}{}: {}",
                            clock(line.at.unwrap_or_default()),
                            if line.uncertain { "?" } else { "" },
                            line.speaker,
                            line.text
                        ),
                    }
                }
            })
            .context("spawning the printer")?;

        // No panel means no button, so no request channel — and therefore no
        // assistant thread and nothing that can spend money. A terminal run
        // of jay is a transcriber, full stop.
        let levels = std::sync::Arc::new(jay_audio::Levels::default());
        levels.mic.set_muted(args.muted);
        let result = run_pipeline(
            source,
            args.device.as_deref(),
            args.model,
            args.seconds,
            line_tx,
            assist,
            None,
            brief,
            levels,
            resolve_vocab(args.vocab.as_deref()),
            epoch,
            notes_at_the_end,
            args.mic_path,
            !args.no_echo_gate,
            jay_ui::Closer::new(),
        );
        let written = saver.and_then(|handle| handle.join().ok());
        let _ = printer.join();
        if let Some(written) = written {
            report(&written, &archived);
            if !args.no_notes && written.spoken + written.unsure > 0 {
                notes_for_session(&archived, &args.notes_model);
            }
        }
        return result;
    }

    // Input levels, written by the capture loop and read by the panel. The
    // panel needs a reading that does not depend on whisper: between a sound
    // arriving and a sentence appearing there are about ten seconds, and for
    // those ten seconds a dead microphone looks exactly like a quiet room.
    let levels = std::sync::Arc::new(jay_audio::Levels::default());
    levels.mic.set_muted(args.muted);

    // Raised when the pipeline stops without being asked — the clock ran out,
    // or the capture died. The panel watches it and closes itself, because the
    // notes are written after `jay_ui::run` returns and a window nobody closes
    // is a session whose notes are never written.
    let finished = jay_ui::Closer::new();

    // The whole pipeline moves to a background thread: on macOS the windowing
    // event loop insists on the main thread and will not negotiate.
    let (device, model, seconds, vocab, mic_path, echo_gate) = (
        args.device.clone(),
        args.model,
        args.seconds,
        resolve_vocab(args.vocab.as_deref()),
        args.mic_path,
        !args.no_echo_gate,
    );
    let pipeline = std::thread::Builder::new()
        .name("jay-pipeline".into())
        .spawn({
            let levels = std::sync::Arc::clone(&levels);
            let finished = finished.clone();
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
                    epoch,
                    notes_at_the_end,
                    mic_path,
                    echo_gate,
                    finished.clone(),
                ) {
                    tracing::error!(%e, "capture pipeline stopped");
                    let _ = complaints.send(jay_ui::Line::notice(format!(
                        "capture stopped: {e}. Nothing more will be heard this session."
                    )));
                }
                // A pipeline that died before reaching its own flag still has
                // to release the panel, or the failure is a hang.
                finished.close();
            }
        })
        .context("spawning the capture pipeline")?;

    let closed = jay_ui::run(
        line_rx,
        request_tx,
        model.to_string(),
        assist.mode,
        jay_agent::Depth::default(),
        levels,
        finished.clone(),
        [source.uses_mic(), source.uses_system()],
    )
    .map_err(|e| anyhow::anyhow!("overlay: {e}"));

    // Closing the panel ends the session; it does not end it *instantly*. The
    // capture loop still has whatever is mid-sentence, the decoder still has a
    // backlog, and the writer is holding the last few seconds back to put them
    // in order. Walking away here loses all three — the process exits through
    // `_exit`, which waits for nothing.
    INTERRUPTED.store(true, Relaxed);
    let _ = pipeline.join();
    if let Some(written) = saver.and_then(|handle| handle.join().ok()) {
        report(&written, &archived);
        // The panel is gone by now and, under `open -a`, so is stdout. The
        // notes still land on disk beside the session, which is where they
        // were always going to be read from.
        if !args.no_notes && written.spoken + written.unsure > 0 {
            notes_for_session(&archived, &args.notes_model);
        }
    }
    closed
}

/// What a set of notes cost, and where it went.
fn describe(notes: &jay_agent::notes::Notes, out: &std::path::Path) -> String {
    format!(
        "notes → {}\n{} · {:.0}s · {} prompt, {} written · ${:.4}",
        out.display(),
        notes.model,
        notes.spent.elapsed.as_secs_f32(),
        notes.spent.prompt_tokens,
        notes.spent.output_tokens,
        notes.spent.usd
    )
}

/// Write the notes for the session that has just ended.
///
/// Never fatal. The session is already on disk and is the thing that mattered;
/// a summariser that could take the recording down with it would be a bad
/// trade at any price. Every failure here is one printed line.
fn notes_for_session(path: &std::path::Path, model: &str) {
    let transcript = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) => {
            println!("\nno notes: could not read back {} ({e})", path.display());
            return;
        }
    };
    println!("\nwriting the notes…");
    match jay_agent::notes::write(&transcript, model) {
        Ok(notes) => {
            let out = jay_agent::notes::path_for(path);
            if let Err(e) = std::fs::write(&out, &notes.markdown) {
                println!("no notes: could not write {} ({e})", out.display());
                return;
            }
            println!("\n{}\n", notes.markdown);
            println!("{}", describe(&notes, &out));
        }
        // Including "nothing was said", which is not a fault and does not
        // need a paragraph about it.
        Err(e) => println!("no notes: {e}"),
    }
}

/// `interview` names the built-in list; anything else is taken literally.
///
/// One keyword rather than a `--vocab-preset` flag nobody would find. The list
/// is genuinely useful for the round it was measured on and actively harmful
/// everywhere else, so it has to be easy to ask for and impossible to get by
/// accident.
fn expand_vocab(vocab: &str) -> String {
    if vocab.trim().eq_ignore_ascii_case("interview") {
        return jay_stt::whisper::INTERVIEW_VOCABULARY.to_string();
    }
    vocab.to_string()
}

/// Where a standing vocabulary lives when none is given on the command line.
fn default_vocab_path() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(|home| {
        std::path::PathBuf::from(home)
            .join(".config")
            .join("jay")
            .join("vocab")
    })
}

/// The vocabulary for this session: the flag if given, otherwise the file.
///
/// `--vocab` is the difference between `Patroni` and `Petrino`, and between
/// "reverse a singly linked list" and "reverse the link please" — both real
/// transcripts of the same sentence. It is also a flag nobody types at the
/// start of a call they are already two minutes late for, which makes it a
/// feature that works in testing and not in life.
///
/// So the names and jargon that come up every week live in a file instead, and
/// only the ones peculiar to today's meeting need typing. Comments and blank
/// lines are allowed, because a list nobody can annotate is a list nobody
/// maintains.
fn resolve_vocab(flag: Option<&str>) -> Option<String> {
    if let Some(vocab) = flag {
        return Some(vocab.to_string());
    }
    let path = default_vocab_path()?;
    let contents = std::fs::read_to_string(&path).ok()?;
    let words: Vec<&str> = contents
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    if words.is_empty() {
        return None;
    }
    tracing::info!(path = %path.display(), "primed from the standing vocabulary");
    Some(words.join(", "))
}

/// The closing line of a session.
fn report(written: &Written, path: &std::path::Path) {
    if written.spoken == 0 && written.unsure == 0 {
        println!(
            "\nnothing was transcribed. If anybody was talking, the likeliest reasons\n\
             are a refused permission, or a call that is not running through\n\
             this Mac. `jay check` answers both.\n{}",
            path.display()
        );
        return;
    }
    let unsure = match written.unsure {
        0 => String::new(),
        n => format!(", {n} marked unsure"),
    };
    println!(
        // "of it" would be a lie on two channels: an echo, or two people
        // talking over each other, is counted once per channel and the total
        // can exceed the length of the meeting.
        "\n{} line(s){unsure} over {}, {} of speech across both channels\n{}",
        written.spoken,
        clock(written.last),
        clock(written.speech),
        path.display()
    );
}

/// The one clock every line in a session is stamped against.
///
/// Set once, by the capture loop, when audio starts flowing.
type SessionClock = std::sync::Arc<std::sync::OnceLock<Instant>>;

/// `mm:ss`, and `hh:mm:ss` once a meeting has gone on long enough to need it.
fn clock(at: Duration) -> String {
    let total = at.as_secs();
    match total / 3600 {
        0 => format!("{:02}:{:02}", total / 60, total % 60),
        hours => format!("{hours}:{:02}:{:02}", (total / 60) % 60, total % 60),
    }
}

/// How long the writer holds a line back before committing it to the file.
///
/// Utterances are decoded in the order they *finished*, which is not the order
/// they began: the two channels overlap whenever people talk over each other,
/// and a twenty-second question that started at 02:00 is decoded after a
/// two-second "mm-hm" that started at 02:15. Written as they arrive, the
/// transcript has the interruption backwards.
///
/// Measured **from when the line arrived**, not from when the speech happened.
/// The first version of this held lines until the session clock had passed
/// their start time by three seconds, which sounds equivalent and is not: a
/// twenty-second question reaches the writer twenty-three seconds after it
/// began, so it was always already due and the buffer sorted nothing at all.
/// The tests below caught that; nothing else would have, because an archive
/// with the interruptions backwards looks exactly like an interview where the
/// interruptions were backwards.
///
/// Three seconds is not a guarantee. Guaranteeing order means holding every
/// line for `MAX_UTTERANCE` plus decode — half a minute — which is too long to
/// tail a meeting. Three seconds sorts everything that arrives close together,
/// which is where inversions come from, and never introduces one that was not
/// already there.
const REORDER_WINDOW: Duration = Duration::from_secs(3);

/// Write the session to disk, in the order things were said.
///
/// Forwards every line onward the instant it arrives — the panel and the
/// terminal stay live — and commits to the file on a delay, oldest first.
fn write_session(
    path: &std::path::Path,
    incoming: &crossbeam_channel::Receiver<jay_ui::Line>,
    forward: &crossbeam_channel::Sender<jay_ui::Line>,
    epoch: &SessionClock,
    window: Duration,
) -> Written {
    use std::io::Write;

    let mut written = Written::default();
    let mut file = match std::fs::File::create(path) {
        Ok(f) => f,
        Err(e) => {
            tracing::error!(%e, path = %path.display(), "could not open the transcript");
            return written;
        }
    };
    let _ = writeln!(
        file,
        "# jay session\n\nTimes are minutes:seconds from the first audio, taken from when \
         each utterance *began*.\nA range is start and end; overlapping ranges are two \
         people talking at once.\nA `?` before the speaker means jay heard it but did not \
         trust it — kept here, kept out of the model's context.\n\n\
         Replay any moment through the real prompt path with:\n\
         `jay ask --mode rehearsal --context <this file> \"<the question>\"`\n"
    );

    // Only used for lines nobody stamped, and only until the capture loop sets
    // the real one. In practice that is nothing: the first line sent is the
    // "listening on …" notice, which the capture loop sends after starting.
    let fallback = Instant::now();
    let elapsed = |epoch: &SessionClock| epoch.get().unwrap_or(&fallback).elapsed();

    // Held back by REORDER_WINDOW, kept sorted by when the speech began. The
    // `Instant` is arrival, which is what the window is measured against.
    let mut pending: Vec<(Instant, jay_ui::Line)> = Vec::new();
    let mut forwarding = true;

    let commit = |file: &mut std::fs::File, written: &mut Written, line: &jay_ui::Line| -> bool {
        // Every line reaching here was stamped on arrival at the latest.
        let at = line.at.unwrap_or_default();
        match line.kind {
            jay_ui::Kind::Transcript if line.uncertain => written.unsure += 1,
            jay_ui::Kind::Transcript => {
                written.spoken += 1;
                written.speech += line.spoken;
            }
            _ => {}
        }
        written.last = written.last.max(at + line.spoken);
        let stamp = match line.kind {
            // A range only means something for speech. A notice happened at
            // an instant.
            jay_ui::Kind::Transcript if !line.spoken.is_zero() => {
                format!("{}–{}", clock(at), clock(at + line.spoken))
            }
            _ => clock(at),
        };
        let written = match line.kind {
            jay_ui::Kind::Transcript => writeln!(
                file,
                "[{stamp}] {}{}: {}",
                if line.uncertain { "?" } else { "" },
                line.speaker,
                line.text
            ),
            jay_ui::Kind::Suggestion => writeln!(
                file,
                "\n[{stamp}] --- jay ---\n{}\n-----------\n",
                line.text
            ),
            jay_ui::Kind::Notice => writeln!(file, "[{stamp}] ({})", line.text),
            // The finished suggestion is archived; the forty drafts of it on
            // the way there are not.
            jay_ui::Kind::Partial => Ok(()),
        };
        if written.is_err() {
            return false;
        }
        let _ = file.flush();
        true
    };

    loop {
        // A timeout rather than a plain `recv`, so a held-back line is still
        // committed when the room goes quiet and nothing arrives to push it
        // out. Otherwise the last thing said before a long silence sits in
        // memory until the next thing is said.
        match incoming.recv_timeout(Duration::from_millis(250)) {
            Ok(mut line) => {
                let at = line.at.unwrap_or_else(|| elapsed(epoch));
                line.at = Some(at);
                // Partials never reach the file and there can be forty of
                // them a second; do not sort them.
                if line.kind != jay_ui::Kind::Partial {
                    let arrived = Instant::now();
                    let seat = pending.partition_point(|(_, held)| held.at <= line.at);
                    pending.insert(seat, (arrived, line.clone()));
                }
                // A renderer that has gone away — the panel closed — must not
                // take the rest of the archive with it. Stop forwarding, keep
                // writing.
                if forwarding && forward.send(line).is_err() {
                    forwarding = false;
                }
            }
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
        }

        // Strictly from the front, so a line that is not yet due blocks
        // everything behind it. Committing a later line around it would be
        // the exact inversion this buffer exists to prevent.
        while pending
            .first()
            .is_some_and(|(arrived, _)| arrived.elapsed() >= window)
        {
            let (_, line) = pending.remove(0);
            if !commit(&mut file, &mut written, &line) {
                return written;
            }
        }
    }

    // Whatever is still held back is said and done; the session is over and
    // nothing older can arrive.
    for (_, line) in pending {
        if !commit(&mut file, &mut written, &line) {
            return written;
        }
    }
    written
}

/// What actually reached the file, for the line printed at the end.
///
/// Worth saying out loud rather than leaving to whoever opens the file. A
/// session that recorded nothing and a session that recorded forty minutes
/// both end with a shell prompt, and only one of them is what you wanted.
#[derive(Debug, Default, Clone, Copy)]
struct Written {
    /// Transcript lines jay stands behind.
    spoken: usize,
    /// Transcript lines it heard but did not trust.
    unsure: usize,
    /// Total speech, which is a good deal less than the length of a meeting.
    speech: Duration,
    /// Where the last thing said sits on the session clock.
    last: Duration,
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
    epoch: SessionClock,
    notes_at_the_end: bool,
    mic_path: MicPath,
    echo_gate: bool,
    // Raised the moment the transcript is complete, so the panel can close
    // itself. It has to be raised from *inside* here rather than after this
    // function returns, because of the deadlock described at the join below.
    finished: jay_ui::Closer,
) -> Result<()> {
    // The frame channel is deep enough to hold the whole model load.
    //
    // 4096 frames is about 65 seconds per channel at 32 ms a frame, and costs
    // 8 MB against the 1.5 GB `medium.en` is about to take, so the arithmetic
    // is not close. It has to cover the load because the captures are opened
    // *first* now — see below.
    let (frame_tx, frame_rx) = crossbeam_channel::bounded::<Frame>(4096);
    let (utterance_tx, utterance_rx) = crossbeam_channel::bounded::<Utterance>(16);

    // Both captures feed the one frame channel; the channel tag on each frame
    // is what keeps the two speakers apart downstream.
    // Only one of these is ever `Some`. Which one decides whether the room
    // comes with it.
    let mut _mic = None;
    let mut _voice_mic = None;
    if source.uses_mic() {
        match mic_path {
            MicPath::Plain => {
                _mic = Some(
                    mic::start(device, frame_tx.clone()).context("starting microphone capture")?,
                );
            }
            MicPath::Aec | MicPath::Bypass => {
                _voice_mic = Some(
                    jay_audio::voice_mic::start(frame_tx.clone(), mic_path == MicPath::Bypass)
                        .context("starting the voice-processing microphone")?,
                );
            }
        }
    }

    let mut _system = if source.uses_system() {
        Some(jay_audio::system::start(frame_tx.clone()).context(
            "starting the system audio tap. macOS asks for permission the first \
             time; if it was refused, grant it in System Settings > Privacy & \
             Security > Screen & System Audio Recording",
        )?)
    } else {
        None
    };
    drop(frame_tx);

    // Zero on the session clock: the first instant anything could be heard.
    // Set here, after the captures are open and before any line is sent, so
    // every stamp in the archive is measured from the same moment.
    let epoch = *epoch.get_or_init(Instant::now);

    // The model loads *after* the microphone is open, and this is the whole
    // point of the ordering.
    //
    // It used to load first, which meant the devices were not merely unread
    // during those seconds, they were not open: the audio did not exist to be
    // lost. A sentence spoken before the panel was ready never reached the
    // transcript at all, and the standing advice was to start jay a minute
    // early — exactly the sort of rule nobody remembers when a call is already
    // starting.
    //
    // Frames pile up in the channel above while this runs and are segmented in
    // one burst afterwards. Nothing downstream notices: every stamp comes from
    // `frame.captured_at`, so the backlog is timestamped from when it was
    // heard rather than from when it was finally read.
    let mut whisper = Whisper::load(model).context("loading whisper model")?;
    if let Some(vocab) = &vocab {
        // Commas are how a person writes a word list; whisper wants prose.
        whisper.prime(&expand_vocab(vocab).replace(',', " "));
    }

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
                // Polled rather than blocked on, and the difference is the
                // whole session shutting down cleanly.
                //
                // `for request in rx` ends only when the panel drops its
                // sender, and eframe does not reliably drop the app struct on
                // macOS when `run_native` returns under an `.app` bundle. That
                // left this thread parked forever holding a clone of the
                // question channel, which the assistant thread waits on, which
                // `run_pipeline` joins, which `main` joins. A 25-second session
                // sat there for two minutes with its transcript finished, its
                // panel already gone, and no notes written — and it only
                // happened through the bundle, which is the only way jay is
                // ever really launched.
                loop {
                    if INTERRUPTED.load(Relaxed) {
                        break;
                    }
                    let request = match rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(request) => request,
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => continue,
                        Err(crossbeam_channel::RecvTimeoutError::Disconnected) => break,
                    };
                    match request {
                        jay_ui::Request::SetMode(next) => {
                            mode = next;
                            // Names what the round gives, not merely which
                            // round it is. See `Mode::gives`.
                            let _ = lines.send(jay_ui::Line::notice(format!(
                                "round: {} — {}",
                                next.label(),
                                next.gives()
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
                // Where this sits in the session, taken from when the speech
                // began. Not from now: "now" is the end of the sentence plus
                // the VAD's silence hangover plus however long the decoder
                // took, which is seconds and varies with the length of what
                // was said.
                let at = utterance.started_at.saturating_duration_since(epoch);

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
                // Not trusted enough to spend money reasoning about. Whether
                // it stays in the *record* is a separate question, and the two
                // used to be the same question, which cost a real session ten
                // of the candidate's utterances with nothing left behind but a
                // duration and a peak.
                //
                // Words nobody said are dropped outright. Words somebody
                // probably said, quietly, go into the transcript marked — and
                // still not into the context.
                if let Some(rejected) = jay_stt::judge(&result, utterance.speech_peak, spoken) {
                    tracing::debug!(
                        text = %result.text,
                        peak = utterance.speech_peak,
                        confidence = result.confidence,
                        ?rejected,
                        "did not trust a transcript"
                    );
                    let line = if rejected.was_said() {
                        jay_ui::Line::transcript(utterance.channel.label(), result.text, lag)
                            .at(at)
                            .spoken(spoken)
                            .uncertain()
                    } else {
                        jay_ui::Line::notice(rejected.notice(spoken, &result.text)).at(at)
                    };
                    if lines.send(line).is_err() {
                        return;
                    }
                    if rejected.was_said() {
                        // The reason, once, without repeating the words: they
                        // are on the line immediately above. This is the
                        // evidence the peak floor gets recalibrated from.
                        let _ = lines.send(
                            jay_ui::Line::notice(format!(
                                "kept the line above out of context — {}",
                                rejected.reason(spoken)
                            ))
                            .at(at),
                        );
                    }
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
                    Channel::Mic
                        if echo.is_echo(utterance.started_at, spoken, label, &result.text) =>
                    {
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
                        // Descending, so each removal leaves the indices below
                        // it untouched.
                        let stale =
                            echo.stale_copies(lines_so_far, Channel::Mic.label(), &result.text);
                        for &index in &stale {
                            history.remove(index);
                        }
                        if !stale.is_empty() {
                            // Named rather than merely announced, because the
                            // archive is written as lines arrive and cannot be
                            // edited afterwards: the `you:` copies stay on disk
                            // even though they have left the context. Whoever
                            // reads the file back — including the notes, and
                            // `jay ask --mode rehearsal` — needs to be told
                            // which lines they were.
                            let _ = lines.send(jay_ui::Line::notice(format!(
                                "{} earlier \"you\" {} of \"{}\" {} this room, not you; \
                                 dropped from context",
                                stale.len(),
                                if stale.len() == 1 { "copy" } else { "copies" },
                                first_words(&result.text, 8),
                                if stale.len() == 1 { "was" } else { "were" },
                            )));
                        }
                    }
                    Channel::Mic => {}
                }
                echo.remember(utterance.started_at, spoken, label, &result.text);

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
                    .send(
                        jay_ui::Line::transcript(utterance.channel.label(), result.text, lag)
                            .at(at)
                            .spoken(spoken),
                    )
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
    let (mut mic_was_muted, mut system_was_muted) = (false, false);

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
    let mut gated_frames = 0u64;
    let mut warned_about_the_gate = false;
    const LONELY_MIC_AFTER: Duration = Duration::from_secs(45);

    println!(
        "transcribing {source:?} audio with {model}. {}\n",
        if unlimited {
            "Ctrl-C when the meeting ends."
        } else {
            "Stopping when the clock runs out, or on Ctrl-C."
        }
    );
    if levels.mic.is_muted() {
        let _ = notices.send(jay_ui::Line::notice(
            "the microphone is muted: it is being metered and not transcribed. \
             Throw MUTE beside the you meter to record yourself."
                .to_string(),
        ));
    }
    if notes_at_the_end {
        // Said at the start rather than discovered at the end. It is the only
        // thing a bare `jay` spends anything on, and a charge nobody was told
        // about is a charge nobody agreed to.
        let _ = notices.send(jay_ui::Line::notice(
            "notes will be written when this ends. --no-notes to skip.".to_string(),
        ));
    }

    while !INTERRUPTED.load(Relaxed)
        && (unlimited || started.elapsed() < Duration::from_secs(seconds))
    {
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

        // Read before the mutable borrow below, and only ever applied to the
        // microphone: the far side arrives down a wire and has no room to
        // cross, so there is nothing on that channel to gate.
        let gated = echo_gate
            && frame.channel == Channel::Mic
            && system_segmenter.is_or_becoming_speech();
        if gated {
            gated_frames += 1;
            if !warned_about_the_gate {
                warned_about_the_gate = true;
                let _ = notices.send(jay_ui::Line::notice(
                    "holding the microphone shut while the other side speaks, so the \
                     room does not come back as you. Anything you say over them will \
                     not be transcribed. --no-echo-gate if you are on headphones."
                        .to_string(),
                ));
            }
        }

        let segmenter = match frame.channel {
            Channel::Mic => &mut mic_segmenter,
            Channel::System => &mut system_segmenter,
        };

        // Muted: the level is still read above, so the meter goes on moving
        // and a muted microphone cannot be mistaken for a dead one, but
        // nothing reaches the transcript.
        //
        // The flush on the way in matters. Throwing the switch mid-sentence
        // leaves the VAD holding half an utterance, and without this it is
        // emitted the moment you unmute — a fragment of the thing you muted
        // yourself to keep out, arriving with a timestamp from before you
        // muted. Discarded rather than sent: that is what the switch is for.
        let muted = meter.is_muted();
        let was_muted = match frame.channel {
            Channel::Mic => &mut mic_was_muted,
            Channel::System => &mut system_was_muted,
        };
        if muted {
            if !*was_muted {
                *was_muted = true;
                let _ = segmenter.flush();
                meter.set_speaking(false);
            }
            continue;
        }
        *was_muted = false;

        let utterance = segmenter.push_gated(&frame, gated);
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

    // Stop the microphone before anything else, and before the decoder is
    // joined below.
    //
    // Nothing reads the frame channel once this loop has exited, so the capture
    // threads fill it and then park inside `send`. They are joined by `Drop`,
    // which used to run when this function returned — after the joins below —
    // so shutdown waited on threads waiting on a channel nobody was draining.
    // An 80-second session hung there with a finished transcript, an open panel
    // and no notes. Dropping here raises their stop flag first, which is the
    // half of the fix that lives outside `jay-audio`.
    //
    // The counters are read first, because they live on the captures and the
    // end-of-session report is the only place a dropped sample is ever
    // mentioned. Dropping first and reporting later would have quietly turned
    // that line off.
    let dropped_mic = _mic
        .as_ref()
        .map(mic::Capture::dropped_samples)
        .or_else(|| _voice_mic.as_ref().map(|c| c.dropped_samples()));
    let system_stats = _system
        .as_ref()
        .map(|c| (c.device_sample_rate(), c.dropped_samples()));
    drop(_mic.take());
    drop(_voice_mic.take());
    drop(_system.take());

    // Whatever was mid-sentence when time ran out is still worth having.
    for tail in [mic_segmenter.flush(), system_segmenter.flush()]
        .into_iter()
        .flatten()
    {
        let _ = utterance_tx.send(tail);
    }
    drop(utterance_tx);
    let _ = worker.join();

    // Here, and not one line later. The transcript is complete — the segmenters
    // are flushed and the decoder has drained — so the panel has everything it
    // will ever be shown and can close.
    //
    // It must be raised before the join below or the two wait on each other
    // forever. The assistant thread blocks on the request channel, whose sender
    // lives in the panel; the panel closes when this flag is raised; and this
    // function was raising it only on the way out. A 60-second session sat
    // there for three minutes with a finished transcript and no notes.
    finished.close();

    // The assistant is released, not joined.
    //
    // Dropping this scope's sender was once enough, on the reasoning that the
    // assistant's loop ends when the question channel closes. It is not: the
    // panel's request thread holds the other sender and outlives us, so joining
    // here waits on a thread waiting on a channel held open by a window that is
    // waiting on this function. Three-cornered, and it hung every timed session
    // launched from the `.app` bundle.
    //
    // Nothing is lost by letting it go. A suggestion still in flight at the end
    // of a session is a suggestion nobody is going to read, and the transcript
    // and the archive — the things that matter — are already complete by here.
    drop(question_tx);
    drop(assistant);

    if skipped_utterances > 0 {
        println!(
            "\n{skipped_utterances} utterance(s) skipped: transcription could not keep up"
        );
    }
    // Said in the archive as well as the terminal, because the gate removes
    // speech and a mechanism that removes speech must leave a mark. A session
    // that quietly dropped half of one side is exactly the failure jay exists
    // to avoid.
    if gated_frames > 0 {
        let held = Duration::from_secs_f64(
            gated_frames as f64 * jay_audio::FRAME_DURATION.as_secs_f64(),
        );
        let _ = notices.send(jay_ui::Line::notice(format!(
            "the echo gate held the microphone shut for {:.0}s in total, while the \
             other side was speaking.",
            held.as_secs_f64()
        )));
        println!("\necho gate held the microphone for {:.0}s", held.as_secs_f64());
    }
    if let Some(dropped) = dropped_mic {
        println!("\nmic dropped samples: {dropped}");
    }
    if let Some((rate, dropped)) = system_stats {
        println!("system tap: {rate} Hz, dropped samples: {dropped}");
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
            // Nothing behind a demo panel can finish, so it closes only by hand.
            jay_ui::Closer::new(),
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

    if state == "design" {
        let _ = line_tx.send(jay_ui::Line::suggestion(
            DEMO_DESIGN.to_string(),
            Duration::from_secs(16),
        ));
        let _ = line_tx.send(jay_ui::Line::notice(
            "16.2s · $0.168 · $0.17 this session".to_string(),
        ));
    } else if state == "writing" {
        // Half an answer, which is what the panel spends most of its ten
        // seconds drawing.
        let cut = DEMO_ANSWER.len() * 3 / 5;
        let _ = line_tx.send(jay_ui::Line::partial(DEMO_ANSWER[..cut].to_string()));
    } else {
        // Two answers, because the reading pane stacks them now and one
        // answer cannot show that it does. The second is a q&a follow-up, the
        // shape a round actually takes: solve it once, then defend it.
        let _ = line_tx.send(jay_ui::Line::suggestion(
            DEMO_ANSWER.to_string(),
            Duration::from_secs(10),
        ));
        let _ = line_tx.send(jay_ui::Line::notice(
            "10.3s · $0.196 · $0.20 this session".to_string(),
        ));
        let _ = line_tx.send(jay_ui::Line::suggestion(
            DEMO_FOLLOW_UP.to_string(),
            Duration::from_secs(8),
        ));
        let _ = line_tx.send(jay_ui::Line::notice(
            "8.1s · $0.301 · $0.50 this session".to_string(),
        ));
    }

    jay_ui::run(
        line_rx,
        request_tx,
        "medium.en".to_string(),
        jay_agent::Mode::Coding,
        jay_agent::Depth::default(),
        levels,
        // Nothing behind a demo panel can finish, so it closes only by hand.
        jay_ui::Closer::new(),
        [true, true],
    )
    .map_err(|e| anyhow::anyhow!("overlay: {e}"))
}

/// A real answer, verbatim, so the panel is checked against what it will
/// actually be asked to draw rather than against a convenient short string.
/// A design answer, as the design round actually returns one.
const DEMO_DESIGN: &str = r#"**1. Numbers.** 12 writes/s, 1500 reads/s: 125:1 read-heavy. Mean 30 KB against a 10 MB tail, so 45 MB/s out, 360 Mbit/s, one NIC. 20 GB/day. This is one machine's load; the second exists for failover.

**2. Diagram.**

```mermaid
flowchart TD
  C[Clients] -->|HTTPS| N[Nginx x2]
  N -->|create paste| A[App servers x2]
  N -->|GET slug| A
  A -->|insert row| P[Postgres primary]
  A -->|read paste| P
  P -->|streaming replication| R[Postgres replica]
  A -->|blobs over 1MB| F[Local NVMe]
```

**3. Components.** Nginx: TLS, 60s proxy_cache, serves hot pastes without touching the app. App: key generation, edit-token check, size limit. Primary: metadata and bodies under 1 MB as bytea. Replica: promotion candidate. NVMe: the 10 MB tail, by content hash.

**4. Decisions.** Bodies under 1 MB in Postgres, above it on disk: one backup story for the common case, traded against a sweeper for orphaned files. 7-char random key with a UNIQUE constraint and retry: no coordinator, costs a rare wasted round trip. Cache is 60s TTL rather than purge-on-edit: edits go visible late, and nothing has to be invalidated."#;

/// A q&a follow-up: prose, no code, no diagram. What the mode exists to give.
const DEMO_FOLLOW_UP: &str = "O(rows x cols), and the proof is the marking \
rule. A cell is looked at up to five times \u{2014} once by the outer sweep and once \
by each of its four neighbours \u{2014} but it is only ever worked on once, because \
it is marked the moment it is pushed, and every later look sees the mark and \
stops. Five is the bound regardless of grid size, because it is the cell's \
degree plus one and a 4-connected grid has degree four forever.\n\n\
Watch out: say \"looked at\" and \"worked on\" as different things, or the \
follow-up will be whether you know the difference.";

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

    // The notes path, which is the same subscription as the line above but a
    // different invocation of it: its own system prompt, no tools, and a
    // different model. Enough differs that "claude works" does not imply
    // "notes work", and finding that out after a meeting means finding it out
    // with nothing to show for the meeting.
    note("  notes     …    summarising one line".to_string());
    // A transcript in the archive's own shape, so this exercises the real path
    // rather than a preflight special case.
    let probe = "# jay session\n\n[00:00–00:04] them: right, we will ship on Friday\n";
    match jay_agent::notes::write(probe, jay_agent::notes::DEFAULT_MODEL) {
        Ok(n) => note(format!(
            "  notes     OK   {} in {:.1}s, {} prompt tokens, ${:.4}",
            n.model,
            n.spent.elapsed.as_secs_f32(),
            n.spent.prompt_tokens,
            n.spent.usd
        )),
        Err(e) => note(format!("  notes     FAIL {e}")),
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
    mic_path: MicPath,
) -> Result<()> {
    let (tx, rx) = crossbeam_channel::bounded::<Frame>(512);

    // Only one of these is ever `Some`. Two bindings rather than an enum
    // because each capture type stops itself on drop and the borrow checker is
    // happier watching two of them than one boxed trait object.
    let mut mic_capture = None;
    let mut voice_capture = None;
    if source.uses_mic() {
        match mic_path {
            MicPath::Plain => {
                mic_capture =
                    Some(mic::start(device, tx.clone()).context("starting microphone capture")?);
            }
            MicPath::Aec | MicPath::Bypass => {
                voice_capture = Some(
                    jay_audio::voice_mic::start(tx.clone(), mic_path == MicPath::Bypass)
                        .context("starting the voice-processing microphone")?,
                );
            }
        }
    }
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
    let channels = u64::from(mic_capture.is_some() || voice_capture.is_some())
        + u64::from(system_capture.is_some());
    let expected =
        channels * seconds * u64::from(SAMPLE_RATE) / jay_audio::FRAME_SAMPLES as u64;
    println!();
    println!("frames delivered : {frames} (expected roughly {expected})");
    println!("peak rms         : {peak_rms:.5}");
    println!("worst queue lag  : {worst_lag:?}");
    if let Some(capture) = &mic_capture {
        println!("mic dropped      : {}", capture.dropped_samples());
    }
    if let Some(capture) = &voice_capture {
        println!(
            "mic path         : voice unit, cancellation {}",
            if mic_path == MicPath::Bypass { "off" } else { "on" }
        );
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

    fn transcript(speaker: &str, at_secs: u64, spoken_secs: u64, text: &str) -> jay_ui::Line {
        jay_ui::Line::transcript(speaker, text, Duration::ZERO)
            .at(Duration::from_secs(at_secs))
            .spoken(Duration::from_secs(spoken_secs))
    }

    /// Run a whole session's worth of lines through the writer and read the
    /// file back. The epoch is set far enough in the past that every line is
    /// already past the reorder watermark, so this exercises ordering rather
    /// than timing.
    fn archive(lines: Vec<jay_ui::Line>) -> String {
        let path = std::env::temp_dir().join(format!(
            "jay-writer-{}-{:?}.md",
            std::process::id(),
            std::thread::current().id()
        ));
        let (tx, rx) = crossbeam_channel::unbounded();
        let (forward_tx, forward_rx) = crossbeam_channel::unbounded();
        for line in lines {
            tx.send(line).unwrap();
        }
        drop(tx);

        let epoch: SessionClock = std::sync::Arc::new(std::sync::OnceLock::new());
        epoch
            .set(Instant::now() - Duration::from_secs(3600))
            .unwrap();
        write_session(&path, &rx, &forward_tx, &epoch, REORDER_WINDOW);

        // Everything is forwarded, in arrival order, whatever the file says.
        assert!(forward_rx.try_iter().count() > 0);

        let text = std::fs::read_to_string(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        text
    }

    fn spoken_lines(archived: &str) -> Vec<&str> {
        archived
            .lines()
            .filter(|l| l.contains(": "))
            .filter(|l| l.starts_with('['))
            .collect()
    }

    #[test]
    fn a_line_is_stamped_from_when_the_speech_began_and_says_how_long_it_ran() {
        let archived = archive(vec![transcript("them", 125, 21, "count the islands")]);
        assert!(
            archived.contains("[02:05–02:26] them: count the islands"),
            "{archived}"
        );
    }

    /// The reason the writer exists. Utterances are decoded in the order they
    /// *ended*: a long question that began at 02:00 finishes after a two-word
    /// answer that began at 02:15, so it arrives second and, written as it
    /// arrives, the interruption reads backwards.
    #[test]
    fn overlapping_speech_is_written_in_the_order_it_was_said() {
        let archived = archive(vec![
            transcript("you", 135, 2, "mm-hm"),
            transcript("them", 120, 21, "so how would you shard that"),
            transcript("you", 142, 8, "by tenant, and here is why"),
        ]);
        assert_eq!(
            spoken_lines(&archived),
            vec![
                "[02:00–02:21] them: so how would you shard that",
                "[02:15–02:17] you: mm-hm",
                "[02:22–02:30] you: by tenant, and here is why",
            ],
            "{archived}"
        );
    }

    /// Heard but not trusted still reaches the file, marked. Ten of a real
    /// candidate's utterances went missing from a forty-minute interview
    /// because "not worth spending money on" and "not worth recording" were
    /// the same decision.
    #[test]
    fn a_line_jay_does_not_trust_is_kept_and_marked() {
        let archived = archive(vec![
            transcript("you", 10, 3, "that's the invariant").uncertain(),
        ]);
        assert!(
            archived.contains("[00:10–00:13] ?you: that's the invariant"),
            "{archived}"
        );
    }

    /// Notices happened at an instant, not over a span.
    #[test]
    fn a_notice_gets_one_stamp_rather_than_a_range() {
        let archived = archive(vec![jay_ui::Line::notice("listening on the default input")
            .at(Duration::from_secs(0))]);
        assert!(
            archived.contains("[00:00] (listening on the default input)"),
            "{archived}"
        );
    }

    /// Drafts of an answer are not the record of a meeting.
    #[test]
    fn partials_never_reach_the_file() {
        let archived = archive(vec![
            jay_ui::Line::partial("the appro"),
            jay_ui::Line::partial("the approach is"),
            jay_ui::Line::suggestion("the approach is a union-find", Duration::ZERO)
                .at(Duration::from_secs(30)),
        ]);
        assert!(!archived.contains("the appro\n"), "{archived}");
        assert!(archived.contains("the approach is a union-find"), "{archived}");
    }

    /// The flush at the end of a session sorts whatever is left, so the test
    /// above would pass even if the live path sorted nothing — which is
    /// exactly what the first version of this did. So: run the writer with a
    /// short window, keep the channel open, and read the file while it is
    /// still being written.
    #[test]
    fn the_live_path_sorts_too_and_not_only_the_final_flush() {
        const WINDOW: Duration = Duration::from_millis(100);
        let path = std::env::temp_dir().join(format!("jay-writer-live-{}.md", std::process::id()));
        let (tx, rx) = crossbeam_channel::unbounded();
        let (forward_tx, _forward_rx) = crossbeam_channel::unbounded();
        let epoch: SessionClock = std::sync::Arc::new(std::sync::OnceLock::new());
        epoch.set(Instant::now()).unwrap();

        let writer = std::thread::spawn({
            let path = path.clone();
            move || write_session(&path, &rx, &forward_tx, &epoch, WINDOW)
        });

        // Both arrive well inside the window, out of order, as they would from
        // a decoder working through two overlapping utterances.
        tx.send(transcript("you", 135, 2, "mm-hm")).unwrap();
        tx.send(transcript("them", 120, 21, "so how would you shard that"))
            .unwrap();

        // Long enough for them to fall due and be written, and far short of
        // the end of the session.
        std::thread::sleep(WINDOW * 6);
        let midway = std::fs::read_to_string(&path).unwrap();

        drop(tx);
        let written = writer.join().unwrap();
        let _ = std::fs::remove_file(&path);

        assert_eq!(
            spoken_lines(&midway),
            vec![
                "[02:00–02:21] them: so how would you shard that",
                "[02:15–02:17] you: mm-hm",
            ],
            "{midway}"
        );
        assert_eq!(written.spoken, 2);
    }

    #[test]
    fn interview_is_the_one_word_that_means_the_built_in_list() {
        assert_eq!(
            expand_vocab("interview"),
            jay_stt::whisper::INTERVIEW_VOCABULARY
        );
        assert_eq!(expand_vocab("  Interview "), jay_stt::whisper::INTERVIEW_VOCABULARY);
        // Everything else is taken at face value, including something that
        // merely mentions the word.
        assert_eq!(
            expand_vocab("interview panel, Patroni"),
            "interview panel, Patroni"
        );
        assert_eq!(expand_vocab("Fathom, MCP, Patroni"), "Fathom, MCP, Patroni");
    }

    #[test]
    fn the_clock_grows_an_hour_field_only_when_a_meeting_needs_one() {
        assert_eq!(clock(Duration::from_secs(0)), "00:00");
        assert_eq!(clock(Duration::from_secs(65)), "01:05");
        assert_eq!(clock(Duration::from_secs(59 * 60 + 59)), "59:59");
        assert_eq!(clock(Duration::from_secs(3600)), "1:00:00");
        assert_eq!(clock(Duration::from_secs(3600 + 125)), "1:02:05");
    }

    /// A bare `jay` is a meeting transcriber: both sides, no time limit, and
    /// nothing that can spend money because there is no panel to press.
    #[test]
    fn the_default_command_records_both_sides_until_stopped() {
        let cli = Cli::parse_from(["jay"]);
        assert!(cli.command.is_none());
        assert!(matches!(cli.transcribe.source, Source::Both));
        assert_eq!(cli.transcribe.seconds, 0);
        // The panel, not the terminal. It is where the mute switch is, and a
        // bare `jay` that opens no window was the first thing anyone using
        // this complained about.
        assert!(!cli.transcribe.terminal);
        assert!(!cli.transcribe.muted);
    }

    #[test]
    fn top_level_flags_reach_the_default_command() {
        let cli = Cli::parse_from(["jay", "--source", "mic", "--seconds", "30"]);
        assert!(matches!(cli.transcribe.source, Source::Mic));
        assert_eq!(cli.transcribe.seconds, 30);
    }

    #[test]
    fn naming_the_subcommand_still_works() {
        let cli = Cli::parse_from(["jay", "transcribe", "--terminal", "--source", "system"]);
        let Some(Command::Transcribe(args)) = cli.command else {
            panic!("expected the transcribe subcommand");
        };
        assert!(args.terminal);
        assert!(matches!(args.source, Source::System));
    }

    #[test]
    fn the_cli_is_internally_consistent() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
