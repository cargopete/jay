# jay

A local-first listening assistant for practising technical interviews.

Named for the bird that sits quietly in the canopy watching, and calls the
moment something is worth calling about. That is roughly the job description.

---

## What it is

You are practising for interviews with someone playing interviewer — over a
call, doing the real thing properly. jay listens to both of you, keeps the
transcript locally, and when you press a button gives you the answer: working
code for an algorithmic question, an architecture diagram for a design question,
or a short nudge if you would rather work it out yourself.

Afterwards it runs the debrief. It quotes your attempt back, says what you
missed, then gives the answer you should have given.

Everything except the model call runs on your machine. Audio never leaves it.

### What it is not

**It does not volunteer.** jay listens, transcribes, and says nothing until you
press the button. No setting changes that. Nothing is spent that you did not ask
for, and a panel that only speaks when spoken to is one you can forget is
running.

**It does not hide.** The flag that used to exclude a window from screen capture
(`NSWindow.sharingType = .none`) stopped working for ScreenCaptureKit in macOS
15.4 and has no replacement, so a switch labelled "invisible" would be a lie you
plan around. Share a *window* rather than the whole display and jay is
structurally out of frame.

---

## Quick start

Needs macOS 14.4+ (Apple silicon assumed), Rust, and the `claude` CLI signed in
— jay drives your Claude subscription rather than an API key.

```sh
git clone git@github.com:cargopete/jay.git ~/Projects/jay
cd ~/Projects/jay
cargo build --release
scripts/bundle.sh release
```

**Check permissions before anything else.** macOS grants these silently and
refuses them just as silently — an ungranted audio tap returns an unbroken
stream of zeros rather than an error.

```sh
open -a ~/Projects/jay/target/release/jay.app --args check --out /tmp/jay-check.txt
cat /tmp/jay-check.txt
```

You want five OKs:

```
  mic       OK   MacBook Pro Microphone
  system    OK   tap running at 48000 Hz
  screen    OK   captured 676 KB
  whisper   OK   …/ggml-small.en.bin
  claude    OK   4.4s, $0.0036 — cache is now warm
```

If `screen` or `system` fails, jay has just asked macOS for that permission, so
it now appears in **System Settings › Privacy & Security › Screen & System Audio
Recording**. Tick it and run the check again. The first run also downloads the
whisper weights (466 MB) and warms the prompt cache, both worth doing before a
session rather than during one.

Then run it:

```sh
open -a ~/Projects/jay/target/release/jay.app --args \
  transcribe --overlay --source both --mode coding --seconds 0
```

A small dark panel appears above everything, draggable by its header. Talk.
Press **ask jay** when you want the answer. Close it with **×**.

> Use the absolute path. `open -a "$PWD/..."` only works if you are already in
> the repository, and fails confusingly if you are not.

---

## Why `open -a` and not just `cargo run`

Because of how macOS decides who is asking for a permission.

A binary launched from a shell inherits the *responsible process* of whatever
owns that shell, so the request is attributed to your terminal rather than to
jay — and a denied audio tap returns silence instead of an error. Launched
through LaunchServices from the `.app` bundle, jay asks for itself and gets an
answer.

`scripts/bundle.sh` **copies** the binary into the bundle, so re-run it after
every `cargo build` or you will be running yesterday's code.

For anything needing only the microphone, a shell is fine:

```sh
cargo run --release -p jay -- transcribe --seconds 20
```

---

## Commands

| | |
| --- | --- |
| `jay check` | Try every permission, weight and credential for real. Run before a session. |
| `jay transcribe` | The main event: listen, show the panel, answer when asked. |
| `jay ask "<question>"` | One-shot, no audio. The fastest way to see what a mode gives you. |
| `jay brief --out brief.md` | Build standing context from your memory index. |
| `jay file talk.wav` | Transcribe a 16 kHz mono WAV — a recorded session, a talk. |
| `jay listen` | Capture smoke test: frame counts, levels, dropped samples. |
| `jay devices` | What inputs jay can see. |

### `transcribe`

```sh
jay transcribe --overlay --source both --mode coding \
  --brief brief.md --budget 2.00 --seconds 0
```

| Flag | |
| --- | --- |
| `--source` | `mic`, `system`, or `both`. `both` lets jay tell you apart. |
| `--overlay` | Floating panel instead of terminal output. |
| `--mode` | What kind of answer the button gives. See below. |
| `--brief` | Standing context for the session. |
| `--budget` | Dollars this session may spend. Default 2.00. |
| `--save` | Override where the session is archived. |
| `--seconds` | `0` runs until you close the panel. |
| `--model` | Whisper size: `tiny`, `base`, `small`. Default `small`. |

### `ask`

```sh
jay ask --mode system-design --brief brief.md \
  --context session.md --hint "How would you scale the read path?"
```

Same modes, plus `--context <file>` to supply a conversation and `--screen` to
send what is on screen. No gate and no button — you typed it, so you meant it.

---

## Modes

The two interview types want different things, and one mode cannot serve both.

| Mode | For | What you get |
| --- | --- | --- |
| `coding` | An algorithmic round | The approach in a sentence, complete compiling Rust with the invariant named in a comment, time and space complexity, and the edge cases a first attempt misses. |
| `system-design` | A design round | Capacity numbers first, an ASCII component diagram, each component in a line, then the decisions that matter and what each traded away. |
| `rehearsal` | The debrief afterwards | What your attempt missed, quoted back, then the full worked answer. |
| `pairing` | Working with a colleague | Concrete, opinionated, short. Will happily give you SQL. |
| `dev` | A test went red | What is likely responsible and the first thing worth checking. |

Add **`--hint`** to any of them to be nudged instead of answered: the approach,
the complexity, or the thing you are about to miss, in under forty words with no
implementation. Use it when you want the rep rather than the answer.

Hints are also about three times faster — 5.4s against 17.9s on the same
question — because latency is dominated by how much gets generated rather than
by thinking.

---

## How it works

```
mic ─────┐                                         ┌── panel
         ├── 16 kHz mono ── VAD ── whisper ── transcript ─┤
system ──┘                                         └── archive
                                                          │
                            [ask jay] ────────────────────►│── + problem
                                                           │  + brief
                                                           │  + screenshot
                                                           ▼
                                                        claude
```

**Capture.** `cpal` for the microphone; a CoreAudio process tap behind an
Objective-C shim for system audio, so the other person's voice arrives on its
own channel. Both resample to 16 kHz mono.

**Segmentation.** Silero VAD, weights compiled into the binary, decides what is
speech. Frames are 512 samples because Silero v5 accepts exactly that at 16 kHz
and rejects anything else — the pipeline is framed to suit the VAD rather than
the other way round.

**Transcription.** whisper.cpp on Metal, `small.en` by default. Small is worth
the download: mishearing "idempotency" or "jemalloc" poisons every suggestion
downstream, and the extra decode time is nothing beside the model call.

**Context.** jay keeps 600 lines and chooses what to send when you press. The
problem statement is **pinned** the moment the interviewer says it, because it
is spoken once and would otherwise scroll away exactly when the questions get
specific. The rest is a 1,200-word budget spent newest-first, with pure
acknowledgements dropped — "Okay. Okay. Yeah. All right." costs tokens and
carries nothing.

**The answer.** Recent conversation, the pinned problem, your brief and a
screenshot of the display, through `claude -p` on your subscription.

The screenshot is scaled to 1800px on the long edge and written as JPEG, which
took it from 8.5 MB to 676 KB. The model's high-resolution tier caps at 2576px
anyway, so everything above that was upload time spent on pixels nobody reads,
and PNG's losslessness buys nothing when the reader is a model looking at a
stack trace.

---

## Sessions

Every run archives itself to a timestamped file under
`~/Library/Application Support/jay/sessions/`. No flag required, because a
feedback loop that depends on remembering a flag is a loop that does not run.

The file holds the conversation, everything jay said, a clock stamp on each
line, and what each suggestion cost. It is written to be *replayed*, not merely
read:

```sh
jay ask --mode rehearsal --brief brief.md \
  --context ~/Library/Application\ Support/jay/sessions/<session>.md \
  "Count the number of islands in a grid."
```

That is the only honest way to tell whether a change to the prompts helped,
rather than whether it reads better.

---

## Costs

Measured on an M3 Pro, driving a Max subscription.

| | |
| --- | --- |
| A hint | ~5s, ~$0.14 |
| A full answer with code or a diagram | ~16–20s, ~$0.20 |
| Idle, listening | 12–25% of one core, ~19 MB |

Every `claude -p` call carries roughly 29,000 tokens of the CLI's own preamble
regardless of how small your question is: $0.0254 on a cold cache, $0.0033 once
warm, for a one-word answer. That single fact shaped the design. It is why jay
has no model deciding when to speak — such a gate would run about $0.40 an hour
to answer yes or no — and why a button is the trigger instead.

`--budget` stops suggestions once a session has spent it. It is a soft ceiling:
checked before a call rather than during one, so a session overshoots by
whatever was in flight.

---

## Troubleshooting

**"cannot be opened … no such file".** `$PWD` was not the repository. Use the
absolute path: `open -a ~/Projects/jay/target/release/jay.app --args …`

**The panel says nothing.** Correct, until you press the button.

**No `them:` lines.** You launched from a shell rather than through `open -a`,
so macOS is feeding the tap silence. Re-run `jay check`.

**Every question appears twice, once as `you:`.** Echo — the other person's
voice is leaving your speakers and returning through your microphone. Wear
headphones.

**"still thinking about the last one".** One suggestion runs at a time, by
design: the alternative is the transcriber stalling behind it and losing audio.

**Sentences nobody said.** Whisper invents fluent text from silence, reaching
for the subtitled video it was trained on — "I'll see you next time" appeared in
90 seconds of an empty room. Known artefacts are filtered and transcripts whose
audio was never loud enough to be speech are dropped, but some get through.

**Jargon mangled.** `--model small` is the default and the best wired up.

---

## Status

Working and measured: capture on both channels, VAD, transcription, the gate,
context selection, every mode, screen capture, session archiving, cost and
latency. Nine test suites.

Not yet exercised by a human: **the button has never been pressed**, and
**nobody has looked at the panel**. There has been no forty-minute run, and jay
has never heard a real human voice — every word it has transcribed came from
macOS `say` or a speaker playing into a microphone.

---

## Layout

```
crates/
  jay-audio   capture, resampling, VAD, the macOS shim
  jay-stt     the SpeechModel trait and whisper.cpp behind it
  jay-agent   the gate, context selection, prompts, screen capture, archiving
  jay-ui      the panel
  jay         the binary that wires it together
docs/
  design.md         decisions and why, including what went wrong
  mock-session.md   a runbook for an actual practice session
```

## Licence

MIT or Apache-2.0, at your option.
