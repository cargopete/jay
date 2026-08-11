# jay

A consented, local-first listening assistant.

Named for the bird that sits quietly in the canopy watching, and calls the moment
something is wrong. That is roughly the job description.

## What it is

jay listens to what you say and to what your machine is playing, keeps a running
transcript locally, and offers help when help is worth having. Three modes share
the same pipeline:

- **Pairing and coaching.** A second pair of ears on a call or a working session.
  Suggests approaches, points at the thing you have not considered, and shuts up
  otherwise.
- **Mock interview rehearsal.** You practise out loud; jay tracks the question,
  offers talking points and an outline to think with, and tells you afterwards
  what your answer actually missed. Prompts to think with, not a script to read.
- **Proactive dev assistant.** Deterministic triggers rather than a chatbot: a
  test goes red, a stack trace appears, CI fails. jay reacts to the event.

## What it is not

jay does not hide. There are no capture-exclusion flags, no compositor tricks,
no attempt to be invisible to a screen share. That machinery is fragile anyway
(Apple broke `NSWindow.sharingType = .none` for ScreenCaptureKit in macOS 15.4,
and there is currently no public API that replaces it), but the reason it is
absent is simpler than that: a tool that helps you think should be one you can
admit to using.

Everything runs on your machine by default. Audio and transcripts do not leave
it unless you configure a cloud backend and say so.

## Status

Early. Stage 1 is capture, transcription and an overlay.

| Piece | State |
| --- | --- |
| Microphone capture (cpal, 16 kHz mono) | Working, smoke-tested on an M3 Pro |
| Silero VAD segmentation | Working, weights compiled in |
| Local STT (whisper.cpp via Metal) | Working, `base.en` at ~40x real time |
| Live mic transcription | Wired, not yet tested against a real voice |
| Transparent overlay | Wired to the live transcript; visual design unreviewed |
| Suggestion gate (rules, free) | Working |
| Live pipeline: listen → gate → suggest → panel | Working, tested end to end |
| Hand-asked suggestions (button + screenshot) | Built, button untested by a human |
| Speaker-aware gating, standing brief | Working, validated on a real transcript |
| Suggestions via the Max subscription | Working, measured |
| System audio capture (CoreAudio process taps) | Working, via a LaunchServices launch |
| Screen capture on escalation | Built, permission path shared with audio |

## Layout

```
crates/
  jay-audio   capture, downmix, resampling, VAD
  jay-stt     the SpeechModel trait and its backends
  jay-ui      the overlay
  jay         the binary that wires it together
```

## Trying it

```sh
cargo run -p jay -- check                # permissions, weights, subscription
cargo run -p jay -- devices              # what inputs can jay see
cargo run -p jay -- listen --seconds 6   # capture smoke test with levels
cargo run -p jay -- transcribe           # live transcription from the mic
cargo run -p jay -- file talk.wav        # transcribe a 16 kHz mono WAV
cargo run -p jay -- transcribe --overlay # live transcript in a floating panel
cargo run -p jay -- transcribe --assist  # …and suggestions when asked a question
cargo run -p jay -- ask "why is this failing?" --mode dev
cargo run -p jay -- ask "what is wrong here?" --mode dev --screen
cargo run -p jay -- transcribe --overlay --brief job-spec.md
cargo run -p jay -- transcribe --assist --mode interview --brief cv.md
```

### Modes

| Mode | For | Shape |
| --- | --- | --- |
| `coding` | Practising a LeetCode-shaped round | Approach, compiling code, complexity, the edge cases a first attempt misses. |
| `system-design` | Practising a design round | Capacity numbers, an ASCII component diagram, then the decisions and what each traded away. |
| `rehearsal` | The debrief afterwards | What your attempt missed, quoted back, then the full worked answer. |
| `pairing` | Working with a colleague | Concrete and opinionated. |
| `dev` | A test went red | What is likely responsible and what to check first. |

Add `--hint` to any of them to be nudged instead of answered: the approach and
the complexity, under forty words, no implementation. Use it when you want the
rep rather than the answer.

Length costs latency, so `--hint` is fast: measured on the same question, a
full answer took 17.9s and a hint 5.4s, because the time goes on generating
rather than thinking.

### Sessions

Every `transcribe` run archives itself to a timestamped file under
`~/Library/Application Support/jay/sessions/` — the conversation, what jay said,
elapsed times, and the cost of each suggestion. No flag required, because a
feedback loop that depends on remembering a flag is a loop that does not run.

It is written to be replayed, not just read: `jay ask --context <session>` puts
any moment back through the real prompt path, which is the only honest way to
tell whether a change to the prompts actually helped.

### Context

jay keeps the whole session — 600 lines — and chooses what to send at ask time.
The problem statement is **pinned** the moment the interviewer says it, because
it is spoken once and would otherwise scroll out of the window exactly when the
questions get specific. The rest is a 1,200-word budget spent newest-first, with
pure acknowledgements dropped: "Okay. Okay. Yeah. All right." costs tokens and
carries nothing. The filter errs towards keeping — "Yes. Later today. Yeah."
looks like noise and actually commits to a time.

Note this is the opposite of the conclusion for `--brief`, where more context
measurably made answers *worse*. Everything in the conversation is about the
thing at hand; most of a 181-project memory index is not.

**Give it a brief.** `--brief <file>` is standing context for the whole
session — a job spec, your CV, the RFC you are pairing on, notes on the
architecture. It is the cheapest large gain available: without it every
suggestion is reasoned from a dozen lines of transcript and reads generic. It
leads the prompt so it forms a stable prefix that prompt caching serves at a
tenth of the price.

**It knows who is talking.** Questions from the other side (system audio) can
escalate; your own speech is recorded as context but never treated as a request
for help, because a question you mutter while thinking is thinking aloud. Saying
"hey jay" overrides that.

The panel has an **ask jay** button, and that is the primary way to get a
suggestion. Pressing it sends the recent conversation *and a screenshot of the
focused window*, taken at that moment, which is almost always the thing being
discussed. Nothing is spent that you did not ask for, and the twelve-second
wait is a great deal easier to live with when you chose it.

`--assist` is the *automatic* gate on top of that: jay escalates by itself when
it hears a question. Off by default, because listening is free and suggesting is
not. Two guards apply either way: a `--cooldown` (30s) so three questions in
quick succession are not three simultaneous escalations, and a `--budget`
($2.00) beyond which jay keeps listening but stops suggesting. The budget is a
soft ceiling — it is checked before a call, not during one, so a session can
overshoot by the cost of the call in flight.

Anything touching system audio or the screen must be launched through
LaunchServices, or macOS silently withholds permission:

```sh
scripts/bundle.sh debug
open -a "$PWD/target/debug/jay.app" --args listen --source system --seconds 12 --out /tmp/jay.txt
```

`ask` runs the gate first, so it declines to spend anything on an utterance
that isn't a question. When it does escalate it reports the model, the latency
and the cost, because those numbers are what decide whether the idea works.

The first run of either transcribe command downloads whisper weights
(`base.en`, 142 MB) into the platform cache directory.

`listen` reports frames delivered, peak RMS, queue lag and dropped samples. A
peak RMS of zero with a healthy frame count means the device is handing over
silence, which almost always means microphone permission was refused. It says so
rather than looking successful.

## Design notes

Longer-form decisions and their reasoning live in [docs/design.md](docs/design.md).

## Licence

MIT or Apache-2.0, at your option.
