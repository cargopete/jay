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
}

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
        })
    }

    /// Free of `self` so the immutable borrow does not collide with the
    /// mutable borrow of `state` at the call site.
    fn params(threads: i32) -> FullParams<'static, 'static> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(threads);
        params.set_language(Some("en"));
        params.set_translate(false);

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

        let params = Self::params(self.threads);
        self.state
            .full(params, audio)
            .map_err(|e| SttError::Whisper(e.to_string()))?;

        let mut text = String::new();
        for i in 0..self.state.full_n_segments() {
            if let Some(segment) = self.state.get_segment(i)
                && let Ok(s) = segment.to_str_lossy()
            {
                text.push_str(&s);
            }
        }

        Ok(Transcription {
            text: text.trim().to_string(),
            inference: started.elapsed(),
        })
    }

    fn name(&self) -> &str {
        &self.name
    }
}
