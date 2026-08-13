//! What whisper does when handed nothing, and what jay must do about it.
//!
//! Whisper does not decline. Given room tone it writes fluent English with a
//! full stop on the end, and a live session archived two of these as things the
//! candidate had said:
//!
//! ```text
//! [00:21] you: -To us, Adam wanted to speak to us. -It's testing.
//! [00:24] you: Cool? Distinct.
//! ```
//!
//! Neither is on any phrase list, and neither was quiet: they came from a room
//! with a fan in it, comfortably above the speech-peak floor.
//!
//! It runs both ways on purpose. Rejecting everything is trivially achievable
//! and completely useless, so a real sentence goes through the same path and
//! has to survive.
//!
//! **What this does not yet prove.** Every rejection below comes from the
//! artefact list, because whisper hears synthetic noise for what it is and
//! writes `[static]` or `[Music]` rather than English. Even forty seconds of a
//! real recorded room produced only `(music)` and `(video plays)`. The fluent
//! case appears to need what the live pipeline does — short VAD-triggered
//! snippets with pre-roll, arriving just after real speech stopped — and until
//! a session yields a corpus of those, the confidence floor is an untested
//! backstop rather than the fix. Said plainly here so that a green run is not
//! mistaken for the problem being solved.
//!
//! Skipped, loudly, when the weights are not already cached — a test suite is
//! not the place to discover a 1.5 GB download.

use std::path::PathBuf;
use std::process::Command;

use jay_stt::{SpeechModel, models, whisper::Whisper};

const SAMPLE_RATE: usize = 16_000;

/// Long enough for the decoder to have something to be wrong about, short
/// enough that the whole corpus stays under a minute of inference.
const CHUNK_SECONDS: usize = 8;

/// Deterministic noise. A seeded LCG rather than `rand`, because a test that
/// fails once a fortnight on a lucky seed is worse than no test.
struct Lcg(u64);

impl Lcg {
    fn next_unit(&mut self) -> f32 {
        // Numerical Recipes constants; the low bits are poor, so take the top.
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        ((self.0 >> 40) as f32 / (1u32 << 24) as f32) * 2.0 - 1.0
    }
}

fn samples(seconds: usize) -> usize {
    seconds * SAMPLE_RATE
}

/// White noise at a chosen RMS.
fn noise(seed: u64, rms: f32) -> Vec<f32> {
    let mut lcg = Lcg(seed);
    // Uniform noise in [-1, 1] has an RMS of 1/sqrt(3), so scale by the ratio
    // rather than by the amplitude, or the level is off by 73%.
    let scale = rms * 3f32.sqrt();
    (0..samples(CHUNK_SECONDS))
        .map(|_| lcg.next_unit() * scale)
        .collect()
}

/// A hum at `hz`, which is what a room with a transformer in it sounds like.
fn hum(hz: f32, rms: f32) -> Vec<f32> {
    let amplitude = rms * 2f32.sqrt();
    (0..samples(CHUNK_SECONDS))
        .map(|i| {
            let t = i as f32 / SAMPLE_RATE as f32;
            (std::f32::consts::TAU * hz * t).sin() * amplitude
        })
        .collect()
}

fn peak(samples: &[f32]) -> f32 {
    samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
}

/// The cached weights, or `None` with a reason printed.
fn model() -> Option<Whisper> {
    let path = models::cache_dir().join(models::Model::Medium.file_name());
    if !path.is_file() {
        eprintln!(
            "SKIPPED: no weights at {}. Run `jay check` once to fetch them.",
            path.display()
        );
        return None;
    }
    match Whisper::load_from(&path, "medium.en".to_string()) {
        Ok(w) => Some(w),
        Err(e) => {
            eprintln!("SKIPPED: could not load {}: {e}", path.display());
            None
        }
    }
}

/// Every kind of nothing a room offers, at levels that matter.
///
/// The loud entries are the point. Quiet noise is already caught by the
/// speech-peak floor, so a corpus of only quiet noise would pass against a
/// build with no confidence check in it at all.
fn corpus() -> Vec<(String, Vec<f32>)> {
    let mut corpus: Vec<(String, Vec<f32>)> = vec![
        ("digital silence", vec![0.0; samples(CHUNK_SECONDS)]),
        ("quiet room tone", noise(1, 0.003)),
        ("loud room tone", noise(2, 0.03)),
        ("a fan, near the mic", noise(3, 0.05)),
        ("another draw of the same fan", noise(4, 0.05)),
        ("50 Hz mains hum", hum(50.0, 0.03)),
        ("a whine off a display", hum(440.0, 0.02)),
    ]
    .into_iter()
    .map(|(name, audio)| (name.to_string(), audio))
    .collect();

    // Synthetic noise is not the hard case. Whisper hears white noise for what
    // it is and writes "[static]", which the bracket rule catches without any
    // of this. Fluent invented English comes from *rooms* — a fan, a fridge, a
    // building — so point this at a real recording of the room you will sit in
    // and the corpus becomes the one that matters:
    //
    //   ffmpeg -f avfoundation -i :default -t 40 -ac 1 -ar 16000 room.wav
    //   JAY_ROOM_TONE=room.wav cargo test -p jay-stt --test silence
    if let Some(path) = std::env::var_os("JAY_ROOM_TONE") {
        let path = PathBuf::from(path);
        let audio = read_wav(&path).unwrap_or_else(|| {
            panic!("JAY_ROOM_TONE is set but {} could not be read", path.display())
        });
        for (i, chunk) in audio.chunks(samples(CHUNK_SECONDS)).enumerate() {
            // A part-chunk at the end is still a stretch of room, but it is a
            // shorter one, and whisper's behaviour on stubs is its own subject.
            if chunk.len() == samples(CHUNK_SECONDS) {
                corpus.push((format!("room tone, chunk {i}"), chunk.to_vec()));
            }
        }
    }

    corpus
}

/// 16 kHz mono, which is the only shape anything here deals in.
fn read_wav(path: &std::path::Path) -> Option<Vec<f32>> {
    let reader = hound::WavReader::open(path).ok()?;
    Some(
        reader
            .into_samples::<i16>()
            .filter_map(std::result::Result::ok)
            .map(|s| f32::from(s) / f32::from(i16::MAX))
            .collect(),
    )
}

#[test]
fn nothing_in_the_room_ever_reaches_the_transcript() {
    let Some(mut whisper) = model() else { return };

    let mut survivors = Vec::new();
    for (name, audio) in corpus() {
        let transcription = whisper.transcribe(&audio).expect("transcribing silence");
        let verdict = jay_stt::judge(&transcription, peak(&audio));

        println!(
            "{name:<30} peak {:.4}  no_speech {:.2}  confidence {:.2}  {:?}  {:?}",
            peak(&audio),
            transcription.no_speech,
            transcription.confidence,
            verdict,
            transcription.text
        );

        if verdict.is_none() {
            survivors.push(format!("{name}: {:?}", transcription.text));
        }
    }

    assert!(
        survivors.is_empty(),
        "the transcriber invented these from an empty room and jay let them \
         through:\n  {}",
        survivors.join("\n  ")
    );
}

/// The other half. A filter that bins everything passes the test above.
#[test]
fn a_real_sentence_still_gets_through() {
    let Some(mut whisper) = model() else { return };

    let Some(spoken) = say(
        "Right, so the question is, reverse a singly linked list. \
         Can you do it in place, in one pass?",
    ) else {
        return;
    };

    let transcription = whisper.transcribe(&spoken).expect("transcribing speech");
    let verdict = jay_stt::judge(&transcription, peak(&spoken));

    println!(
        "control  no_speech {:.2}  confidence {:.2}  {:?}",
        transcription.no_speech, transcription.confidence, transcription.text
    );

    assert!(
        transcription.text.to_lowercase().contains("linked list"),
        "the control was not transcribed at all: {:?}",
        transcription.text
    );
    assert_eq!(
        verdict, None,
        "a real sentence was binned as {verdict:?}; the thresholds are too tight"
    );
    // Not merely above the floor, but clear of it. A control that scrapes past
    // by a hundredth is a floor about to bin somebody mid-interview.
    assert!(
        transcription.confidence > jay_stt::CONFIDENCE_FLOOR + 0.2,
        "real speech scored {:.2} against a floor of {:.2}; too close to trust",
        transcription.confidence,
        jay_stt::CONFIDENCE_FLOOR
    );
}

/// Does priming the decoder make it invent more?
///
/// Worth asking, because priming is not free in the way it looks. Telling
/// whisper which words to expect is the cheapest accuracy available, and it is
/// also a list of words it can reach for when it has nothing to transcribe — a
/// real session archived `them: Redis  Kafka` twice off a silent channel.
///
/// [`jay_stt::is_prompt_echo`] catches the recitations. This measures whether
/// priming also increases the *other* kind of invention, which nothing catches.
/// Needs real room audio to say anything at all: synthetic noise is heard as
/// `[static]` and never fabricates.
///
///   JAY_ROOM_TONE=room.wav cargo test -p jay-stt --test silence -- --nocapture
#[test]
fn priming_is_measured_rather_than_assumed() {
    let Some(path) = std::env::var_os("JAY_ROOM_TONE") else {
        eprintln!("SKIPPED: set JAY_ROOM_TONE to a recording of your own room");
        return;
    };
    let Some(audio) = read_wav(&PathBuf::from(path)) else {
        eprintln!("SKIPPED: could not read JAY_ROOM_TONE");
        return;
    };
    let chunks: Vec<&[f32]> = audio
        .chunks(samples(CHUNK_SECONDS))
        .filter(|c| c.len() == samples(CHUNK_SECONDS))
        .collect();

    for primed in [false, true] {
        let Some(mut whisper) = model() else { return };
        if primed {
            whisper.prime("pastebin, base62, Postgres, nginx, Redis, Kafka");
        }
        let mut survived = 0;
        for chunk in &chunks {
            let t = whisper.transcribe(chunk).expect("transcribing room tone");
            let verdict = jay_stt::judge(&t, peak(chunk));
            println!(
                "{:>8} {:?}  {:?}",
                if primed { "primed" } else { "bare" },
                verdict,
                t.text
            );
            if verdict.is_none() {
                survived += 1;
            }
        }
        println!(
            "{} : {survived} of {} chunks reached the transcript",
            if primed { "primed" } else { "bare" },
            chunks.len()
        );
        assert_eq!(
            survived,
            0,
            "room tone reached the transcript with priming {}",
            if primed { "on" } else { "off" }
        );
    }
}

/// Synthesised speech, via the one text-to-speech engine every mac already has.
///
/// Not a fixture in the repository because a WAV of a sentence is a megabyte
/// that would need reviewing, and `say` is deterministic enough for the job:
/// the assertion is that the words survive, not that the bytes match.
fn say(text: &str) -> Option<Vec<f32>> {
    let out = std::env::temp_dir().join("jay-stt-control.wav");
    let status = Command::new("say")
        .args(["-o", &out.to_string_lossy(), "--data-format=LEI16@16000", text])
        .status();

    match status {
        Ok(s) if s.success() => {}
        _ => {
            eprintln!("SKIPPED: `say` is unavailable, so there is no control utterance");
            return None;
        }
    }

    let reader = hound::WavReader::open(&out).ok()?;
    Some(
        reader
            .into_samples::<i16>()
            .filter_map(std::result::Result::ok)
            .map(|s| f32::from(s) / f32::from(i16::MAX))
            .collect(),
    )
}
