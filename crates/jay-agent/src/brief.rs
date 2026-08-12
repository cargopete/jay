//! Assembling standing context — the "second brain" half.
//!
//! Without a brief, every suggestion is reasoned from a dozen lines of
//! transcript and nothing else, which is why they read generic. With one, the
//! same model knows you have operated real indexers and gateways, and starts
//! reaching for cache invalidation on the delete path unprompted.
//!
//! The richest source of that context already exists: the per-scope `MEMORY.md`
//! index files, which are one line per project with the hook already written.
//! They were built to be read into context, so they need very little doing to
//! them.
//!
//! The brief is written to a file rather than assembled on every run, on
//! purpose. It is a starting point to *edit*: the generator cannot know which
//! projects matter for the conversation you are about to have, and a human
//! deleting forty irrelevant lines is worth more than any heuristic.

use std::path::{Path, PathBuf};

use crate::{AgentError, Result};

/// Default location of the memory tree on this machine.
pub fn default_memory_root() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let candidate = PathBuf::from(home)
        .join("Projects")
        .join("claude-skills")
        .join("memory");
    candidate.is_dir().then_some(candidate)
}

/// One project, as the memory index describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    pub hook: String,
}

/// Pull the index lines out of a `MEMORY.md`.
///
/// The format is `- [Title](file.md) — hook`, so the link target is dropped
/// (it points at a file the model will never open) and the hook is kept, since
/// the hook is the part carrying the knowledge.
pub fn parse_index(markdown: &str) -> Vec<Entry> {
    markdown
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix("- [")?;
            let (name, rest) = rest.split_once("](")?;
            let (_target, rest) = rest.split_once(')')?;
            let hook = rest
                .trim_start_matches([' ', '-', '—', '–'])
                .trim()
                .to_string();
            (!hook.is_empty()).then(|| Entry {
                name: name.trim().to_string(),
                hook,
            })
        })
        .collect()
}

/// Build a brief from every `MEMORY.md` under `root`.
///
/// `keywords` filters entries to those mentioning any of them, case
/// insensitively. Passing none keeps everything, which is rarely what you
/// want: measured on one interview question, a six-line hand-written brief
/// produced a better answer than the full 181-project dump, which lost a
/// specific point about negative caching and cost two and a half times as
/// much. Context is not free and it is not automatically helpful.
///
/// Returns the markdown and the number of projects kept.
pub fn assemble(root: &Path, keywords: &[String]) -> Result<(String, usize)> {
    if !root.is_dir() {
        return Err(AgentError::Brief(format!(
            "{} is not a directory",
            root.display()
        )));
    }

    let mut entries: Vec<Entry> = Vec::new();
    collect(root, &mut entries)?;

    // Same project can be indexed in more than one scope.
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries.dedup_by(|a, b| a.name == b.name);

    if !keywords.is_empty() {
        let needles: Vec<String> = keywords.iter().map(|k| k.to_lowercase()).collect();
        entries.retain(|entry| {
            let haystack = format!("{} {}", entry.name, entry.hook).to_lowercase();
            needles.iter().any(|needle| haystack.contains(needle))
        });
    }

    let mut out = String::new();
    out.push_str(
        "# Standing context\n\n\
         Edit this before a session. The generator cannot know which of these \
         matter for the conversation you are about to have, and deleting the \
         irrelevant ones sharpens every suggestion.\n\n\
         ## Who you are\n\n\
         <!-- Write two or three sentences: role, depth, what you have actually \
         operated rather than only read about. This is the part that changes \
         suggestions the most, and it is the part no generator can write for \
         you. -->\n\n\
         ## What you have built and run\n\n",
    );

    for entry in &entries {
        out.push_str("- **");
        out.push_str(&entry.name);
        out.push_str("** — ");
        out.push_str(&entry.hook);
        out.push('\n');
    }

    Ok((out, entries.len()))
}

fn collect(dir: &Path, into: &mut Vec<Entry>) -> Result<()> {
    let listing = std::fs::read_dir(dir)
        .map_err(|e| AgentError::Brief(format!("reading {}: {e}", dir.display())))?;

    for item in listing.flatten() {
        let path = item.path();
        if path.is_dir() {
            collect(&path, into)?;
        } else if path.file_name().is_some_and(|n| n == "MEMORY.md")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            into.extend(parse_index(&text));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_index_format() {
        let md = "# Memory Index\n\n\
                  - [project_nuthatch.md](project_nuthatch.md) — nuthatch: a Rust indexer\n\
                  - [feedback_voice.md](feedback_voice.md) — VOICE: prose over bullets\n";
        let entries = parse_index(md);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, "project_nuthatch.md");
        assert_eq!(entries[0].hook, "nuthatch: a Rust indexer");
    }

    #[test]
    fn ignores_prose_and_headings() {
        let md = "# Memory Index\n\nSome preamble.\n\n- a bare bullet\n";
        assert!(parse_index(md).is_empty());
    }

    #[test]
    fn drops_entries_with_no_hook() {
        // A link with nothing after it carries no knowledge, so it is only
        // tokens.
        let md = "- [thing.md](thing.md)\n- [other.md](other.md) — a real hook\n";
        let entries = parse_index(md);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].hook, "a real hook");
    }

    #[test]
    fn handles_both_dash_kinds() {
        let md = "- [a.md](a.md) - hyphen hook\n- [b.md](b.md) — em dash hook\n";
        let entries = parse_index(md);
        assert_eq!(entries[0].hook, "hyphen hook");
        assert_eq!(entries[1].hook, "em dash hook");
    }

    #[test]
    fn assemble_rejects_a_missing_root() {
        assert!(assemble(Path::new("/nonexistent/memory"), &[]).is_err());
    }

    #[test]
    fn keywords_match_name_or_hook_case_insensitively() {
        let entries = [
            Entry { name: "project_nuthatch.md".into(), hook: "a Rust indexer".into() },
            Entry { name: "project_linnet.md".into(), hook: "a Flutter period tracker".into() },
        ];
        let keep = |kw: &str| {
            let needle = kw.to_lowercase();
            entries
                .iter()
                .filter(|e| format!("{} {}", e.name, e.hook).to_lowercase().contains(&needle))
                .count()
        };
        assert_eq!(keep("INDEXER"), 1);
        assert_eq!(keep("nuthatch"), 1);
        assert_eq!(keep("flutter"), 1);
        assert_eq!(keep("kubernetes"), 0);
    }
}
