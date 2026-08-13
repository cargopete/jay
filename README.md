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
open -n -a ~/Projects/jay/target/release/jay.app --args check --out /tmp/jay-check.txt
cat /tmp/jay-check.txt
```

You want five OKs:

```
  mic       OK   46 frames, peak 0.0028 RMS
  system    OK   tap at 48000 Hz, 0 frames, peak 0.0000 RMS
  screen    OK   captured 676 KB
  whisper   OK   …/ggml-medium.en.bin
  claude    OK   4.4s, $0.0036 — cache is now warm
```

The microphone line is a real three-second recording, not a device listing.
Say something during the check and the peak should jump above 0.02; a room at
rest reads around 0.003. **Exact zeros are a refused permission, not a quiet
room** — macOS answers a denied microphone with perfect silence rather than an
error, so the only way to tell is to look at the samples.

If `screen` or `system` fails, jay has just asked macOS for that permission, so
it now appears in **System Settings › Privacy & Security › Screen & System Audio
Recording**. Tick it and run the check again. The first run also downloads the
whisper weights (1.5 GB) and warms the prompt cache, both worth doing before a
session rather than during one.

Then run it:

```sh
open -n -a ~/Projects/jay/target/release/jay.app --args \
  transcribe --overlay --source both --mode coding --seconds 0
```

A small dark panel appears above everything, draggable by its header. Talk.
Press **ask jay** when you want the answer. Close it with **×**.

It is in two halves. The **reading** holds the top and stays where you put it,
starting at the beginning of each new answer. The **conversation** runs along
the bottom and chases itself. They were one pane until it became clear that the
"Approach:" line — the sentence you are meant to say out loud first — had
scrolled off the top by the time the code finished arriving, pushed away by
every transcript line that landed while you read.

Under the meters is a switch bank. **ROUND** picks what the lever answers for —
`CODE`, `DESIGN`, `DEBRIEF`, `PAIR`, `DEV` — and **GIVES** picks `ANSWER` or
`NUDGE`. Throwing either starts a fresh Claude process, so the next press pays
the 4.7 second startup again; that is the price of not quitting jay in the
middle of an interview, which is what changing rounds used to cost. `--mode`
still sets where it starts.

> Use the absolute path. `open -n -a "$PWD/..."` only works if you are already in
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
| `jay demo` | Draw the panel with sample content and no audio. `--state empty\|writing\|answered`. |
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
| `--mode` | Which round to start in. Switchable in the panel afterwards. |
| `--brief` | Standing context for the session. |
| `--budget` | Stop suggesting after this many dollars. No limit by default. |
| `--save` | Override where the session is archived. |
| `--seconds` | `0` runs until you close the panel. |
| `--model` | `tiny`, `base`, `small`, `medium`, `turbo`. Default `medium`. |
| `--vocab` | Extra words to expect, comma separated. Primes the transcriber. |

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

Set **GIVES** to `NUDGE` in the panel, or add **`--hint`** to `jay ask`, to be
nudged instead of answered: the approach, the complexity, or the thing you are
about to miss, in under forty words with no implementation. Use it when you
want the rep rather than the answer.

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

**Transcription.** whisper.cpp on Metal, `medium.en` by default. Measured on
one jargon-heavy question:

| | inference | speed | got wrong |
| --- | --- | --- | --- |
| `small.en` | 783ms | 26.6× | "**Write**, so" for "Right, so"; one run-on sentence |
| `medium.en` | 1720ms | 12.6× | nothing that changed the meaning |
| `large-v3-turbo` | 1426ms | 15.1× | "idempotent **rights**" for "writes" |

Turbo is faster than medium and wrong in the more damaging direction, being
multilingual: a plausible wrong word is worse than an obviously wrong one. At
12.6× real time a 22 second question decodes in under two seconds, which is
nothing beside a nine second answer, so medium is the default.

None of them get *pastebin* or *jemalloc*, and a larger model will not fix a
name. That is what `--vocab` is for.

**Context.** jay keeps 600 lines and chooses what to send when you press. The
problem statement is **pinned** the moment the interviewer says it, because it
is spoken once and would otherwise scroll away exactly when the questions get
specific. The rest is a 1,200-word budget spent newest-first, with pure
acknowledgements dropped — "Okay. Okay. Yeah. All right." costs tokens and
carries nothing.

**Levels.** Two meters above the transcript, fed by the RMS of the actual
samples, with the VAD's speech decision beside them. They exist because there
are about ten seconds between a sound arriving and a sentence appearing, and
until there was a meter, a dead microphone and a quiet room looked identical
for all ten of them.

**The answer.** Recent conversation, the pinned problem, your brief and a
screenshot of the display, through `claude -p` on your subscription.

The screenshot is scaled to 1800px on the long edge and written as JPEG, which
took it from 8.5 MB to 676 KB, and goes in as an inline image block rather than
a path for the model to open with `Read`. That tool call was a whole extra
round trip — measured at about four seconds, the same as the entire spawn and
preamble floor — spent fetching a file jay already had in hand. It also means
no tools need be enabled at all.

**The press drains first.** Pulling the lever closes whatever sentence is
mid-flight and waits, up to 1.5s, for it to be transcribed before reading the
conversation. Without that, the sentence least likely to be in the transcript
is the one just spoken, which is the one being asked about.

**Streaming.** The answer is painted as it is written. Total time is unchanged;
the first words land at about five seconds instead of the whole thing at
fourteen, and five seconds into a conversation you can still use what you
read.

**One process per session.** The CLI is spawned once and kept, so only the
first press pays the 4.7 second spawn-and-preamble toll — measured at 3.1s for
the first answer and 1.7s for the second, including a 75 second idle gap
between them. The process also keeps the conversation, so the third question of
a round is asked of something that heard the first two. If it dies, jay
restarts it and asks again once.

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
| Spawn, preamble and one round trip | 4.7s — the floor, before any answer |
| A hint | ~5s, ~$0.18 |
| A coding answer with working Rust | ~9s, ~$0.19, first words at ~5s |
| A design answer | ~16s, ~$0.20 |
| A rehearsal debrief | ~53s, ~$0.28 — uncapped by design, run after the round |
| Idle, listening | ~0.2% of one core, 1.87 GB resident |

A hint is roughly twice as fast as a coding answer and three times as fast as a
design one, and barely cheaper than either, because the preamble below dominates
the bill whatever you ask for. Buy hints for the tempo, not for the money.

That resident figure is almost entirely `medium.en`, and it is the number to
plan a laptop around: 1.87 GB, flat to the megabyte across a twenty minute run
with no leak and no growth. Idle cost is otherwise nil — two tenths of one core
in a silent room, because the VAD does almost nothing until somebody speaks.
Drop to `--model small` if you need the gigabyte back and can accept
"**Write**, so" for "Right, so".

That coding figure was 77 seconds until the prompt was given a hard length
cap. Latency here is almost entirely output length: the model is not thinking
for longer, it is writing more. An answer with an alternatives section and an
aside about what the interviewer might prefer is not a better answer to read
mid-interview, and it costs a minute.

Every `claude -p` call carries roughly 29,000 tokens of the CLI's own preamble
regardless of how small your question is: $0.0033 once warm for a one-word
answer, and on a cold cache anywhere from $0.0254 to $0.0590 — three runs of
`jay check` on this machine gave $0.0254, $0.0590 and $0.0260, with no change to
jay between them. The spread is real and it is not a trend, so budget for the
high end and do not read one cheap run as a saving. The preamble belongs to the
CLI rather than to jay and moves under you, so treat every figure on this page
as the right order of magnitude and re-measure before you trust one. That single
fact shaped the design. It is why jay
has no model deciding when to speak — such a gate would run about $0.40 an hour
to answer yes or no — and why a button is the trigger instead.

`--budget` stops suggestions once a session has spent it. It is a soft ceiling:
checked before a call rather than during one, so a session overshoots by
whatever was in flight.

---

## Troubleshooting

**"cannot be opened … no such file".** `$PWD` was not the repository. Use the
absolute path: `open -n -a ~/Projects/jay/target/release/jay.app --args …`

**The panel says nothing.** Correct, until you press the button.

**No `them:` lines.** Two possibilities. If the `them` meter reads `NO FRAMES`,
you launched from a shell rather than through `open -n -a`, so the tap is
unauthorised — re-run `jay check`. If it reads `QUIET`, the tap is fine and
nothing is playing: **a process tap produces no callbacks at all on an idle
output**, so a silent machine and a broken tap look the same until sound
arrives. `jay check` reports zero frames on the system line for exactly this
reason; play something before you trust it.

**Nothing happens and the meters say `NO FRAMES`.** The capture never started
or has died. The panel will now also print `capture stopped: …` if the pipeline
itself failed. Before this, that error only went to a log, and an app bundle
has no terminal to log to, so the panel simply sat there looking ready.

**Two jay windows.** Don't. Two instances contend for the microphone and both
lose frames — measured at 222 of an expected 281 while sharing.

**A debug build hears nothing while the release build is fine.** Permission is
attached to the binary, not to the project, so `target/debug/jay` is a stranger
to macOS however many times you have granted `target/release/jay`. Observed in
the same minute: 498 frames of an expected 500 from the release binary, and 0
of 375 from the debug one, with no error from either. Test capture with the
release build, or grant the debug binary separately and expect to do it again
after `cargo clean`.

**Every question appears twice, once as `you:`.** Echo. The other person's
voice leaves your speakers and returns through your microphone, so it is
captured on both channels and one copy is blamed on you. Reproduced here
exactly:

```
[00:11] them: Given a two-dimensional grid of ones and zeros, find the largest island by area.
[00:11] you:  Given a two-dimensional grid of ones and zeros, find the largest island by area.
```

**Wear headphones.** This is not a tuning problem, it is a room.

jay now notices anyway. A line matching one from the other channel within four
seconds is dropped as an echo, and the copy kept is always the one from the
system tap, since that is the audio that did not cross a desk. If the microphone
copy was transcribed first and is already recorded, it is retracted from the
context by name. Nothing under eight words is ever treated as an echo, because
both people say "okay, yeah, that sounds right" constantly and editing a
conversation on that basis would be the worse fault. None of which is a reason
not to wear headphones.

**"still thinking about the last one".** One suggestion runs at a time, by
design: the alternative is the transcriber stalling behind it and losing audio.

**Sentences nobody said.** Whisper invents fluent text from silence, reaching
for the subtitled video it was trained on — "I'll see you next time" appeared in
90 seconds of an empty room. Known artefacts are filtered and transcripts whose
audio was never loud enough to be speech are dropped, but some get through.
Both kinds of drop now say so in the panel rather than only in a debug log.

Enough get through to matter. Thirty seconds of a silent room, on the `you`
channel, archived these two as things you said:

```
[00:21] you: -To us, Adam wanted to speak to us. -It's testing.
[00:24] you: Cool? Distinct.
```

That is the failure worth caring about, because a fabricated `you:` line is not
merely noise in the transcript — it is spent as context on the next press, and
the model has no way to know you never said it.

**They come from your speakers, not from silence.** This was measured both ways.
Twelve minutes of a real room with nothing playing produced an empty transcript
— not one invented line. Thirty seconds with the interviewer's question coming
out of the speakers produced four, all blamed on the candidate. It is the same
bleed that duplicates the question, one notch more degraded: when the microphone
copy transcribes faithfully you get the question twice, and when it does not you
get this.

**So headphones are not advice, they are the fix.** Nothing else here comes
close.

There is a third filter behind the phrase list and the level floor — the mean
probability of the tokens whisper emitted, rejected below 0.45 — and you should
know what it does not do. Measured on the run above, the invented lines scored
0.71 and 0.82 while the real question scored 0.87. A floor high enough to catch
them bins real speech. It stays because it is free and it catches the genuinely
unbelieved, but it is a backstop, not a solution.

So: read the conversation pane before you press, and treat a line you do not
recognise as a reason to press later rather than now.

The signal that *should* do this job, whisper's own `no_speech_prob`, is dead on
the English-only weights jay uses — it reads 0.00 even for eight seconds of pure
digital silence, because it is the probability of a token an `.en` vocabulary
never predicts. It is recorded and ignored rather than quietly trusted.

**You spoke and nothing appeared.** Watch the `you` meter. If it moves and says
`SPEECH`, jay heard you and the fault is downstream — look for a
`too quiet to trust` notice. If it says `NO INPUT`, the capture thread has
stopped. If it says `OFF`, that channel was never started, so check `--source`.

**Jargon mangled.** The transcriber is primed with the vocabulary of technical
interviews and of Rust, which is the difference between "reverse a singly
linked list" and "reverse the link please" — both real transcripts of the same
sentence, before and after. Add anything specific to your round with
`--vocab "SiloBin, Redpanda, Kademlia"`. `--model medium` is the default and the
best wired up.

---

## Status

Working and measured: capture on both channels, VAD, transcription, the gate,
context selection, every mode, screen capture, session archiving, cost and
latency. Nine test suites.

The button has been pressed, twice, and did the right thing both times: looked
at an empty screen, said there was no problem to work on, and declined to
invent one.

Rebuilt from a clean clone and put through every command that does not need a
second person: preflight green on all five lines, `medium.en` fetched and
decoding a jargon-heavy sentence verbatim at 11.5× real time, both channels
capturing with zero dropped samples and 340µs worst queue lag, the system tap
proven with real audio, all five modes answering in their documented shape, and
the Rust from a `coding` answer compiled and passed a test written against it,
empty list included.

Still not exercised: **a real conversation**. There has been no forty-minute
run and no session with two people in it. jay has never transcribed a live
human voice — every word it has handled came from macOS `say` or a speaker
played into a microphone.

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
