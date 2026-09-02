# jay

A local-first meeting transcriber for macOS. It records both halves of a call,
writes a timestamped transcript, and turns it into notes when the meeting ends.
The audio never leaves your machine.

Named for the bird that sits quietly in the canopy watching, and calls the
moment something is worth calling about. That is roughly the job description.

```sh
cargo run --release      # Ctrl-C when the meeting ends
```

What lands on disk while it runs:

```
[00:00] (listening on you: MacBook Pro Microphone, them: whatever plays through AirPods Pro.)
[00:21–00:33] them: The problem is a rate limiter, distributed across many nodes.
[00:34–00:36] you: Right.
[00:35–00:52] them: And I want the counters consistent without a round trip per request.
[01:04–01:06] ?you: token bucket, per tenant
[01:04] (kept the line above out of context — 2.1s at peak 0.009, too brief and too faint)
```

And what it writes beside it when you stop:

```markdown
## Decisions
- **Shard by contract, not a bigger box** — a bigger box buys ~6 weeks
  and that route has already been tried once [00:20–00:31]

## Actions
- **you** — do the shard key work, branch up by Wednesday [00:53–01:08]
- **them** — write the migration plan, present it Thursday [00:34–00:52]

## Open questions
- How do reorgs that straddle two shards get handled? [01:09–01:20]
```

Every line cites the moment it came from, because the timestamps are honest
enough to go back and listen to. `you` and `them` are two separate microphones
rather than one stream a diarizer has guessed at, so an action assigned to you
was said by you.

---

## What it is

**A transcriber.** `jay` on its own records your microphone and whatever is
playing through your speakers as two separate channels, so it knows which of
you said what without guessing at voices. Every line is stamped from when the
speech began. When you stop it, it writes the notes: what was decided, who owes
what, what nobody answered, each line citing the moment it came from.

Whisper runs locally. The notes are one call to Claude at the end — the only
thing that leaves the machine, and only ever the text.

It opens a small panel above everything, showing the transcript as it arrives,
a level meter per channel, and a **MUTE** switch on each. Muting in a call
application mutes that application, not the microphone — jay opens the device
itself — so the switch is here because there is nothing to detect. Add
`--terminal` if you would rather it just printed.

**And a button, if you want one.** The panel has one, and it was built for
practising technical interviews with a partner playing interviewer: press it
and you get working code for an algorithmic question, a diagram for a design
question, or a short nudge if you would rather work it out yourself. Afterwards
it runs the debrief — quotes your attempt back, says what you missed, then
gives the answer you should have given.

The transcriber came out of building that and turned out to be the more
generally useful half.

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

You want six OKs:

```
  mic       OK   46 frames, peak 0.0028 RMS
  system    OK   tap at 48000 Hz, 0 frames, peak 0.0000 RMS
  screen    OK   captured 676 KB
  whisper   OK   …/ggml-medium.en.bin
  claude    OK   4.4s, $0.0036 — cache is now warm
  notes     OK   claude-sonnet-5 in 7.0s, 14670 prompt tokens, $0.0124
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

Then run it. To transcribe a meeting and nothing else, in a terminal:

```sh
jay
```

Both sides, no time limit. Ctrl-C when the meeting ends, or close the panel. The
transcript is written as it goes, so it survives whatever happens to the
process afterwards, and when it ends jay writes the meeting notes beside it —
what was decided, who owes what, what nobody answered — each line citing the
timestamp it came from. That last part is the only thing a bare `jay` spends
anything on, and `--no-notes` turns it off. The session says which it is doing
before it starts listening.

`--mode coding` starts the button on the round you want, for the interview
half. Launching from the `.app` bundle rather than the binary is what gets you
Screen Recording, which the "ask jay" button needs and the transcriber does
not:

```sh
open -n -a ~/Projects/jay/target/release/jay.app --args --mode coding
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
`CODE`, `DESIGN`, `Q&A`, `DEBRIEF`, `PAIR`, `DEV` — and **DETAIL** picks `ANSWER`
or `NUDGE`. Throwing ROUND announces what that round gives, because leaving it
in the wrong position has already cost one interview its diagram. Throwing
either starts a fresh Claude process, so the next press pays the 4.7 second
startup again; that is the price of not quitting jay in the middle of an
interview, which is what changing rounds used to cost. `--mode` still sets where
it starts.

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
| `jay demo` | Draw the panel with sample content and no audio. `--state empty\|writing\|answered\|design`. |
| `jay` | The default. Listen to both sides, show the panel, write the transcript and the notes. |
| `jay --terminal` | The same, printing to the terminal instead of opening the panel. |
| `jay ask "<question>"` | One-shot, no audio. The fastest way to see what a mode gives you. |
| `jay notes <session.md>` | Write the meeting notes for a session already recorded. |
| `jay brief --out brief.md` | Build standing context from your memory index. |
| `jay file talk.wav` | Transcribe a 16 kHz mono WAV — a recorded session, a talk. |
| `jay listen` | Capture smoke test: frame counts, levels, dropped samples. |
| `jay devices` | What inputs jay can see. |

### `transcribe`

The default command, so `jay` and `jay transcribe` are the same thing. Both
sides, no time limit, terminal output.

```sh
jay --muted --mode coding --brief brief.md --vocab "union-find, Patroni"
```

| Flag | |
| --- | --- |
| `--source` | `mic`, `system`, or `both`. Default `both`; it is what lets jay tell you apart. |
| `--seconds` | Default `0`: runs until Ctrl-C, or until you close the panel. |
| `--terminal` | Print instead of opening the panel. There is then no mute switch and no button, so nothing can be spent. |
| `--muted` | Start with the microphone muted. Throw MUTE in the panel to change it mid-meeting. |
| `--mode` | Which round to start in. Switchable in the panel afterwards. Does nothing with `--terminal`. |
| `--brief` | Standing context for the session. |
| `--budget` | Stop suggesting after this many dollars. No limit by default. |
| `--save` | Override where the session is archived. |
| `--model` | `tiny`, `base`, `small`, `medium`, `turbo`. Default `medium`. |
| `--vocab` | Extra words to expect, comma separated. Falls back to `~/.config/jay/vocab`. `--vocab interview` loads the built-in algorithms list. |
| `--no-echo-gate` | Transcribe your microphone even while the other side is speaking. Off the gate goes; wear headphones. |
| `--mic-path` | `plain`, `aec`, `bypass`. A measurement tool, not a setting — `aec` loses the `them` channel. |
| `--no-notes` | Do not write the meeting notes when the session ends. |
| `--notes-model` | Which model writes them. Default `claude-sonnet-5`. |

### `notes`

```sh
jay notes ~/Library/Application\ Support/jay/sessions/<session>.md
```

Turns a transcript into the page you actually keep: what was decided, who owes
what, what nobody answered, and the thread of the conversation. Written beside
the session as `<session>.notes.md`. A live session does this for itself when
it ends, so this command is for meetings recorded before it did, and for
changing your mind after `--no-notes`.

Two things about it are not merely a summariser pointed at a log.

**Every line cites the moment it came from.** A decision reads
`Sharding by tenant, not by hash — the hot-tenant case is rare and the
rebalance cost is not [12:04]`, and you can go back and listen to 12:04. That
only works because the timestamps are honest, which they were not until
recently.

**Who owes what is not a guess.** `you` and `them` are two separate
microphones, so an action assigned to you was said by you. Everything a
diarizer would have to infer from voices, jay knows from wiring. What it
cannot do is tell six people on the far side apart; they are all `them`, and
the notes say `them` rather than picking a name.

It is also told, at some length, that an unresolved discussion is an open
question rather than a decision, that an empty section is a true section, and
that a point resting on a `?` line has to say so. The failure mode of a meeting
summary is not that it reads badly; it is that it asserts a decision nobody
made.

| Flag | |
| --- | --- |
| `--model` | Default `claude-sonnet-5`. |
| `--out` | Where to write them. Defaults to `<session>.notes.md`. |

Same subscription as everything else, through the `claude` CLI. Unlike
everything else, it asks the CLI to stop being Claude Code first — see
[shedding the agent](#shedding-the-agent), which is worth reading whether or
not you care about notes.

Nothing here is fatal. A session that cannot write its notes prints one line
and leaves the transcript alone; the recording is the thing that mattered and a
summariser must not be able to take it down.

### `ask`

```sh
jay ask --mode system-design --brief brief.md \
  --context session.md --hint "How would you scale the read path?"
```

Same modes, plus `--context <file>` to supply a conversation and `--screen` to
send what is on screen. No gate and no button — you typed it, so you meant it.

---

## Modes

An interview has phases, and no single mode serves them. Solving is not
defending: `coding` answers a follow-up question by writing another
implementation and `system-design` answers one with another diagram, which is
why `q&a` exists.

| Mode | For | What you get |
| --- | --- | --- |
| `coding` | An algorithmic round | The approach in a sentence, complete compiling Rust with the invariant named in a comment, time and space complexity, and the edge cases a first attempt misses. |
| `system-design` | A design round | Capacity numbers first, a component diagram jay draws in the panel, each component in a line, then the decisions that matter and what each traded away. The diagram is Mermaid underneath, so `copy mermaid` and Excalidraw's **Mermaid to Excalidraw** import gives you an editable drawing. |
| `q&a` | Defending an answer already given | Plain prose under 120 words, no code, no diagram, no capacity table. For the twenty minutes of "why is that there" that follow a solution, where `coding` would write another implementation and `system-design` another diagram. |
| `rehearsal` | The debrief afterwards | What your attempt missed, quoted back, then the full worked answer. |
| `pairing` | Working with a colleague | Concrete, opinionated, short. Will happily give you SQL. |
| `dev` | A test went red | What is likely responsible and the first thing worth checking. |

Set **DETAIL** to `NUDGE` in the panel, or add **`--hint`** to `jay ask`, to be
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

### Shedding the agent

`claude --print` is Claude Code with the interactive part removed, and it
brings Claude Code's system prompt and its whole tool catalogue with it. For
the notes, which involve no tools and no repository, that is a large amount of
instruction about being a coding agent sitting in front of a request to
summarise a meeting.

Measured on this machine, one `claude --print` call carrying a one-word
question:

| | prompt tokens |
| --- | --- |
| Default system prompt, `--allowed-tools ""` | 42,535 |
| Own system prompt (`--system-prompt`), `--allowed-tools ""` | 33,911 |
| Own system prompt, tools named in `--disallowed-tools` | 13,609 |

Two thirds of it, gone. The surprise is the second row: **`--allowed-tools ""`
does not remove the tool definitions**, it only stops them being called. They
stay in the prompt, and they are about twenty thousand tokens. Naming the tools
in `--disallowed-tools` is what removes them.

`notes` does both. **The panel's suggestions still do neither**, and that is
the obvious next thing to try — the tokens are imputed on a subscription, but
19,000 of them are instructions about a filesystem the model is not allowed to
touch, and shorter prompts are the one lever that has reliably moved latency in
this project.

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

### The room, coming back through your microphone

Without headphones the other person's voice leaves your speakers, crosses the
desk and arrives at your microphone, so it is captured on both channels and one
copy is blamed on you. Mute stops it while you are quiet; it cannot help the
moment you unmute to actually say something.

This was going to be judged on loudness — a microphone utterance overlapping a
system one and much quieter than it is the room. A real 75-minute meeting said
otherwise. Its opening is one 25-second system utterance against three short
microphone fragments, each sitting *inside* its span and made of its words,
because the two channels segment independently and disagree about where the
sentences are.

So the discriminator is **containment**, and it is better evidence than
loudness for a reason that has nothing to do with tuning. Restating a question
back to the interviewer is good practice, word-for-word similar to what was
asked, and the only thing separating it from an echo is that nobody can begin
repeating a sentence before it has been said. A restatement therefore starts
*after* its source ends and is never contained. The rule protects that case by
construction rather than by a threshold somebody has to guess at.

Two conditions keep it honest. A fragment must be at least four words, and its
parent at least twice its length — otherwise two people saying "Okay, yeah,
that sounds right" over each other looks exactly like a whole and a part. That
one was caught by a test written for the previous version of this, which is the
argument for keeping tests that assert what must *not* happen.

Replayed against that meeting's opening, four of the five wrongly-attributed
lines now go. The fifth straddles the boundary between two system utterances
and is contained by neither.

### Priming

Whisper decodes conditioned on a prompt, so telling it which words to expect is
the cheapest accuracy available. Untuned, `small.en` heard "reverse the linked
list" as "reverse the link please".

**Nothing is primed by default, and it used to be.** Every session was told it
was an algorithms interview, which is wrong for almost every session and not a
hint the decoder is free to ignore. A 75-minute meeting about knowledge graphs
and MCP servers has the receipt: at 25:29 the decoder gave up and recited its
own prompt back, and the transcript contains the sentence "This is a technical
interview about algorithms and system design", said by nobody.

Pass `--vocab "Fathom, Patroni, MinIO"` with the names and jargon of the
meeting you are about to have. Pass `--vocab interview` when the round really
is one, for the built-in list.

### Mute

**Muting yourself in Zoom, Meet or Teams does not mute your microphone.** It
mutes that application's outgoing stream. jay opens the input device directly
through CoreAudio, so as far as it is concerned nothing happened: it goes on
transcribing you, and files it under `you`.

Which is bad in the obvious direction and worse in the other one. Muted in a
call, you are not wearing headphones, so the other person's voice comes out of
your speakers, into your live microphone, and is archived as something you
said. The echo suppressor catches the clean cases — the same sentence on both
channels, one copy dropped — but a copy that crossed a desk often does not
transcribe cleanly enough to match, and then it is just fluent invented English
attributed to you.

macOS has no global microphone mute for jay to read, and a call application's
mute is private to that application. There is nothing to detect. So there is a
switch, on the meter of the channel it silences:

```
YOU   ▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░░░░   MUTED  MUTE
THEM  ▓▓▓▓▓░░░░░░░░░░░░░░░░░░░░░░░░░   SPEECH MUTE
```

The words are anchored to the right edge and the bar takes what is left. It
shipped the other way round for about an hour — a fixed width reserved for the
status word, sized before there was a switch after it — and the switch rendered
off the edge of the panel as "MU".

The bar keeps moving while it is muted, deliberately. A muted meter that went
flat would be indistinguishable from a microphone that had died, and jay has
lost a session to exactly that class of confusion before. Throwing the switch
mid-sentence discards whatever the VAD was holding rather than emitting it on
unmute.

`--muted` starts that way, for sitting in on something you are not speaking in.

### The echo gate

**While the other side is speaking, your microphone is not transcribed.** On by
default, `--no-echo-gate` to turn it off.

This is a blunt instrument and it is there because the subtle one does not work.
Without headphones the speakers arrive at the microphone *louder than speech
does* — 0.0408 RMS against a speech level of about 0.02, measured on a real
call. The segmenter therefore never falls silent, never reaches its exit
condition, and runs to the 25-second cap in `vad.rs`, cutting mid-sentence with
both people's words inside the same utterance. Nineteen of fifty-five microphone
utterances went that way in one 19-minute meeting, and twenty-seven of the
fifty-five began on a lowercase word, which is what that looks like from the
outside.

The text-side echo guard cannot repair it. It compares a microphone line against
a system line expecting two copies of one sentence, and once the segmenters
disagree about where sentences are there is nothing left to match — the two
channels' start times differed by nine to sixteen seconds against a two-second
window.

So the gate works upstream instead: while the `them` channel is mid-utterance,
frames on the `you` channel are fed to the detector but cannot open an utterance
or keep one open. The cost is real and one-sided. **Speak over the other person
and you are not transcribed at all.** That is worse than a proper echo canceller
and better than what it replaces, where the same words survived inside a
25-second block attributed to the wrong person. It says so in the panel the
first time it fires, and reports its total when the session ends, because a
mechanism that removes speech should not do it quietly.

Turn it off with headphones on. There is no echo to gate, and talking over each
other is then just a conversation.

> The platform's own echo canceller was tried first and rejected. It works, and
> it takes the `them` channel with it: `VoiceProcessingIO` puts the output
> device into a mode a CoreAudio process tap cannot see through, so the far side
> vanishes entirely. The code is kept behind `--mic-path aec` so the measurement
> can be repeated. See [IMPROVEMENTS.md](IMPROVEMENTS.md).

---

## Sessions

Every run archives itself to a timestamped file under
`~/Library/Application Support/jay/sessions/`. No flag required, because a
feedback loop that depends on remembering a flag is a loop that does not run.

The file holds the conversation, everything jay said, a clock stamp on each
line, and what each suggestion cost.

```
[00:00] (listening on you: MacBook Pro Microphone, them: whatever plays through AirPods Pro.)
[00:21–00:33] them: The problem is a rate limiter, distributed across many nodes.
[00:34–00:36] you: Right.
[00:35–00:52] them: And I want the counters consistent without a round trip per request.
[01:04–01:06] ?you: token bucket, per tenant
[01:04] (kept the line above out of context — 2.1s at peak 0.009, too brief and too faint)
```

Three things to know about reading it back.

**A stamp is when the speech began, not when the transcript of it appeared.**
Those differ by the VAD's silence hangover plus however long the decoder took,
which is seconds and varies with the length of what was said. jay stamped
arrival times until August 2026, which made every session read two to four
seconds late.

**Ranges overlap when people talk over each other**, as `00:34–00:36` and
`00:35–00:52` do above. Utterances are *decoded* in the order they finished, so
a long question is decoded after a short interjection that began later; the
writer holds each line back three seconds and commits them in the order they
were said. Three seconds is not a guarantee — guaranteeing it means holding
every line for half a minute, which is too long to watch a meeting go by — but
it sorts everything that arrives close together, which is where the inversions
come from.

**A `?` means jay heard it and did not trust it.** Those lines stay in the
record and stay out of the model's context, and the reason is written on the
line beneath so the thresholds can be recalibrated from evidence. A record of a
meeting that silently omits the quiet half of it is worse than one that admits
what it is unsure about — a real interview lost ten of the candidate's
utterances that way, leaving nothing behind but a duration and a peak.

Words nobody said are still dropped outright: known whisper artefacts, and the
decoder reciting its own `--vocab` back. Those get a notice rather than a line.

The artefact list has two halves, and it did not always. Subtitle credits
("thanks for watching", "amara.org") are caught anywhere in a line, because
nobody says them in a meeting. Sign-offs whose words are also ordinary speech
("the end", "www.", ".com") are only caught when they are *all* that was said.
Matched anywhere, as they were until a real meeting was recorded, they delete
"at the end of the day", "hit the endpoint", "the end user", and any sentence
citing a `.com`. Two real sentences went that way in 75 minutes, one of them
the only technical proposal in the first two minutes, and nothing announced it.

It is written to be *replayed*, not merely read:

```sh
jay ask --mode rehearsal --brief brief.md \
  --context ~/Library/Application\ Support/jay/sessions/<session>.md \
  "Count the number of islands in a grid."
```

That is the only honest way to tell whether a change to the prompts helped,
rather than whether it reads better.

And when the session ends, jay writes `<session>.notes.md` beside it. See
[`notes`](#notes).

---

## Costs

Measured on an M3 Pro, driving a Max subscription. Dollar figures are the CLI's
own `total_cost_usd`, which is an imputed list price rather than money leaving
an account.

| | |
| --- | --- |
| Spawn, preamble and one round trip | 4.7s — the floor, before any answer |
| A hint | ~5s, ~$0.18 |
| A coding answer with working Rust | ~9s, ~$0.19, first words at ~5s |
| A design answer | ~16s, ~$0.20 |
| A rehearsal debrief | ~53s, ~$0.28 — uncapped by design, run after the round |
| Meeting notes, one-minute meeting | 11s, ~$0.027 imputed, 16,440 prompt tokens — of which the meeting is about 1,400 |
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

**A `--seconds` session finished but the panel is still there and there are no
notes.** Known, and not fixed. The recording stopped on time and the transcript
on disk is complete; the notes are written when the window closes, and macOS
will not redraw an unattended window to let it close itself. Close it and they
land. Or use `--terminal`, which has no window to leave open. Ending a session
the normal way — closing the panel yourself — has never had this problem.

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

**The transcriber repeating itself.** A line that says the same clause four or
more times running is dropped as a decoder loop rather than kept as speech. The
line that earned the filter was `I'm not sure what to say.` five times over, in
a silent room with nothing playing, and it had cleared the artefact list, the
level floor and the confidence floor — all three ask what the words *are*, and
none of them asks whether the line eats itself. Three repeats survive, because
"yeah, yeah, yeah" and "good day, good day" are things people actually say.

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

**The first seconds of a meeting.** Captures are opened *before* `medium.en`
loads, so audio spoken during the load is buffered and decoded once the model is
ready rather than lost. It used to load first, which meant the devices were not
merely unread during those seconds but not open at all. About the first second
still goes while the tap itself starts.

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

**Used in anger twice: one 40-minute interview, one 75-minute meeting.** What
follows is what those two sessions showed, which is worth more than everything
that preceded them put together.

Transcribing a conversation is now the thing jay does by default rather than
the thing it does on the way to answering. `jay` on its own opens the panel and
records both sides until you stop it; the assistant is a button you have to
press. Stamps are taken from when speech began rather than when the decoder
finished with it, overlapping utterances are written in the order they were
said, a line jay does not trust stays in the record marked, and the notes are
written when the session ends.

### What works

**The listening is done.** A forty-minute system-design interview with a
principal engineer at a data company: 61 lines from the interviewer, 57 from the
candidate, attribution correct throughout, long passages near-verbatim. Every
part of the capture story that took a week to get right — the process tap, the
two channels, the echo handling in both directions, Bluetooth at whatever sample
rate the headphones feel like — held up unattended for the whole session.

Also verified along the way: preflight on all five lines, `medium.en` decoding at
11.5× real time, zero dropped samples with 340µs worst queue lag, session
archiving, screen capture, six modes, diagrams drawn in the panel and importable
straight into Excalidraw, and a clean exit. 129 tests.

**The notes work, on two transcripts.** One dictated interview opening and one
hand-written two-sided conversation built to trip them up. Both held: an
explicitly deprioritised item ("let's not do that this week") was recorded in
the thread and *not* turned into an action, an empty section came out `- none`
rather than invented, the two speakers' actions were not swapped, and a point
resting on a `?` line was carried across with the caveat rather than asserted —
including quoting the transcriber's mangled "Petrino" for Patroni rather than
silently correcting it into a claim.

**Then a real one: 75 minutes, 550 stamped lines, unattended.** The notes off
that transcript are the best evidence this half of jay works. They separated
about thirty minutes of engineering from forty-five minutes of tangent and said
so in the opening sentence rather than pretending the whole thing was a
meeting; `Decisions: - none`, which was the honest answer and the one most
likely to be faked; three actions, each cited; a topic that came up twice given
two ranges in the thread.

The mechanisms held up at that length too. **One ordering inversion in 550
lines**, and it is precisely the case the three-second reorder window is
documented as unable to fix — a 25-second question decoded after three shorter
utterances that began later. The `hh:mm:ss` clock ran to 1:15:14. And the
brief-and-faint filter, at its deliberately weak 0.012, held back **two lines in
75 minutes, both of them a single word**: "Ah." and "But." That question is
closed.

### What does not

**It is too slow for a fast interviewer.** Eight to thirteen seconds an answer,
against an interviewer who opened by observing that nobody can work a model as
fast as they can think and talk, and then set the pace to prove it. Three
presses in twenty minutes of live round. One of those three answered the
question before last, because the conversation had moved on in the twelve
seconds it took to arrive.

**A design round has never produced a design.** The switch was left on `q&a`
from before the problem was stated, and `q&a` is forbidden from drawing a
diagram. It now announces what each round gives when you throw it, which is a
sticking plaster over a switch that is still easy to leave in the wrong place.

**Two filters were deleting real speech, and one recording found both.** The
artefact list matched `"the end"`, `".com"` and `"www."` anywhere in a line, so
"at the end of the day", "hit the endpoint" and any sentence citing a document
were binned as whisper inventions; the meeting lost two sentences that way, one
of them the only technical proposal in its first two minutes. And every session
was primed as an algorithms interview, including that one — at 25:29 the
decoder gave up and recited the prompt back, so the transcript contains a
sentence about binary trees that nobody said. Both are fixed; both had been in
the tree for months, doing this quietly, on every session.

**Everyone on the far side is `them`.** One person or six, jay cannot tell,
because the separation it has is physical — two microphones — rather than
acoustic. Splitting the far side needs real diarization and is a different
project.

**The mute switch needs a mouse.** Clicking it means focusing the panel
mid-meeting, which is exactly when you do not want to be hunting for a window.
A global hotkey needs a `CGEventTap` and an Accessibility permission.

### Next, when it is picked up again

1. **A mute hotkey.** The switch works and reaching it does not. A `CGEventTap`
   and an Accessibility permission, so an afternoon rather than a line.
2. **Shrink the panel's prompt.** The ask-jay path still sends 42,535 tokens,
   about 19,000 of them definitions for tools it is not allowed to call. The
   notes path does the same job in 13,609 — see
   [shedding the agent](#shedding-the-agent). Latency here has only ever moved
   when the prompt or the output got shorter, and the risk is that the answers
   lean on Claude Code's system prompt for formatting in ways that only testing
   will show.
3. **Make the round harder to get wrong.** Announcing the mode is not enough.
   Either infer it, or stop `q&a` and `design` being mutually exclusive.
4. **A live design round.** No diagram has ever been produced in an actual
   interview, so the whole design half remains unproven where it counts.

The cross-channel bleed suppressor came off this list. It was designed as a
loudness rule for a year and built as a containment rule in an afternoon, once
there was a recording to look at.

### The lesson worth keeping

Three of the filters in this repository were built by reasoning about how a
failure ought to work, and none of them touched it. The one that worked came
from twenty minutes of logging a repeatable session. `scripts/dictate.sh` exists
for that reason: it reads a script aloud through the output device so the tap
hears it as the other person, which makes a session repeatable, which makes it
measurable. Build the harness first.

One 75-minute recording then found two filters that had been deleting real
speech for months, closed a threshold question that three rounds of guessing had
not, and replaced the bleed suppressor's central assumption — it was going to
compare loudness, and the thing that actually separates the room from a person
is whether one utterance sits inside another. None of that was visible from
here. It was visible the moment somebody put the thing in a real meeting and
read the file afterwards.

---

## Layout

```
crates/
  jay-audio   capture, resampling, VAD, the macOS shim
  jay-stt     the SpeechModel trait and whisper.cpp behind it
  jay-agent   the gate, context selection, prompts, screen capture, archiving,
              echo suppression, and the meeting notes
  jay-ui      the panel
  jay         the binary that wires it together
docs/
  design.md         decisions and why, including what went wrong
  mock-session.md   a runbook for an actual practice session
scripts/
  bundle.sh         copy the binary into the .app, for permissions
  dictate.sh        read a script aloud through the speakers, so a session repeats
```

---

## Licence

MIT or Apache-2.0, at your option. See [LICENSE-MIT](LICENSE-MIT) and
[LICENSE-APACHE](LICENSE-APACHE).

---

## A word about recording people

jay records both halves of a conversation. Whether you may do that where you
are is a question with a legal answer that varies by jurisdiction and a social
answer that does not: tell people. The software does nothing to help you record
somebody who has not agreed to it, and the section above on why it refuses to
hide itself is there for the same reason.
