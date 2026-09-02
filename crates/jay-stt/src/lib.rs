//! Speech to text for jay.
//!
//! One trait, several possible backends. The first is whisper.cpp on Metal,
//! which is the pragmatic local choice on Apple silicon. Parakeet through
//! CoreML and a cloud backend for people who would rather pay than wait both
//! fit the same shape, which is the reason for the trait.

use std::time::Duration;

pub mod models;
pub mod whisper;

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("model file not found at {0}")]
    ModelMissing(std::path::PathBuf),
    #[error("downloading model: {0}")]
    Download(String),
    #[error("whisper: {0}")]
    Whisper(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, SttError>;

/// What a backend gives back for one utterance.
#[derive(Debug, Clone)]
pub struct Transcription {
    pub text: String,
    /// How long inference took, which is the half of the latency budget that
    /// is actually ours to control.
    pub inference: Duration,
    /// The decoder's own estimate that this window contained no speech at all,
    /// taken as the worst of the segments.
    ///
    /// **Reported, not acted on.** It would be the ideal signal, and on the
    /// English-only weights jay ships with it is dead: whisper.cpp reads it off
    /// the probability of the `<|nospeech|>` token, which an `.en` vocabulary
    /// effectively never predicts. Measured 0.00 on `medium.en` for everything
    /// including eight seconds of pure digital silence, which is the strongest
    /// no-speech case that exists. Anything gating on it would be a gate that
    /// never closes, so [`judge`] does not.
    ///
    /// Kept because it costs nothing, it is worth re-checking on a multilingual
    /// model, and a zero here is now a documented zero rather than a mystery.
    pub no_speech: f32,
    /// The decoder handed back a piece of its own priming prompt.
    ///
    /// See [`is_prompt_echo`].
    pub prompt_echo: bool,
    /// Mean probability of the tokens actually emitted, in `0.0..=1.0`.
    ///
    /// Real speech decodes confidently. An invention assembled out of room
    /// tone does not, and this is the number that says so. A backend with no
    /// such signal should report `1.0`, for the same reason as above.
    pub confidence: f32,
}

/// A local or remote speech recogniser.
///
/// `&mut self` rather than `&self` because whisper carries decoder state and
/// the borrow makes it obvious that one instance transcribes one thing at a
/// time. Run two if you want two channels in parallel.
pub trait SpeechModel: Send {
    /// Transcribe one complete utterance of 16 kHz mono `f32`.
    fn transcribe(&mut self, samples: &[f32]) -> Result<Transcription>;

    /// Human-readable identifier, for logs and for the overlay's status line.
    fn name(&self) -> &str;
}

/// Strip the artefacts whisper emits when handed near-silence.
///
/// Even gated by a VAD, a short breath or a door closing occasionally reaches
/// the model, and whisper is famously willing to fill the gap with "Thank
/// you." or "[BLANK_AUDIO]" delivered with total confidence. These are worth
/// dropping before they reach a transcript that something else will reason
/// over.
pub fn is_hallucination(text: &str) -> bool {
    /// Exact matches: bracketed markers and bare artefacts.
    const ARTEFACTS: &[&str] = &[
        "[blank_audio]",
        "(blank_audio)",
        "[silence]",
        "[music]",
        "(upbeat music)",
        "thank you.",
        "you",
        ".",
    ];

    /// Phrases whisper produces from silence, anywhere in the line.
    ///
    /// These are not guesses. Whisper's training data is heavy with subtitled
    /// video, so given nothing to transcribe it reaches for the way videos
    /// end. A 90-second recording of an empty room produced "I'll see you next
    /// time" unprompted, which would quietly poison a transcript with a
    /// sentence nobody said.
    ///
    /// Every entry here is a phrase nobody says in a meeting. That is the bar
    /// for being on this list rather than [`SIGN_OFFS`], and it is a bar three
    /// entries used to fail.
    const CREDITS: &[&str] = &[
        "thanks for watching",
        "thank you for watching",
        "don't forget to subscribe",
        "like and subscribe",
        "subscribe to my channel",
        "hit the bell",
        "please subscribe",
        "transcription by",
        "subtitles by",
        "amara.org",
        // Not a sign-off, and not from subtitles either. Whisper's training
        // data carries a good deal of Word-generated HTML, and handed a second
        // of nothing it will occasionally emit the schema namespace out of it.
        // Observed verbatim on the `you` channel of a dictated test:
        // `urn:schemas-microsoft-com:office:smarttags City urn:schemas-…`
        // Safe anywhere in a line, since nobody has ever said it out loud.
        "urn:schemas-microsoft-com",
    ];

    /// Artefacts whose words are also ordinary speech.
    ///
    /// Only an artefact when they are *all* that was said, which is why these
    /// are checked against short utterances only. Matched anywhere in a line,
    /// as they were until a 75-minute meeting was recorded, they are a
    /// disaster:
    ///
    /// ```text
    /// "at the end of the day we should just cache it"   -> dropped
    /// "hit the endpoint and see what comes back"        -> dropped
    /// "the end user never sees any of that"             -> dropped
    /// "it is documented at nuthatch-indexer.com"        -> dropped
    /// ```
    ///
    /// Two real sentences went that way in that meeting, one of them the only
    /// substantive technical proposal in the first two minutes. "The end" is a
    /// substring of "the endpoint"; a `.com` is how anybody cites a document.
    /// Nothing announced it — the words were quoted in a drop notice and the
    /// transcript simply did not have them.
    const SIGN_OFFS: &[&str] = &[
        "see you next time",
        "see you in the next",
        "bye bye",
        "the end",
        "amara.org",
        "www.",
        ".com",
    ];

    /// Longest an utterance can be and still be dismissed as a sign-off.
    ///
    /// Whisper's inventions on silence are short — it is filling a gap, not
    /// composing. Six words keeps "And that's the end", "www.mooji.org" and
    /// "Bye bye, see you next time", and keeps its hands off every sentence in
    /// the block comment above.
    const SIGN_OFF_MAX_WORDS: usize = 6;

    let normalised = text.trim().to_ascii_lowercase();
    if normalised.is_empty() || ARTEFACTS.contains(&normalised.as_str()) {
        return true;
    }

    // Bracketed sound markers of any kind: [BLANK_AUDIO], (typing), [laughs].
    //
    // Trimmed of the subtitle dashes and stray punctuation whisper puts around
    // them first. Without that, `- (speaking in foreign language)` walked
    // straight through this check on a real session, because it does not start
    // with a bracket — it starts with the dash that introduces a speaker turn
    // in the subtitles whisper learnt from.
    let bare = normalised.trim_matches(|c: char| c == '-' || c == '.' || c.is_whitespace());
    let bracketed = (bare.starts_with('[') && bare.ends_with(']'))
        || (bare.starts_with('(') && bare.ends_with(')'));
    if bracketed {
        return true;
    }

    if CREDITS.iter().any(|fragment| normalised.contains(fragment)) {
        return true;
    }

    // Short enough to be something whisper made up rather than something
    // somebody said.
    normalised.split_whitespace().count() <= SIGN_OFF_MAX_WORDS
        && SIGN_OFFS.iter().any(|fragment| normalised.contains(fragment))
}

/// Is this transcript a piece of the prompt the decoder was primed with?
///
/// Priming whisper with the vocabulary of the round is the cheapest accuracy
/// available, and it buys a great deal — it is the difference between "reverse
/// a singly linked list" and "reverse the link please". It also creates a new
/// way to invent text, which cost a real session:
///
/// ```text
/// [00:12] them: Redis  Kafka
/// [00:20] them: Redis  Kafka
/// ```
///
/// Nobody said that. The session was primed with `--vocab "… Redis, Kafka"`,
/// the system channel had nothing on it, and the decoder reached for its own
/// prompt. Those clear every other filter: they are not known artefacts, they
/// are not quiet, and they are decoded confidently, because the model is
/// copying rather than guessing.
///
/// The test is whether every word of the transcript appears in the prompt, *in
/// the prompt's own order*. Contiguity is too strict: the first attempt at this
/// required an unbroken run and was immediately beaten by
///
/// ```text
/// [00:04] you: Postgres  nginx  Kafka
/// ```
///
/// which skips "Redis" from the middle of the list it was reciting. Order is
/// the property that matters, because a vocabulary list's order is arbitrary.
/// Real speech that uses those words puts its own words between them — "we
/// would put Redis in front of it" fails on "we" alone — and real speech that
/// happens to name two of them usually names them in its own order.
///
/// Two words minimum, because a lone "Postgres" is plausibly something somebody
/// said. The known cost of this rule: someone who genuinely says nothing but
/// "Postgres, Redis", in that order, loses it. That is two words of an
/// utterance nobody could act on, against fabricated lines that get spent as
/// context, and the trade is worth it.
pub fn is_prompt_echo(text: &str, prompt: &str) -> bool {
    let spoken = words(text);
    if spoken.len() < 2 {
        return false;
    }
    let primed = words(prompt);

    // Greedy in-order match, restarted from each place the first word occurs.
    // Greedy from a single start is not enough: the prompt repeats terms, and
    // the run that matches may begin at the second occurrence.
    primed
        .iter()
        .enumerate()
        .filter(|(_, word)| *word == &spoken[0])
        .any(|(start, _)| {
            let mut wanted = spoken.iter().skip(1);
            let mut next = wanted.next();
            for word in &primed[start + 1..] {
                match next {
                    None => break,
                    Some(target) if word == target => next = wanted.next(),
                    Some(_) => {}
                }
            }
            next.is_none()
        })
}

/// Lowercased words, stripped of the punctuation that separates a vocabulary
/// list from a sentence.
fn words(text: &str) -> Vec<String> {
    text.split_whitespace()
        .map(|w| {
            w.trim_matches(|c: char| !c.is_alphanumeric() && c != '\'')
                .to_ascii_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Speech peak below which a transcript is treated as invented.
///
/// Compared against the loudest frame the VAD called speech, never against the
/// mean over the whole utterance. The mean includes ~250 ms of pre-roll and
/// ~600 ms of trailing silence, so it scales with how long the pause afterwards
/// was rather than with how loudly anyone spoke — which quietly binned a
/// perfectly clear "reverse a linked list" on a machine whose room floor
/// measures 0.0028.
///
/// A single frame peak from a voice at a laptop's distance clears this by a
/// wide margin. Room tone does not.
pub const SPEECH_PEAK_FLOOR: f32 = 0.01;

/// An utterance shorter than this is only trusted if it was also loud.
///
/// Measured, by dictating a script into a live session and logging every
/// utterance. Real speech, on both channels, ran **8.4 to 15.4 seconds**.
/// Every invention in the quiet minute afterwards ran **1.25 to 3.14 seconds**.
/// Nothing straddled the gap.
///
/// Duration alone would be too blunt — "use a visited grid" is a real answer
/// and takes two seconds — so this is paired with [`QUIET_SPEECH_PEAK`] and
/// both must hold. Somebody speaking briefly *at their laptop* is short and
/// loud; a room being captioned is short and faint.
pub const BRIEF_UTTERANCE: Duration = Duration::from_millis(3500);

/// The peak below which a brief utterance is not believed.
///
/// **Lowered from 0.03 after a real interview, where it was plainly wrong.**
///
/// The original number came from measuring a *speaker* playing into the
/// microphone, which reads 0.06–0.15. A person actually being interviewed —
/// sitting normally, headphones on, not leaning into the machine — reads
/// 0.02–0.03. So the threshold had been set in the middle of the range it was
/// supposed to be under, and ten of the candidate's utterances were dropped in
/// forty minutes, every one of them peaking between 0.018 and 0.029.
///
/// The asymmetry decides the new value. A fabricated line is visible in the
/// panel and can be ignored; a real sentence that never appears is gone, and
/// its absence looks exactly like not having spoken. So this now sits below any
/// plausible speaking voice and the rule catches only the genuinely faint.
///
/// That makes the rule weaker than it was, and knowingly so — several of the
/// inventions it caught in testing peaked between 0.018 and 0.042 and will now
/// get through. It is not recalibrated by guessing again: every drop now
/// records the words it threw away, so one session says whether these were
/// sentences or "mm-hm"s. See [`Rejected::notice`].
pub const QUIET_SPEECH_PEAK: f32 = 0.012;

/// Reject when the mean token probability falls below this.
///
/// Deliberately low. The evidence behind it is thin: `say`-synthesised speech
/// decodes at 0.88, digital silence captioned "Thank you for watching." at
/// 0.48, and noise whisper correctly labels `[static]` sits anywhere from 0.68
/// to 0.98. That last range is the problem — confidence alone does not separate
/// noise from speech, it only catches the decoder writing something it plainly
/// does not believe.
///
/// So this is a backstop behind the artefact list and the level floor, set to
/// err toward keeping a real quiet sentence rather than binning one. Raising it
/// requires a corpus of genuine fluent inventions, which needs live sessions to
/// collect. See `tests/silence.rs`.
pub const CONFIDENCE_FLOOR: f32 = 0.45;

/// Why a transcript was not allowed into the conversation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Rejected {
    /// A phrase whisper is known to produce from nothing.
    KnownArtefact,
    /// The audio was never loud enough to have been speech.
    TooQuiet { peak: f32 },
    /// The decoder wrote it without believing it.
    Invented { confidence: f32 },
    /// The decoder handed back part of its own priming vocabulary.
    PrimingEcho,
    /// Too short to be speech, and too faint to be a short answer.
    TooBrief { spoken: Duration, peak: f32 },
    /// The same clause over and over: the decoder looping on near-silence.
    Looping,
}

impl Rejected {
    /// Did a person say this, or did the decoder make it up?
    ///
    /// The distinction decides what happens to the words. A known artefact and
    /// a recital of the priming vocabulary are things nobody said, and putting
    /// them in a record of a meeting is worse than leaving them out. The other
    /// three are judgements about audio that a person quite possibly did
    /// produce — quietly, or briefly — and those belong in the transcript,
    /// marked, even when they are kept out of the model's context.
    ///
    /// This is the same asymmetry [`QUIET_SPEECH_PEAK`] is set on: a
    /// fabrication you can see is a nuisance, and a sentence that never
    /// appears is indistinguishable from not having spoken.
    pub fn was_said(&self) -> bool {
        match self {
            Rejected::KnownArtefact | Rejected::PrimingEcho | Rejected::Looping => false,
            Rejected::TooQuiet { .. } | Rejected::Invented { .. } | Rejected::TooBrief { .. } => {
                true
            }
        }
    }

    /// The reason on its own, for when the words are already on the line above.
    pub fn reason(&self, spoken: Duration) -> String {
        match self {
            Rejected::KnownArtefact => "a known whisper artefact".to_string(),
            Rejected::PrimingEcho => "the transcriber reciting its own --vocab".to_string(),
            Rejected::TooQuiet { peak } => format!("peak {peak:.4}, too quiet to trust"),
            Rejected::Invented { confidence } => {
                format!("confidence {confidence:.2}, the transcriber did not believe it")
            }
            Rejected::TooBrief { peak, .. } => format!(
                "{:.1}s at peak {peak:.3}, too brief and too faint",
                spoken.as_secs_f32()
            ),
            Rejected::Looping => "the transcriber repeating itself".to_string(),
        }
    }

    /// What the panel says, including the words that were thrown away.
    ///
    /// Quoting the text matters more than it looks. A real interview dropped
    /// ten of the candidate's utterances as brief-and-faint, and the archive
    /// recorded only the durations and peaks — so afterwards there was no way
    /// to tell whether jay had binned ten "mm-hm"s or ten sentences he had
    /// actually said. A filter you cannot audit is a filter you cannot tune.
    ///
    /// The dropped text lands in the panel and the archive; it never reaches
    /// the model's context, which is the thing being protected.
    pub fn notice(&self, spoken: Duration, text: &str) -> String {
        match self {
            Rejected::KnownArtefact => format!("dropped a known whisper artefact: {text:?}"),
            Rejected::TooQuiet { peak } => format!(
                "heard {:.1}s at peak {peak:.4} — too quiet to trust, dropped: {text:?}",
                spoken.as_secs_f32()
            ),
            Rejected::Invented { confidence } => format!(
                "dropped {:.1}s the transcriber did not believe \
                 (confidence {confidence:.2}): {text:?}",
                spoken.as_secs_f32()
            ),
            Rejected::PrimingEcho => {
                format!("dropped the transcriber reciting its own --vocab back: {text:?}")
            }
            Rejected::TooBrief { spoken, peak } => format!(
                "dropped {:.1}s at peak {peak:.3} — too brief and too faint: {text:?}",
                spoken.as_secs_f32()
            ),
            Rejected::Looping => {
                format!("dropped the transcriber repeating itself: {text:?}")
            }
        }
    }
}

/// How many times one clause may repeat before the line is a decoder loop.
///
/// Three, so "no, no, no" and "yeah, yeah, yeah" survive — both are ordinary
/// speech and both appear verbatim in real transcripts from this machine. Four
/// identical clauses in a row is not emphasis; it is whisper looping on
/// near-silence:
///
/// ```text
/// [00:29–00:38] you: I'm not sure what to say. I'm not sure what to say. I'm
///                    not sure what to say. I'm not sure what to say. I'm not
///                    sure what to say.
/// ```
///
/// Nobody said that. Nothing was playing. It cleared the artefact list, the
/// level floor and the confidence floor, because all three ask what the words
/// *are* and none of them asks whether the line eats itself.
const MAX_CLAUSE_REPEATS: usize = 3;

/// Fewest characters a clause needs before repeating it means anything.
///
/// Below this the repetition is punctuation as much as language — "Ah. Ah. Ah.
/// Ah." is a person reacting, and a filter that bins it is editing the meeting
/// rather than transcribing it.
const REPEAT_MIN_CLAUSE: usize = 12;

/// Is this line the decoder looping on itself?
///
/// Splits on sentence punctuation rather than on words, because the unit that
/// loops is the clause. People repeat words constantly and whole clauses almost
/// never.
pub fn is_looping(text: &str) -> bool {
    let clauses: Vec<String> = text
        .split(['.', '!', '?'])
        .map(|clause| {
            clause
                .trim()
                .to_lowercase()
                .chars()
                .filter(|c| c.is_alphanumeric() || c.is_whitespace())
                .collect::<String>()
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|clause| clause.len() >= REPEAT_MIN_CLAUSE)
        .collect();

    // Consecutive runs only. The same sentence twice at either end of a long
    // answer is a person making a point; back to back it is a loop.
    let mut run = 1;
    for pair in clauses.windows(2) {
        if pair[0] == pair[1] {
            run += 1;
            if run > MAX_CLAUSE_REPEATS {
                return true;
            }
        } else {
            run = 1;
        }
    }
    false
}

/// Decide whether one transcript is worth keeping.
///
/// Every reason a transcript gets binned lives here, because they used to live
/// in two separate `continue`s in the capture loop and a third was needed.
/// `speech_peak` is the loudest frame the VAD called speech; a caller with no
/// VAD should pass [`f32::INFINITY`] rather than zero, since the check is a
/// lower bound and zero would reject everything.
pub fn judge(
    transcription: &Transcription,
    speech_peak: f32,
    spoken: Duration,
) -> Option<Rejected> {
    if is_hallucination(&transcription.text) {
        return Some(Rejected::KnownArtefact);
    }
    if transcription.prompt_echo {
        return Some(Rejected::PrimingEcho);
    }
    // Before the level and confidence floors, because a loop clears both. The
    // run that produced this scored well enough on every measure of what the
    // words were; the only thing wrong with it was that it said the same thing
    // five times.
    if is_looping(&transcription.text) {
        return Some(Rejected::Looping);
    }
    if speech_peak < SPEECH_PEAK_FLOOR {
        return Some(Rejected::TooQuiet { peak: speech_peak });
    }
    if transcription.confidence < CONFIDENCE_FLOOR {
        return Some(Rejected::Invented {
            confidence: transcription.confidence,
        });
    }
    // Short *and* faint. Either on its own is ordinary: a real answer can be
    // two seconds, and a long utterance can be quiet. Together they are the
    // signature of a room being captioned.
    if spoken < BRIEF_UTTERANCE && speech_peak < QUIET_SPEECH_PEAK {
        return Some(Rejected::TooBrief {
            spoken,
            peak: speech_peak,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spoken(text: &str, no_speech: f32, confidence: f32) -> Transcription {
        Transcription {
            text: text.to_string(),
            inference: Duration::from_millis(1),
            no_speech,
            confidence,
            prompt_echo: false,
        }
    }

    /// Long enough that the brief-and-faint rule never fires. Most of these
    /// tests are about the other rules and should not have to care.
    const SAID: Duration = Duration::from_secs(9);

    /// The real line, from the session that produced this filter. Nothing was
    /// said and nothing was playing.
    const LOOP: &str = "I'm not sure what to say. I'm not sure what to say. I'm not \
        sure what to say. I'm not sure what to say. I'm not sure what to say.";

    #[test]
    fn the_word_html_namespace_is_an_artefact() {
        assert!(is_hallucination(
            "urn:schemas-microsoft-com:office:smarttags City urn:schemas-microsoft-com:office:smarttags"
        ));
    }

    #[test]
    fn a_decoder_looping_on_silence_is_rejected() {
        assert!(is_looping(LOOP));
        assert_eq!(
            judge(&spoken(LOOP, 0.0, 0.9), 0.5, SAID),
            Some(Rejected::Looping),
            "a loop that clears every level and confidence floor still got through"
        );
    }

    /// The point of the filter is what it does *not* catch. Every line here is
    /// real speech from a real transcript on this machine.
    #[test]
    fn ordinary_repetition_is_not_a_loop() {
        for line in [
            "Yeah, yeah, yeah.",
            "No. No. No.",
            "Good day, good day.",
            "Okay. Okay. Very good. Very good. So yeah, Aaron is going to be our onboarding buddy.",
            "Glad to hear. Glad to hear. Yes. Very good. Very good. Thank you.",
            "I think we should shard by tenant. A bigger box only buys six weeks.",
        ] {
            assert!(!is_looping(line), "treated real speech as a loop: {line:?}");
        }
    }

    /// Three is emphasis, four is a loop. The boundary is the whole design, so
    /// it is asserted rather than left to the constant.
    #[test]
    fn three_repeats_survive_and_four_do_not() {
        let clause = "the counters drift under load";
        let three = format!("{clause}. {clause}. {clause}.");
        let four = format!("{clause}. {clause}. {clause}. {clause}.");
        assert!(!is_looping(&three), "three repeats should survive");
        assert!(is_looping(&four), "four repeats should not");
    }

    /// The vocabulary from the session that produced the bug.
    const PRIMED: &str = "Likely terms: linked list, binary tree, hash map, \
        big O, Postgres, Kafka, Redis, S3. Also: pastebin, base62, flood fill, \
        union-find, usize, HAProxy, nginx, MinIO, Patroni, Redis, Kafka";

    /// A 75-minute meeting lost two real sentences to substring matches on
    /// "the end" and ".com". Every line here is the shape of ordinary
    /// technical speech, rewritten from that recording rather than quoted
    /// from it, and every one of them was silently deleted.
    #[test]
    fn ordinary_speech_that_happens_to_contain_a_sign_off_survives() {
        for said in [
            "at the end of the day we should just cache it and move on",
            "hit the endpoint and see what comes back before we change anything",
            "the end user never sees any of that, it is all server side",
            "there is a write-up at nuthatch-indexer.com if you want the detail",
            "the spec lives on www.example.com and it has not been updated since March",
            "so the end state is one collection with two levels of abstraction in it",
        ] {
            assert!(!is_hallucination(said), "deleted real speech: {said}");
        }
    }

    /// The same words, short and alone, are still what whisper writes when
    /// handed a silent room.
    #[test]
    fn a_bare_sign_off_is_still_an_artefact() {
        assert!(is_hallucination("And that's the end."));
        assert!(is_hallucination("See you next time!"));
        assert!(is_hallucination("Bye bye."));
        assert!(is_hallucination("www.mooji.org"));
    }

    /// Subtitle credits never appear in a meeting, so they are caught wherever
    /// they sit and however long the line is.
    #[test]
    fn credits_are_caught_at_any_length() {
        assert!(is_hallucination(
            "And that is it for this talk, thanks for watching, do let me know \
             in the comments below what you would like to see covered next"
        ));
        assert!(is_hallucination("Subtitles by the Amara.org community"));
    }

    #[test]
    fn the_transcriber_reciting_its_own_vocabulary_is_caught() {
        // Verbatim from the archive of a real session, twice on a silent
        // channel. The double space is how it arrived.
        assert!(is_prompt_echo("Redis  Kafka", PRIMED));
        assert!(is_prompt_echo("MinIO, Patroni", PRIMED));
        assert!(is_prompt_echo("hash map, big O", PRIMED));
    }

    /// Also verbatim, and the reason contiguity was not enough: this one skips
    /// "Redis" out of the middle of the list it is reciting.
    #[test]
    fn a_recitation_that_skips_a_term_is_still_a_recitation() {
        const SHORT: &str = "pastebin, base62, Postgres, nginx, Redis, Kafka";
        assert!(is_prompt_echo("Postgres  nginx  Kafka", SHORT));
        assert!(is_prompt_echo("pastebin, Kafka", SHORT));
    }

    /// Order is the whole discriminator, so backwards must not match.
    #[test]
    fn the_same_terms_in_another_order_are_not_a_recitation() {
        const SHORT: &str = "pastebin, base62, Postgres, nginx";
        assert!(!is_prompt_echo("nginx Postgres", SHORT));
    }

    #[test]
    fn real_speech_using_those_words_is_left_alone() {
        // The words are primed; the sentence is not the prompt.
        assert!(!is_prompt_echo("we would put Redis in front of it", PRIMED));
        assert!(!is_prompt_echo("Kafka and Redis", PRIMED));
        assert!(!is_prompt_echo("so, Postgres", PRIMED));
        // One word is never enough. Somebody may simply have said it.
        assert!(!is_prompt_echo("Postgres", PRIMED));
        assert!(!is_prompt_echo("nginx", PRIMED));
    }

    #[test]
    fn a_primed_echo_is_binned_even_though_it_reads_confidently() {
        let mut echo = spoken("Redis Kafka", 0.0, 0.97);
        echo.prompt_echo = true;
        assert_eq!(judge(&echo, 0.4, SAID), Some(Rejected::PrimingEcho));
    }

    #[test]
    fn judge_keeps_confident_speech_and_bins_the_rest() {
        let real = spoken("reverse a singly linked list in place", 0.02, 0.93);
        assert_eq!(judge(&real, 0.4, SAID), None);

        assert_eq!(
            judge(&spoken("[BLANK_AUDIO]", 0.0, 1.0), 0.4, SAID),
            Some(Rejected::KnownArtefact)
        );
        assert_eq!(
            judge(&real, 0.001, SAID),
            Some(Rejected::TooQuiet { peak: 0.001 })
        );
        // Loud enough, not a known phrase, and written by a decoder that did
        // not believe a word of it.
        assert!(matches!(
            judge(&spoken("Cool? Distinct.", 0.91, 0.34), 0.4, SAID),
            Some(Rejected::Invented { .. })
        ));
    }

    /// The signal is dead on `.en` weights, so nothing may depend on it. If a
    /// future model makes it live, this test is where to notice.
    #[test]
    fn no_speech_is_reported_but_never_decides() {
        let certain_silence = spoken("a sentence from nowhere", 1.0, 0.99);
        assert_eq!(judge(&certain_silence, 0.4, SAID), None);
    }

    /// The artefact check runs first so its notice is the specific one, and a
    /// quiet artefact does not get reported as merely quiet.
    #[test]
    fn reasons_are_reported_most_specific_first() {
        assert_eq!(
            judge(&spoken("Thank you.", 0.99, 0.1), 0.0001, SAID),
            Some(Rejected::KnownArtefact)
        );
    }

    /// The rule that finally caught the inventions, and the measurements it
    /// was set from: real speech ran 8.4-15.4s, every invention 1.25-3.14s.
    /// Both conditions must hold, because a real two-second answer spoken at
    /// the laptop is short but not faint.
    #[test]
    fn brief_and_faint_together_are_not_speech() {
        let text = spoken("ive still packed my tounou manu", 0.0, 0.83);
        assert!(matches!(
            judge(&text, 0.0115, Duration::from_millis(1600)),
            Some(Rejected::TooBrief { .. })
        ));

        // Short, but spoken at the machine: a real answer.
        assert_eq!(judge(&text, 0.09, Duration::from_millis(1600)), None);
        // Faint, but sustained: the interviewer across a room.
        assert_eq!(judge(&text, 0.0115, Duration::from_secs(9)), None);
    }

    /// The peaks of ten utterances a real candidate actually said, every one of
    /// which the old 0.03 threshold threw away. None of them may be dropped.
    #[test]
    fn a_normal_speaking_voice_is_never_too_faint() {
        let said = spoken("we can route that block to a dead letter queue", 0.0, 0.9);
        for peak in [0.018, 0.020, 0.021, 0.023, 0.027, 0.028, 0.029] {
            assert_eq!(
                judge(&said, peak, Duration::from_millis(1500)),
                None,
                "dropped a real utterance peaking at {peak}"
            );
        }
    }

    /// And the drop still says what it threw away, so the next calibration is
    /// evidence rather than another guess.
    #[test]
    fn a_drop_quotes_the_words_it_binned() {
        let notice = Rejected::TooBrief {
            spoken: Duration::from_millis(1500),
            peak: 0.009,
        }
        .notice(Duration::from_millis(1500), "ive got this");
        assert!(notice.contains("ive got this"), "{notice}");
    }

    /// A backend without confidence signals reports 0.0/1.0, which must read as
    /// "no opinion" rather than as a reason to bin everything it produces.
    #[test]
    fn a_backend_with_no_confidence_signal_is_not_penalised() {
        assert_eq!(judge(&spoken("a real sentence here", 0.0, 1.0), 0.4, SAID), None);
    }

    #[test]
    fn recognises_the_usual_whisper_artefacts() {
        assert!(is_hallucination(""));
        assert!(is_hallucination("   "));
        assert!(is_hallucination("[BLANK_AUDIO]"));
        assert!(is_hallucination("Thank you."));
        assert!(is_hallucination("you"));
    }

    /// Real output from a 90-second recording of an empty room.
    #[test]
    fn catches_the_youtube_outros_whisper_invents_from_silence() {
        for line in [
            "I'll see you next time.",
            "Thanks for watching!",
            "Thank you for watching.",
            "Don't forget to subscribe!",
            "Bye bye.",
            "[typing]",
            "(door closes)",
            // The subtitle dash, which walked through the bracket check on a
            // real session because the line does not begin with a bracket.
            "- (speaking in foreign language)",
            "-[BLANK_AUDIO]",
            "- (music) .",
        ] {
            assert!(is_hallucination(line), "should have been dropped: {line}");
        }
    }

    #[test]
    fn leaves_real_speech_alone() {
        assert!(!is_hallucination("thank you for the detailed explanation"));
        assert!(!is_hallucination("why is this test failing"));
        // "you" alone is an artefact; "you" in a sentence plainly is not.
        assert!(!is_hallucination("you were right about the lock"));
        // "see you next time" is an artefact; this is a real sentence that
        // happens to be about subscribing.
        assert!(!is_hallucination(
            "the subscriber sees events in the order the log wrote them"
        ));
    }
}
