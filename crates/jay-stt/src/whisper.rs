//! whisper.cpp backend, running on Metal.

use std::path::Path;
use std::time::Instant;

use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::{Result, SpeechModel, SttError, Transcription, models};

/// Whisper's encoder always processes a 30 second window, and it behaves badly
/// on inputs much shorter than a second. Anything briefer gets padded rather
/// than fed in as-is.
const MIN_SAMPLES: usize = 16_000;

pub struct Whisper {
    /// `WhisperState` holds an `Arc` of the inner context, so it keeps the
    /// model alive on its own and there is nothing else to store here.
    state: WhisperState,
    threads: i32,
    name: String,
    /// Words the decoder is told to expect. See [`Whisper::prime`].
    vocabulary: &'static str,
}

/// The interview vocabulary, available on request as `--vocab interview`.
///
/// **Not applied by default, and it used to be.** Every session got this,
/// including a 75-minute meeting about knowledge graphs and MCP servers, which
/// was told before every utterance that it was listening to an algorithms
/// interview. Priming is not a hint the decoder is free to ignore: it
/// conditions the output, so a wrong vocabulary is worse than none. That
/// meeting has the receipt — at 25:29 the decoder gave up and recited the
/// prompt back, and the transcript contains the line "This is a technical
/// interview about algorithms and system design", said by nobody.
///
/// It earns its place when the round really is an interview. Untuned,
/// `small.en` heard "reverse the linked list" as "reverse the link please".
/// Two registers, because both appear in the same sentence: the language of
/// algorithmic interviews, and the language of the work itself. Terms are
/// chosen for being both likely and easy to mishear — "idempotent" and
/// "jemalloc" are the sort of word a general model has no reason to reach for.
pub const INTERVIEW_VOCABULARY: &str = "This is a technical interview about \
algorithms and system design. Likely terms: linked list, binary tree, hash \
map, breadth-first search, depth-first search, dynamic programming, two \
pointers, sliding window, time complexity, space complexity, big O, amortised, \
in-place, memoisation, backtracking, adjacency list, topological sort, heap, \
trie, union-find. Also: Rust, borrow checker, ownership, lifetimes, trait, \
enum, Option, Result, iterator, async, tokio, mutex, atomic, Arc, idempotent, \
idempotency, jemalloc, throughput, latency, sharding, partition, replication, \
consistency, quorum, write-ahead log, backpressure, rate limiter, cache \
invalidation, load balancer, Postgres, Kafka, Redis, S3, blob storage, CDN, \
schema, index, subgraph, indexer, blockchain.";

impl Whisper {
    /// Load `model`, downloading the weights on first use.
    pub fn load(model: models::Model) -> Result<Self> {
        let path = models::ensure(model)?;
        Self::load_from(&path, model.to_string())
    }

    pub fn load_from(path: &Path, name: String) -> Result<Self> {
        // whisper.cpp and ggml log per-token detail straight to stderr, well
        // below the level the FullParams print flags reach. This routes them
        // into tracing, where the env filter can have an opinion.
        static HOOKS: std::sync::Once = std::sync::Once::new();
        HOOKS.call_once(whisper_rs::install_logging_hooks);

        if !path.is_file() {
            return Err(SttError::ModelMissing(path.to_path_buf()));
        }

        let path_str = path.to_string_lossy().into_owned();
        let context = WhisperContext::new_with_params(&path_str, WhisperContextParameters::default())
            .map_err(|e| SttError::Whisper(e.to_string()))?;
        let state = context
            .create_state()
            .map_err(|e| SttError::Whisper(e.to_string()))?;

        // Leave a core or two for capture and the UI. Whisper will happily
        // take every thread available and make the audio callback late.
        let threads = (std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
            .saturating_sub(2))
        .clamp(1, 8) as i32;

        tracing::info!(%name, threads, "whisper ready");
        Ok(Self {
            state,
            threads,
            name,
            vocabulary: "",
        })
    }

    /// Prime the decoder with the words this session expects to hear.
    ///
    /// whisper decodes conditioned on a prompt, so telling it which words are
    /// likely is the cheapest accuracy there is — and telling it the wrong
    /// ones is the cheapest way to make things worse. Nothing is primed unless
    /// a caller asks; see [`INTERVIEW_VOCABULARY`] for what asking used to
    /// cost when the answer was always yes.
    ///
    /// Leaked deliberately: the params borrow for `'static`, this is set once
    /// per process, and the alternative is threading a lifetime through the
    /// whole model for a few hundred bytes.
    pub fn prime(&mut self, vocabulary: &str) {
        let vocabulary = vocabulary.trim();
        if vocabulary.is_empty() {
            return;
        }
        self.vocabulary = Box::leak(vocabulary.to_string().into_boxed_str());
    }

    /// Free of `self` so the immutable borrow does not collide with the
    /// mutable borrow of `state` at the call site.
    fn params(threads: i32, vocabulary: &'static str) -> FullParams<'static, 'static> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(threads);
        params.set_language(Some("en"));
        params.set_translate(false);
        params.set_initial_prompt(vocabulary);

        // Each utterance is transcribed on its own. Carrying decoder context
        // between them lets one hallucination seed the next, and the VAD has
        // already decided these are separate stretches of speech.
        params.set_no_context(true);
        params.set_suppress_blank(true);

        // whisper.cpp prints to stdout by default, which would fight the
        // overlay for the terminal.
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        params
    }
}

impl SpeechModel for Whisper {
    fn transcribe(&mut self, samples: &[f32]) -> Result<Transcription> {
        let started = Instant::now();

        let padded;
        let audio = if samples.len() < MIN_SAMPLES {
            padded = {
                let mut v = Vec::with_capacity(MIN_SAMPLES);
                v.extend_from_slice(samples);
                v.resize(MIN_SAMPLES, 0.0);
                v
            };
            &padded[..]
        } else {
            samples
        };

        let params = Self::params(self.threads, self.vocabulary);
        self.state
            .full(params, audio)
            .map_err(|e| SttError::Whisper(e.to_string()))?;

        let mut text = String::new();
        // Worst segment rather than the mean: one confidently-invented segment
        // in an otherwise real utterance is still a sentence nobody said.
        let mut no_speech = 0.0f32;
        let mut prob_total = 0.0f32;
        let mut prob_count = 0usize;

        for i in 0..self.state.full_n_segments() {
            let Some(segment) = self.state.get_segment(i) else {
                continue;
            };
            if let Ok(s) = segment.to_str_lossy() {
                text.push_str(&s);
            }
            no_speech = no_speech.max(segment.no_speech_probability());
            for t in 0..segment.n_tokens() {
                if let Some(token) = segment.get_token(t) {
                    prob_total += token.token_probability();
                    prob_count += 1;
                }
            }
        }

        let text = text.trim().to_string();

        // Against the prompt actually used, so a session primed with
        // `--vocab` is checked against its own words rather than the defaults.
        let prompt_echo = crate::is_prompt_echo(&text, self.vocabulary);

        Ok(Transcription {
            prompt_echo,
            text,
            inference: started.elapsed(),
            no_speech,
            // No tokens means no text, which the artefact check rejects on its
            // own. Claiming full confidence here keeps this field from being
            // the thing that decides an empty transcript.
            confidence: if prob_count == 0 {
                1.0
            } else {
                prob_total / prob_count as f32
            },
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
