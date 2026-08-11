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
cargo run -p jay -- devices              # what inputs can jay see
cargo run -p jay -- listen --seconds 6   # capture smoke test with levels
cargo run -p jay -- transcribe           # live transcription from the mic
cargo run -p jay -- file talk.wav        # transcribe a 16 kHz mono WAV
cargo run -p jay -- transcribe --overlay # live transcript in a floating panel
cargo run -p jay -- transcribe --assist  # …and suggestions when asked a question
cargo run -p jay -- ask "why is this failing?" --mode dev
cargo run -p jay -- ask "what is wrong here?" --mode dev --screen
```

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
