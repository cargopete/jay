//! Whisper weight management.
//!
//! Models are fetched once into a cache directory and then found there. They
//! are deliberately not vendored: they run to hundreds of megabytes, they are
//! byte-identical for everyone, and git has no business holding them.

use std::fmt;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::{Result, SttError};

const HOST: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main";

/// Which whisper weights to run.
///
/// English-only variants wherever one exists: they are smaller and measurably
/// better than the multilingual ones at the same size, and jay's first job is
/// technical English. The exception is `turbo`, which has no English-only
/// build and does not need one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Model {
    /// 75 MB. Fast enough for anything, accurate enough for very little.
    /// Useful for latency experiments.
    Tiny,
    /// 142 MB. Fast, and good enough on clear speech in a quiet room.
    Base,
    /// 466 MB. Fast, and good on clear speech.
    ///
    /// Noticeably better on accents, jargon and poor microphones, which is
    /// exactly the failure that matters here: mishearing "idempotency" or
    /// "jemalloc" poisons every suggestion downstream, and no amount of
    /// cleverness in the model recovers a word the transcript never had. The
    /// extra decode time is irrelevant next to a twelve-second model call.
    Small,
    /// 1.5 GB. The default.
    ///
    /// Measured against the others on one jargon-heavy question: `small` heard
    /// "Right, so" as "Write, so" and ran the whole thing together as one
    /// sentence; `turbo` heard "idempotent writes" as "idempotent rights",
    /// which is worse, being both wrong and plausible. `medium` got both, at
    /// 12.6x real time — so a 22 second question decodes in under two seconds,
    /// which is nothing beside a nine second answer.
    #[default]
    Medium,
    /// 1.6 GB. `large-v3-turbo`: large's encoder with a four-layer decoder.
    ///
    /// Multilingual, which costs a little on English, and much better on
    /// unusual vocabulary, which is where jay actually loses. Decodes far
    /// faster than `medium` despite being larger, because almost all of
    /// whisper's decode cost is in decoder depth.
    Turbo,
}

impl Model {
    pub fn file_name(self) -> &'static str {
        match self {
            Model::Tiny => "ggml-tiny.en.bin",
            Model::Base => "ggml-base.en.bin",
            Model::Small => "ggml-small.en.bin",
            Model::Medium => "ggml-medium.en.bin",
            Model::Turbo => "ggml-large-v3-turbo.bin",
        }
    }

    /// Approximate download size, so the wait can be explained rather than
    /// simply endured.
    pub fn approx_mb(self) -> u32 {
        match self {
            Model::Tiny => 75,
            Model::Base => 142,
            Model::Small => 466,
            Model::Medium => 1533,
            Model::Turbo => 1624,
        }
    }
}

impl fmt::Display for Model {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Model::Tiny => "tiny.en",
            Model::Base => "base.en",
            Model::Small => "small.en",
            Model::Medium => "medium.en",
            Model::Turbo => "large-v3-turbo",
        };
        f.write_str(name)
    }
}

impl std::str::FromStr for Model {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "tiny" | "tiny.en" => Ok(Model::Tiny),
            "base" | "base.en" => Ok(Model::Base),
            "small" | "small.en" => Ok(Model::Small),
            "medium" | "medium.en" => Ok(Model::Medium),
            "turbo" | "large-v3-turbo" | "large" => Ok(Model::Turbo),
            other => Err(format!(
                "unknown model {other:?}, expected tiny, base, small, medium or turbo"
            )),
        }
    }
}

/// Where downloaded weights live.
pub fn cache_dir() -> PathBuf {
    std::env::var_os("JAY_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs_cache()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("jay")
                .join("models")
        })
}

fn dirs_cache() -> Option<PathBuf> {
    // No dependency needed for the two platforms that matter here.
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            return Some(home.join("Library").join("Caches"));
        }
        return Some(
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".cache")),
        );
    }
    None
}

/// Return the path to `model`, downloading it if it is not already cached.
pub fn ensure(model: Model) -> Result<PathBuf> {
    let dir = cache_dir();
    let path = dir.join(model.file_name());
    if path.is_file() {
        return Ok(path);
    }

    fs::create_dir_all(&dir)?;
    let url = format!("{HOST}/{}", model.file_name());
    tracing::info!(%model, mb = model.approx_mb(), "downloading whisper weights");

    // Download to a temporary name and rename on success, so an interrupted
    // download cannot leave a truncated file that looks cached forever after.
    let partial = path.with_extension("partial");
    download(&url, &partial)?;
    fs::rename(&partial, &path)?;

    tracing::info!(path = %path.display(), "weights ready");
    Ok(path)
}

fn download(url: &str, dest: &Path) -> Result<()> {
    let response = ureq::get(url)
        .call()
        .map_err(|e| SttError::Download(e.to_string()))?;

    let mut reader = response.into_body().into_reader();
    let mut file = fs::File::create(dest)?;
    let copied = std::io::copy(&mut reader, &mut file)?;
    file.flush()?;

    // A zero-length or absurdly small file means the host served an error page
    // with a 200, which happens more often than it should.
    if copied < 1_000_000 {
        let _ = fs::remove_file(dest);
        return Err(SttError::Download(format!(
            "downloaded only {copied} bytes from {url}, which is not a model"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_names() {
        assert_eq!("base".parse::<Model>().unwrap(), Model::Base);
        assert_eq!("small.en".parse::<Model>().unwrap(), Model::Small);
        assert_eq!("  TINY ".parse::<Model>().unwrap(), Model::Tiny);
        assert_eq!("turbo".parse::<Model>().unwrap(), Model::Turbo);
        assert_eq!("medium.en".parse::<Model>().unwrap(), Model::Medium);
        assert!("enormous".parse::<Model>().is_err());
    }

    /// Every model must name a real file and an honest size. A typo here is a
    /// 200-response HTML error page saved as weights.
    #[test]
    fn every_model_has_a_file_and_a_size() {
        for model in [Model::Tiny, Model::Base, Model::Small, Model::Medium, Model::Turbo] {
            assert!(model.file_name().starts_with("ggml-"), "{model}");
            assert!(model.file_name().ends_with(".bin"), "{model}");
            assert!(model.approx_mb() > 0, "{model}");
            // and the name round-trips through the parser
            assert_eq!(model.to_string().parse::<Model>().unwrap(), model);
        }
    }

    #[test]
    fn cache_dir_respects_the_override() {
        // SAFETY: single-threaded test, and the variable is read on the next line.
        unsafe { std::env::set_var("JAY_MODEL_DIR", "/tmp/jay-models-test") };
        assert_eq!(cache_dir(), PathBuf::from("/tmp/jay-models-test"));
        unsafe { std::env::remove_var("JAY_MODEL_DIR") };
    }
}
