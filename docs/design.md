# Design notes

Decisions and the reasoning behind them, so that the reasoning is still
available when the decision looks arbitrary six months from now.

## The shape of the thing

Four stages, which is the same shape every tool in this category has:

```
capture -> VAD -> STT -> context -> trigger gate -> agent -> overlay
```

None of it is deep technology. The difficulty is not the pipeline, it is
knowing when to speak, and not spending a fortune deciding.

## Target rate is 16 kHz mono f32

Both whisper.cpp and Silero VAD want exactly that, so the conversion happens
once, in `jay-audio`, and nothing downstream has to think about device rates.

Resampling uses rubato's polynomial resampler at septic degree. Naive
decimation would be cheaper and would alias every consonant into mush, which
shows up much later as a worse word error rate and is close to undiagnosable
from the transcript alone.

## Mic and system audio stay on separate channels

Speaker attribution by channel is free and correct. Attribution by
diarisation from a single mixed stream is neither. Keeping them apart the
whole way through means the transcript always knows who said what.

## The audio callback does almost nothing

Convert to `f32`, push into a lock-free ring, return. Downmix, resample and
framing happen on a worker thread. Work in the callback risks an xrun, and an
xrun is indistinguishable from a word the transcript simply never received.

The worker carries the partial interleaved frame between ring reads rather
than discarding it. Discarding it would rotate the channel order and the
downmix would quietly start averaging the wrong pairs.

## Frames are 32 ms

512 samples at 16 kHz. Not a free choice: Silero v5 accepts exactly 512 samples
at 16 kHz and rejects anything else, so the whole pipeline is framed to suit the
VAD rather than the other way round. (This began life at 320 samples, which is
the number you would pick on latency grounds alone, and the VAD refused it.)

## `captured_at` is stamped in capture, not on arrival

So that later latency measurements describe the pipeline rather than the depth
of the queue.

## macOS system audio: CoreAudio process taps, not ScreenCaptureKit

`AudioHardwareCreateProcessTap` arrived in macOS 14.4. It is audio-only, can
tap a named process, and does not drag in the screen-recording permission the
way `SCStream` does. This machine runs 26.5.1, so it is available. Both routes
need an FFI shim; the tap route asks the user for less.

## Permissions: jay must be launched through LaunchServices

The system-audio tap spent a while running perfectly and capturing nothing:
the IOProc fired steadily, 276 frames in ten seconds with a 4 ms queue lag and
zero drops, and every sample was zero during audible playback. Screen capture
failed the same way later, with `screencapture` reporting only "could not
create image from rect".

Neither was a CoreAudio or CoreGraphics problem. **A binary launched from a
shell inherits the responsible process of whatever owns that shell**, so macOS
never attributed the request to jay: `tccutil reset AudioCapture
dev.cargopete.jay` succeeded, proving the system tracked a grant for the bundle
id, while the TCC log showed jay had never once asked for it. An unauthorised
tap is fed silence rather than an error, which is the worst of both worlds.

Launched through LaunchServices, the same code works:

```sh
scripts/bundle.sh debug
open -a "$PWD/target/debug/jay.app" --args listen --source system --seconds 12 --out /tmp/jay.txt
```

374 frames of an expected 375, peak RMS 0.265, zero drops. `open -a` needs an
absolute path to the bundle, and detaches jay from the terminal, which is why
`listen` grew an `--out` flag.

The lesson generalises to every permission jay will ever want: it is not enough
to be a signed bundle with the right usage descriptions, it has to be *started*
as one.

## What a real transcript changed

A recording of two real interviews, in which a commercial tool of this kind
helped badly, drove three changes. It is worth writing down what it got wrong,
because the failures are not obvious from the outside.

**It answered the scheduling.** Three minutes of "do you see the updated
invitation?", "would you like to start earlier?", "I assume we have another
interview setup still for today, right?" — every one a grammatical question,
not one of them wanting help. Detecting a question is easy; detecting a
question worth twelve seconds and twenty cents is the actual problem. Hence
the small-talk filter in `jay-agent::gate`, whose test corpus is those exact
lines, verbatim.

**Its help arrived after the moment had passed.** In the second interview the
candidate reasoned his own way to JWT-plus-ownership between 02:52 and 02:53;
the polished answer landed at 02:53, after he had got there. That matches the
measured 12–20s and it is not fixable by being cleverer. So every mode's prompt
now carries [`LATE_ARRIVAL`]: read what has already been said, do not repeat it,
give only what is missing. Arriving late is only a problem if you were trying
to be first.

**Replaying that moment through the new prompts** produced what the original
missed: authentication and authorisation separated, ownership pushed into the
`WHERE` clause so there is no read-then-write window, 404 rather than 403 so
the response does not leak which codes are taken (the original tool
specifically recommended 403), and a flag that the interviewer will ask about
anonymous creation next. `jay ask --context <file>` exists so any recorded
transcript can be replayed through the real prompt path, which is the only
honest way to tell whether a prompt change helped.

## Audio never waits for the expensive path

One real run reported **34,048 dropped samples** — about 0.7 seconds of speech
gone. The cause was four layers away from the symptom.

`crossbeam`'s bounded `send` blocks when the channel is full. The question
channel to the assistant held four, and each suggestion occupies it for twenty
to thirty seconds. So: assistant busy → the transcriber blocks trying to queue
a question → the utterance channel (16) fills → the capture loop blocks → the
frame channel (512) fills → the microphone worker blocks → the ring buffer
overflows and the device's audio is discarded.

Every hand-off from the audio path to something slower is now `try_send`. A
suggestion that cannot be queued is skipped with a notice in the panel; an
utterance that cannot be transcribed in time is counted and reported. Losing a
sentence is recoverable and visible. Losing the audio is neither.

The general rule, worth keeping: **anything downstream of the microphone may
drop work, but nothing downstream of the microphone may make the microphone
wait.**

## Capture in process, or TCC will refuse you politely

Screen capture first shelled out to `/usr/sbin/screencapture`. It failed from
the app bundle with "could not create image from display" — with Screen
Recording granted, the toggle visibly on in System Settings, a stable ad-hoc
signature, and the identical flags succeeding from a shell one second earlier.

TCC evaluates a request against the process that makes it, and a spawned Apple
binary does not cleanly inherit its parent's grant. Capturing in the shim, via
`CGDisplayCreateImage`, makes the request unambiguously jay's and it works
immediately.

This is the third time the same lesson has cost time on this project. macOS
grants capabilities to a *process identity*, and every layer of indirection
between the grant and the request is a chance to lose it: a shell-launched
binary inherits its terminal's identity (the silent audio tap), and a spawned
subprocess does not inherit its parent's (this). **Ask directly, from the
process that holds the grant.**

The failure mode is what makes it expensive: `CGDisplayCreateImage` returns
`NULL` and `screencapture` reports a generic error. Neither says "permission".
Hence `jay check`, which tries every capability for real and prints what
happened.

## Speaker attribution needs two channels, and says so when it has one

The gate treats microphone audio as you and system audio as them, which is free
and correct when the other person is on a call. In a room, both voices arrive on
the same microphone, every question looks like you thinking aloud, and the gate
would never fire — silently.

So attribution is only applied when both channels are actually running. With
one, jay says in the panel that it cannot tell who is speaking and treats every
question as worth answering. A wrong guess made quietly is worse than a
capability declined out loud.

## No capture exclusion, deliberately

`SetWindowDisplayAffinity(WDA_EXCLUDEFROMCAPTURE)` on Windows and
`NSWindow.sharingType = .none` on macOS are the flags this category of tool
uses to stay out of a screen share. jay does not set them.

Partly because the macOS one stopped working for ScreenCaptureKit in 15.4 and
Apple has said there is no public replacement, so anything built on it is a
lie waiting to be discovered. Mostly because the tool is for rehearsal,
pairing and debugging, all of which are things you can say out loud that you
are doing.

## Suggestions are outlines, not scripts

In rehearsal and pairing modes jay proposes talking points, an approach, and
the gaps in what you actually said. It does not generate a paragraph to read
aloud. This is not squeamishness: a script you read is worse help. It makes
you sound like someone reading, it collapses the moment you are asked a
follow-up, and it teaches you nothing for next time. Points you have to
assemble into your own sentences survive contact with a real conversation.

## Cost control is a design constraint, not an optimisation

A continuously running agent is the expensive failure mode of this whole idea.
Anthropic's own figures put Claude Code at roughly $13 per developer per active
day; Microsoft reportedly pulled 5,000 engineers off it after per-engineer
costs ran $500 to $2,000 a month. A tool that thinks constantly would be worse.

So: a cheap gate runs continuously, and the expensive model runs only when the
gate escalates. Deterministic triggers (a red test, a stack trace, a failed CI
run) cost nothing to detect and are the highest-signal events available. Those
come first, before any clever conversational judgment.

## What a suggestion actually costs and how long it takes (measured)

All figures from this machine, driving `claude -p` on the Max subscription.

A one-word reply to a trivial prompt:

| Cache state | Cost |
| --- | --- |
| Cold | $0.0254 |
| Warming | $0.0176 |
| Fully warm | $0.0033 |

The prompt was "reply with exactly one word". The cost is the CLI's own
preamble: roughly 29,000 tokens of Claude Code system prompt and tool
definitions, carried on every single invocation, cached for an hour and cold
again after that.

A real rehearsal suggestion, same question to each model:

| Model | Latency | Cost |
| --- | --- | --- |
| `claude-opus-5` | 17.9s | $0.2177 |
| `claude-sonnet-5` | 20.1s | $0.1553 |
| `claude-haiku-4-5` | 11.9s | $0.0311 |

Two conclusions follow, and both shaped the design.

**The gate cannot be a model.** At $0.0033 warm and $0.0254 cold per call, a
model-based gate firing every thirty seconds costs roughly $0.40 an hour warm
and far more cold, to answer yes or no. So [`jay-agent::gate`] is rules: a
question mark, an interrogative opening, a wake phrase, or a deterministic
event. It costs nothing and the subscription pays only for escalations.

**Latency is a fixed overhead, not a model property.** Haiku is seven times
cheaper than Opus and still takes twelve seconds, because the time goes on
process spawn and preamble rather than on thinking. That is fine for rehearsal,
where the gap between questions is long, and for anything reviewed afterwards.
It is too slow to help mid-sentence in a live pairing session. Making live
suggestions fast would mean the HTTP API with an API key, which skips the
preamble entirely but is a separate bill from the subscription. That is a
decision to take deliberately rather than drift into.

## Backend is the Claude Max subscription via the local CLI

Not an API key. The `claude` CLI already holds the OAuth session, so headless
invocation uses the subscription. Worth noting the metering rules here have
been in flux and should be checked against current policy rather than assumed.

## A reading beats a guess, twice over

Two faults found the same way, in the same session, and they are the same fault.

**The preflight lied about the microphone.** `jay check` printed `mic OK` on
the strength of the device *existing* — it called `input_devices()` and never
pulled a sample. macOS answers a refused microphone with perfect digital
silence rather than an error, so the check would have printed OK on a machine
that could not hear a thing. It now records for three seconds and judges the
samples: no frames is one fault, frames of exact zeros is a refused permission,
and a peak below 0.02 is a room where nobody spoke. Three different fixes,
three different messages.

**The panel could not tell silence from deafness.** Between a sound arriving
and a sentence appearing there are about ten seconds of VAD and whisper. For
all ten, an empty panel means either "nobody has spoken" or "nothing is being
heard", and those look identical. There are now two meters fed by the RMS of
the actual samples, with the VAD's own speech decision beside them. A channel
that has never delivered a frame reads `OFF`, one whose frames have stopped
reads `NO INPUT` in ember, and neither is drawn the same as `QUIET`.

The general form: **jay must never report a state it has not measured.** An
instrument bound to a real value is worth more than any amount of careful
reasoning about what ought to be happening.

## The silence floor was measured against the wrong number

A session where a clear "reverse a linked list" was spoken into a working
microphone produced an empty transcript. The audio was fine, whisper
transcribed it, and then jay threw it away.

`HALLUCINATION_FLOOR` guards against whisper inventing fluent sentences out of
room tone, and it was compared against the RMS of the whole utterance. But
every utterance carries ~250 ms of pre-roll and ~600 ms of trailing silence by
construction, because that is how the segmenter finds its boundaries. So the
number being tested scaled with how long the pause afterwards was rather than
with how loudly anyone spoke, and a short sentence with a normal pause after it
was diluted below a floor that a long one cleared easily. On this machine the
room reads 0.0028 and the floor was 0.01, so the margin was never wide.

The utterance now carries `speech_peak`, the loudest frame the VAD actually
called speech, and the floor is compared against that. Pre-roll and trailing
silence cannot move it.

The drop was also logged at `debug` and nowhere else, which is what made it
cost a whole session: a run where every utterance was binned looked exactly
like a run where nobody spoke. Both filters now say so in the panel.

## Two prompts, one of them lying

`coding` mode returned five paragraphs of prose and no code at all, which is
the one thing it exists to produce. The system prompt asked for "complete,
idiomatic, compiling code". The user prompt built alongside it, in the same
call, said "Not the code — the insight that makes the code obvious."

Both were true when written. The tails in `build_prompt` were written when
every mode was a nudge, and were never revisited when `Depth::Full` arrived and
the system prompts grew to describe whole worked answers. The tail won.

At full depth the tail now states the question and stops. **The shape of the
answer is the system prompt's job, and exactly one place may hold it.** A test
asserts full depth never forbids the code it is being asked for.

## Latency is output length, and nothing else much

Measured, all on this machine against the Max subscription:

| | |
| --- | --- |
| Spawn, preamble, one round trip, one word back | 4.7s |
| Screenshot inline, one sentence back | 4.7s |
| Coding answer, uncapped prompt | 77s |
| Coding answer, capped prompt | 9.4s |

Three things follow.

**The 4.7s floor is not negotiable** while jay drives the CLI. It is node
startup plus ~29,000 tokens of Claude Code preamble, and it is paid whether the
answer is one word or one page.

**The `Read` tool call for the screenshot was pure waste.** jay wrote the JPEG,
then handed the model a path and enabled a tool so it could open the file jay
had just written — an entire extra round trip, about as long as the floor
itself. The image now goes in as a base64 block via `--input-format
stream-json`, which costs the same as sending nothing, and no tools need be
enabled at all.

**Everything else is output length.** 77 seconds versus 9.4 for the same
question is not the model thinking harder, it is the model writing an
alternatives section and an aside about what the interviewer might prefer.
Neither is a better answer to read while you are talking. Both interview modes
now carry a hard word cap, and a test enforces that they do. Rehearsal is
exempt, because it runs after the interview where thoroughness costs nobody
the thread.

Streaming does not change any of these numbers, and is still the largest
improvement of the four: with `--output-format stream-json
--include-partial-messages` the panel paints the answer as it is written, so
the first words arrive around five seconds instead of the whole thing at
fourteen. Late help is only useless if you cannot start reading it.

## Two traps that make a working stack look broken

Both found while chasing a panel that showed `OFF` on both meters. Neither was
a bug in the pipeline, which was working the whole time.

**A process tap on an idle output produces no callbacks at all.** Not silent
frames — no frames. Measured: `listen --source system` on a quiet machine gives
0 frames of an expected 312; with `say` running through the speakers, the same
command gives 312 of 312 at peak 0.271 RMS and 205µs lag. So "the tap is
delivering nothing" is the *normal* state of a silent Mac and cannot be
distinguished from a broken tap without playing something. `jay check` says so
on the system line rather than reporting a failure.

**`open -a` silently discards `--args` if the app is already running.** It
activates the existing instance, exits 0, and starts nothing. Verified: two
launches two seconds apart produced one process, and the second command's
`--out` file was never written. Every instruction in this repository therefore
uses `open -n -a`, which forces a new instance. Worth knowing that two
instances then contend for the microphone and both lose frames — 222 of an
expected 281 while sharing — so `-n` is for making sure you get *the* session,
not for running several.

The connecting theme with the permission trilogy: **every one of these reports
success while doing nothing.** A tap that returns no callbacks, a launcher that
exits 0 having ignored its arguments, a TCC denial served as digital silence.
None of them raises an error, so none of them can be caught by handling errors.
They can only be caught by measuring the thing itself, which is what `jay check`
and the panel meters are for.

## A failure the user cannot see is not a failure the user can report

`run_pipeline` returning an error logged it with `tracing::error!` and did
nothing else. Launched through LaunchServices there is no terminal attached, so
the log went nowhere at all, and the panel — which had already printed its
"ready in Coding mode" notice before the failure point — sat there looking
perfectly healthy for as long as anyone cared to watch.

The pipeline thread now keeps a clone of the line sender back specifically so a
dying pipeline can still reach the panel. The meters likewise distinguish four
states rather than two: `OFF` for a channel never asked for, `STARTING` for the
first few seconds, `NO FRAMES` in ember for a channel that was asked for and
has delivered nothing, and `STALLED` for one whose frames have stopped. Only
the first is not a fault.

## The two channels need different words for the same silence

The meters shipped calling any channel that had delivered frames and then
stopped `STALLED`, in ember. On the first real session with headphones on, the
system channel read `STALLED` throughout — correctly, by that definition, and
uselessly, because an idle output tap produces no callbacks and the panel would
therefore have flapped between `SPEECH` and a fault light at every pause in the
conversation.

A microphone and a process tap are different instruments and absence means
opposite things on them:

- A live microphone delivers frames whether or not anyone is speaking. Measured
  at 248 frames in a silent room. So silence there is a real fault, and is
  drawn as one.
- A process tap delivers nothing at all when the output is idle. A quiet call
  and a dead tap are indistinguishable from inside the panel and always will
  be. `jay check`, run with something playing, is the only instrument that can
  tell them apart.

So the system channel says `IDLE` and `NO AUDIO YET` in faint ink, and only the
microphone gets ember. **A warning light that is on during normal operation is
not a warning light**, and the cost of getting this wrong is not a cosmetic
one: it trains you to ignore the exact indicator that was added to catch a
fault you had already lost an evening to.

## One process per session, not one per question

Every `claude -p` invocation pays about 4.7 seconds before it says anything:
node startup, then ~29,000 tokens of the CLI's own preamble. Paid per press,
that is most of the wait. It need not be paid per press.

`--input-format stream-json` reads a *stream* of messages, so the process can
be kept and asked again. Measured, including a 75 second idle gap between the
two to match what a real session looks like:

| | |
| --- | --- |
| First ask | 3.1s |
| Second ask, same process | **1.7s** |

The process survives idling, so this is not a trick that only works when the
questions are queued up front. An ignored test in `claude.rs` asserts the
second ask beats the first, because if the process is ever silently respawned
the type is buying nothing and the tests should say so rather than the
stopwatch.

The second benefit has nothing to do with speed: the process keeps the
conversation, so the third question of a round is asked of something that heard
the first two. The brief is therefore sent with the first question only — after
that it is in the history already, and repeating it every turn pays for it
again. If the process dies the history dies with it, so the brief is re-armed
and the question retried once.

## Priming the transcriber is the cheapest accuracy available

`small.en` heard "reverse a singly linked list" as "reverse the link please".
jay answered correctly anyway, because the screenshot and the surrounding
transcript carried what the words lost, but the same failure on the *problem
statement* would poison everything downstream — and that is the one sentence
spoken exactly once.

whisper decodes conditioned on a prompt, so telling it which words to expect
costs nothing and is set once per process. The default list covers two
registers, because both turn up in the same sentence: the language of
algorithmic interviews, and the language actually being worked in.
`--vocab` appends the terms specific to a round. After priming, the same
spoken sentence transcribes exactly, with no leakage of the prompt into the
output.

## jay is in its own screenshots

The panel sits on the display jay captures, so the image contains jay's
previous answer. Asked what was on screen, it once described its own last turn
quoted back at it, which reads as a curiosity until you notice that mid-round
the panel holds the *code jay suggested*. Mistaking that for the candidate's
own work makes every "what have they got so far" judgement wrong in the same
direction, and the whole `LATE_ARRIVAL` instruction depends on that judgement
being right.

Every question carrying a screenshot now says which part of the image is jay's
own output. Capturing only the focused window would be stronger and is the
better fix if this proves insufficient; a sentence is what it costs today.

## The switches, and why they cost 4.7 seconds

A mock loop is an algorithmic round followed by a design round, and those want
different prompts. Before the switch bank the only way between them was to quit
jay and relaunch it with a different `--mode` — which means quitting *during an
interview*, losing the transcript that had just been built up, and fumbling with
a terminal while somebody waits.

The system prompt is baked in when the CLI process spawns, so a session is
defined by the pair (mode, depth) and changing either means a new process. The
next press therefore pays the ~4.7 second startup again. That is the right
trade: it is paid once per round rather than once per question, and the thing
it buys is not having to leave the interview.

Two details that matter. The switch position is carried **on the question**
rather than read from shared state when the answer comes back, so moving a
switch while an answer is in flight cannot retroactively change what that
answer was asked for. And the switches light immediately on click rather than
when the pipeline acknowledges them, because a control that does not move when
you press it gets pressed again.

They are drawn as switch positions rather than buttons — one lit in brass,
the rest faint — because that is what they are. Only one position of each bank
can be thrown at a time, and a button is a thing you press to make something
happen rather than a state you leave the machine in.

Two tests hold this together: every mode must appear in `Mode::ALL`, or it
would simply be unreachable from the panel and nothing would say so; and the
switch positions must produce genuinely different system prompts, or throwing
one would cost 4.7 seconds and buy nothing.

## The press drains what is still in the segmenter

An utterance is not emitted until 600 ms of silence have passed, and whisper
takes another half second on top. So at the moment the lever is pulled, the
sentence least likely to be in the transcript is the one just spoken — which is
precisely the one being asked about. jay would answer the previous question,
confidently, having genuinely never heard the current one.

Pressing now raises a flag the capture loop honours on its next frame: both
segmenters are flushed, whatever was mid-sentence is queued, and the hand-ask
thread waits for the backlog to clear before reading the transcript. The wait
is capped at 1.5s and is only ever paid when there is something to wait for.

The fiddly part is knowing when the backlog *is* clear. The transcription loop
has five `continue`s — a failed inference, an artefact, audio too quiet, and so
on — and every one is an utterance that will never reach the transcript. A
press waiting on a count that those never decrement would sit out the full
timeout every time. Hence a drop guard rather than a decrement at the end of
the loop body, and a test that exercises the branches, so a `continue` added
later cannot quietly reintroduce it.

## Markdown that nothing renders

The model returns markdown. The panel renders none of it, so a coding answer
arrived with its fences and its asterisks intact, as one flat block of text —
read at a glance, mid-sentence, with somebody waiting.

Answers are now split on fenced code blocks and the code is drawn in a well cut
into the plate: `--inset` ground, hairline border, `--brass-hi`, a shade smaller
than the body because a line of Rust is longer than a line of prose and
wrapping code is worse than reading it small. Emphasis markers are stripped
rather than rendered.

The case that matters most is the unterminated fence, because **every partial
is one**: the panel draws the answer while it is still being written, so the
closing fence has not arrived yet. Treating that as "no code block" would show
the code only once the answer was complete, which is precisely what streaming
exists to avoid. A test covers it.

## Bigger is not the axis; being wrong plausibly is

Measured on one 22-second jargon-heavy question, spoken by `say` at 16 kHz:

| | inference | speed | errors |
| --- | --- | --- | --- |
| `small.en` | 783ms | 26.6× | "Write, so" for "Right, so", "pasta bin", "jemaloc", one run-on sentence |
| `medium.en` | 1720ms | 12.6× | "pastabin", "jemaloc" |
| `large-v3-turbo` | 1426ms | 15.1× | "pastabin", "idempotent **rights**", "Jamaloc" |

Turbo is both larger and faster than medium — almost all of whisper's decode
cost is decoder depth, and turbo has four layers — and it is the worst of the
three here, because it is multilingual where the others are English-only.
"idempotent rights" is a more expensive error than "Write, so": it is wrong and
*plausible*, so nothing downstream has any reason to doubt it.

`medium.en` is therefore the default. At 12.6× real time a 22 second question
decodes in under two seconds, against a nine second answer.

The more useful finding is what none of them fixed. `jemalloc` is in the
priming vocabulary and still came back "jemaloc", while `idempotent`,
`write-ahead log` and `quorum` all landed. Priming raises the odds; it does not
guarantee. And no model size rescues a product name it has never seen, which is
what `--vocab` exists for.

## One thought, four utterances

Watching a real person think aloud, the transcript came out as:

```
you: Hello, testing, um, do you-
you: We think we can...
you: Um... reverse.
you: A linked list.
```

One thought, four utterances, each transcribed with no knowledge of the others
— and whisper is markedly worse on a two-word fragment than on a sentence, so
the fragmentation costs accuracy as well as readability.

The cause is the 600 ms exit window, which was chosen to keep the transcript
close behind the speaker. That was a fair trade when a press read whatever
happened to be in the transcript already. It is not one now: pressing the lever
flushes whatever is mid-sentence and waits for it, so a longer window costs
nothing at the moment anybody is actually reading. Raised to about a second.

## The panel is checkable now

`jay demo` opens the overlay with one of everything in it — a notice, both
speakers, a dropped artefact, a real answer with a real code block — and no
audio, no model, no spending. Meters are driven by a thread writing plausible
levels so the four states can be seen rather than reasoned about.

It exists because the panel was the one part of jay with no way to check it.
The tests can assert that an answer splits into prose and code; they could not
tell anyone that the switch bank came out reading `NUDGE ANSWER GIVES`, which
shipped and was obvious the moment somebody looked at it. Opened, screenshotted
and read, that took thirty seconds to find.

Two more came out of the same first look:

**The answer was being read from its end backwards.** One scroll area held
everything and stuck to the bottom, so the "Approach:" line — the one sentence
the candidate is meant to say out loud first — had already scrolled off by the
time the code finished arriving, and every transcript line landing while they
read pushed it further away. Now the reading has the top and holds its
position, rewinding to the beginning of each new answer; the conversation has
the bottom and chases itself.

**Notices were shouting.** Uppercase with wide tracking is how a machine labels
a dial. Applied to "I WILL NOT SAY ANYTHING UNTIL YOU PRESS ASK JAY" it is how
a machine sounds unhinged. Stencils are for labels; sentences are set in quiet
faint ink.

## Code with a known panic in it is not an answer

The coding prompt asked for edge cases, and got them — as a list underneath
code that did not handle them:

> - Empty grid: `grid[0]` panics, guard with `if grid.is_empty()`.

That is worse than not mentioning it. An interviewer reading a function whose
own author has documented the panic sees somebody who spotted the bug and
shipped it anyway. The prompt now requires the code to handle everything it
lists, and the same question returns
`grid.first().map_or(0, |r| r.len())` with bounds guarded in the recursion.

## The panel is a strip until it has something to say

Opened empty and photographed, the panel was a 620x620 blank slab parked over
the editor, holding two words of "nothing asked for yet" and one faint notice.
The whole justification for an overlay is that it is glanceable without leaving
what you are doing, and it spends most of a session with nothing in the reading
pane at all.

It now opens at 330px — header, meters, switches, and enough room for the
conversation to accumulate — and grows to 640 the first time there is something
to read. It never shrinks again on its own: moving a window somebody is
mid-sentence through is worse than leaving it large.

`jay demo --state empty|writing|answered` draws each moment. All three were
worth looking at; only the last one had been.

## The model narrating its own missing tools

A debrief opened:

> I wrote the tests but the sandbox here declined to run `rustc`, so what
> follows is hand-checked rather than machine-checked.

jay disables every tool deliberately — it wants an opinion, not an agent loose
in a filesystem — but the CLI still carries Claude Code's own system prompt,
which frames everything in terms of tools it might have used. So it apologised
for an absence that was the whole design, in front of an answer somebody is
reading mid-interview.

Every mode is now told, in the shared tail, that it has no tools and should not
remark on it, and to start with the first substantive word rather than a
preamble about its own process. The same debrief came back at **60.1s instead
of 108.7s**, opening "Three things, in order of how much they cost you." A test
asserts every mode and depth carries the instruction.

## What the modes actually produce (measured)

| | latency | notes |
| --- | --- | --- |
| `code` · answer | 9.4s | approach, compiling Rust, complexity, three edge cases |
| `code` · nudge | 5.6s | the trap and the complexity, no implementation |
| `design` · answer | 14.9s | opens with what was missed, numbers, diagram, tradeoffs |
| `design` · nudge | 6.1s | one point: collision handling, counter versus random |
| `debrief` | 60.1s | quotes the attempt back, then the full worked answer |

The debrief is the one mode with no length cap, and should stay that way: it
runs after the interview, where being thorough costs nobody the thread. On a
flawed attempt it found a `usize` underflow in a bounds check that had been
stated out loud and passed unremarked, and told the candidate that naming a
failure mode without offering the fix "reads as luck". That is the mode
earning its place.

## Every reason to bin a transcript now lives in one place

There were two filters, in two `continue`s, thirty lines apart: the artefact
list and the silence floor. A third was wanted, and the shape of the code was
already arguing against it — each check carried its own `tracing` call, its own
panel notice, and its own opportunity for a branch added later to skip the lot.

They are now one function, `jay_stt::judge`, returning `Option<Rejected>`, with
the notice text hanging off the reason. The capture loop asks once. A reason
added tomorrow cannot be silently bypassed, and the panel wording for each lives
next to the rule that produces it rather than at the call site.

## The decoder's own doubt, and the signal that turned out to be dead

Thirty seconds of a silent room, on a machine that had just played audio,
archived two sentences nobody said:

```
[00:21] you: -To us, Adam wanted to speak to us. -It's testing.
[00:24] you: Cool? Distinct.
```

Neither is on any phrase list and neither was quiet — they cleared the speech
floor comfortably, because the room has a fan in it. A blocklist is a losing
race against a model that invents novel fluent English, so the obvious move is
to stop guessing and ask whisper how much it believed itself.

whisper.cpp offers exactly that, per segment: `no_speech_prob`. It reads 0.00 on
`medium.en` for everything, including eight seconds of pure digital silence,
which is the strongest no-speech case that can exist. The reason is in the
implementation: the field is the probability of the `<|nospeech|>` token, and an
English-only vocabulary effectively never predicts it. A gate on that number is
a gate that never closes, which is the same class of fault as a warning light
that is on during normal operation, and worse, because it looks like diligence.

So it is read, reported, documented and **not** acted on. What is acted on is
the mean probability of the tokens actually emitted, which is live: `say`
speech decodes at 0.88, digital silence captioned "Thank you for watching."
at 0.48.

The floor is set at 0.45, which is low, and deliberately. Confidence does not
separate noise from speech on its own — whisper labels white noise `[static]`
with 0.88 confidence, exactly the score real speech gets — so it is a backstop
behind the artefact list and the level floor, sized to err toward keeping a
quiet real sentence rather than binning one.

`crates/jay-stt/tests/silence.rs` holds the corpus and states plainly what it
does not yet prove. Every rejection in it comes from the artefact list, because
whisper hears synthetic noise for what it is; even forty seconds of a real
recorded room produced only `(music)` and `(video plays)`. The fluent case
appears to need what the live pipeline does — short VAD-triggered snippets with
pre-roll, arriving just after real speech stopped — and until a session yields a
corpus of those, the confidence floor is an untested backstop rather than a fix.
Point the test at your own room with `JAY_ROOM_TONE=room.wav`.

## The room is not a tuning problem, but it can still be undone

Without headphones the interviewer's voice leaves the speakers, crosses the
desk and arrives at the microphone, so the same sentence is captured on both
channels and one copy is blamed on the candidate. Wearing headphones remains the
fix. Advice, however, is not a mechanism, and the cost of the room winning is
not cosmetic: the duplicate is spent as context, and a model reading it sees
somebody parroting the interviewer word for word.

`EchoGuard` holds the last four seconds of kept lines and drops a line that
matches a recent line *from the other channel*. Three details are load-bearing:

**Order decides nothing; the channel decides.** Both orders have been observed
on this machine an hour apart, because which copy is transcribed first depends
on utterance length rather than on when the sound happened. So the system tap
always wins — it did not cross a room — and the microphone copy always goes. If
the microphone copy got there first it is retracted from the context, by name,
because the archive is written as lines arrive and cannot be edited afterwards.

**Similarity is measured over the shorter line, not the union.** The microphone
copy is a degraded one. Observed live: the tap heard "An island is a group of
ones connected horizontally or vertically" while the microphone heard "Loop of
ones connected horizontally or vertically". Over the union those score 0.55 and
the echo survives; over the shorter line they score 0.86 and it does not.

**Nothing under eight words is ever an echo.** Both people say "okay, yeah, that
sounds right" constantly and identically, and silently editing a conversation on
the strength of five common words is a worse failure than the one being fixed.
Eight is one above `context::FILLER_MAX_WORDS`, so the two mechanisms meet
rather than overlap: anything shorter is already dropped as filler before it
costs anything.

## What it actually costs to leave running

The README carried "~19 MB" for a listening session, which was wrong by two
orders of magnitude and had no measurement behind it anywhere. A twenty minute
soak with both channels open reads **1.87 GB resident, flat**, and two tenths of
one core.

Both halves of that are worth knowing. The gigabyte and three quarters is
`medium.en` and nothing else, so it moves with the model and not with the length
of the session — there is no leak, and the figure did not shift by a megabyte
between the first sample and the last. The near-zero CPU is the VAD doing its
job: in a silent room nothing reaches whisper, so nothing is decoded.

It matters because a laptop in an interview is also running a browser, a call
and an editor, and 1.87 GB is a real share of that. `--model small` returns
about a gigabyte at the price documented under transcription.

## Permission is attached to the binary, not to the project

`target/debug/jay` and `target/release/jay` are two different applications as
far as macOS is concerned, and a grant given to one means nothing to the other.
Measured within the same minute, with the same room and the same command: 498
frames of an expected 500 from the release binary, and 0 of 375 from the debug
one. Neither reported an error.

This is the same trap as the denied microphone returning silence, one level up,
and it is easy to lose an hour to after a `cargo clean`. Capture is tested with
the release build.

## Counting two channels against one channel's expectation

`jay listen --source both` reported "frames delivered : 545 (expected roughly
312)", because the count spanned both channels while the expectation was
computed for one. A healthy run therefore read as a 75% over-delivery, which is
precisely the kind of number that sends somebody looking for a fault that is not
there. The expectation is now multiplied by the number of channels actually
opened: 498 of 500.

## The fabrications are not from silence, they are from the speakers

The received story here — whisper invents from silence, so gate on level and on
the decoder's own doubt — turns out to be wrong about this machine, and it took
a controlled pair of runs to see it.

**Twelve minutes of a real room, nothing playing: the transcript is empty.** Not
one invented line. Whatever the phrase list and the level floor are doing, they
are not being tested by an idle room, because an idle room produces nothing to
test them with.

**Thirty seconds with the interviewer's question played through the speakers:
four invented lines**, all attributed to the candidate.

```
[00:19] you: I didn't drink no coals.
[00:22] you: E mi da?
[00:29] you: Amortised and...
[00:33] you: The moral of the French logic is this,
```

So the source is not silence. It is the other person's voice arriving at the
microphone across a desk, quiet and smeared, and being decoded into fluent
English that has nothing to do with what was said. The same mechanism as the
duplicate-question echo, one notch further degraded: when the bleed transcribes
faithfully you get the question twice, and when it does not you get this.

Which also explains why text similarity cannot catch it. `EchoGuard` compares
words, and "I didn't drink no coals" shares none with "an island is a group of
ones connected horizontally or vertically". There is nothing to match.

And confidence cannot catch it either. Measured on the run above:

| | mic RMS | confidence |
| --- | --- | --- |
| invented, from bleed | 0.0216 | 0.71 |
| invented, from bleed | 0.0271 | 0.82 |
| the real question, on the tap | 0.1030 | 0.87 |

A floor set high enough to reject 0.82 rejects real speech at 0.87 the same
afternoon. The confidence check stays as a cheap backstop for the genuinely
unbelieved — digital silence captioned "Thank you for watching." scores 0.48 —
but it is not the fix for this and must not be described as one.

The number that does separate them is in the left column, and it is a *ratio*
rather than a level: the bleed is three to five times quieter than the source,
and it arrives while the system channel is itself mid-utterance. A cross-channel
rule — a microphone utterance overlapping a system utterance and substantially
quieter than it is bleed — would catch all four, and would not have needed the
transcript at all.

That is deliberately not built yet. It can silently discard the candidate
interrupting the interviewer, which is a worse failure than four visible lines
of nonsense, and calibrating the ratio needs two real people in a real call
rather than a laptop playing a WAV at itself. It is the next change, and the
first live session is what it is waiting for.

Until then the honest instruction is the one the README already gives, promoted
from advice to prerequisite: **wear headphones**. It removes the loudest source
of this by a wide margin.

### Correction, the same day

"With no bleed there are no fabrications" was too strong, and a later run
disproved it: sixty seconds in a room with nothing playing produced six invented
lines, among them "Jeff, you are not a nerd" and "You're kind of a scammer".

The variable is not the speakers. It is whether anything trips the VAD at all.
The twelve minute soak was clean because the room was quiet enough that no
utterance ever reached whisper; the sixty second run was not. Speaker bleed is
the most reliable way to trip it, which is why it produces fabrications so
consistently, but a fridge, a fan or a chair will do the same job.

The rule that survives both observations: **whisper invents whenever it is
handed audio that is not speech**, and everything upstream of it is a question
of how often that happens.

A second hypothesis died the same afternoon. Given that `--vocab` had been
supplied in the noisy run and not in the clean soak, priming looked like a
plausible cause — a list of words to reach for. Measured directly, on the same
recorded room audio, primed against bare: four chunks each, zero survivors
either way. Priming is not implicated. `tests/silence.rs` keeps that measurement
so the idea does not have to be re-had.

## The transcriber reciting its own prompt

Priming does have exactly one failure mode of its own, and it is not the above.
From a live session, on a channel with nothing on it:

```
[00:12] them: Redis  Kafka
[00:20] them: Redis  Kafka
```

Nobody said that. The session was primed with `--vocab "… Redis, Kafka"` and the
decoder handed the prompt back. These clear every other filter — not a known
artefact, not quiet, and decoded *confidently*, because copying is easier than
guessing.

`is_prompt_echo` rejects a transcript whose every word appears in the priming
prompt in the prompt's own order. The first attempt required a contiguous run
and was beaten within one test run by `Postgres  nginx  Kafka`, which skips
"Redis" out of the middle of the list it is reciting. Order is the discriminator
that holds: a vocabulary list's order is arbitrary, so reproducing it is not
something speech does by accident, while real speech using those words puts its
own words between them.

Two words minimum. The cost is that somebody who says nothing but "Postgres,
Redis" in that order loses it, which is two words nobody could have acted on.

## The subtitle dash

`- (speaking in foreign language)` walked through the bracketed-marker check on
a live session, because the check asked whether the line *starts* with a bracket
and this one starts with the dash that introduces a speaker turn in subtitles.
The same training data that supplies "I'll see you next time" supplies the dash.
Leading dashes and full stops are now trimmed before the test.

## Every exit aborted

Three crash reports, three identical stacks, one line of explanation from ggml:

```
abort <- ggml_abort <- ggml_metal_rsets_free <- ggml_metal_device_free
      <- __cxa_finalize_ranges <- exit

// note: if you hit this assert, most likely you haven't deallocated all
// Metal resources before exiting
```

ggml frees its Metal device from a static destructor at `exit` and asserts if
any Metal resource is outstanding. jay's whisper context is one. Under the panel
it lives on a pipeline thread that is never joined, because the windowing event
loop owns the main thread and only returns once the window has closed.

The obvious fix is to join that thread, and it is the wrong one. The terminal
path *already* joins its transcription thread and drops the model, and it
aborted too — so something in the whisper context outlives the Rust value that
appears to own it, and no amount of tidying on jay's side clears the assert.

So jay leaves through `_exit`, which ends the process without running atexit
handlers or finalising shared libraries. Nothing of jay's depends on them: the
session archive is written with unbuffered writes as each line arrives, and the
two output streams are flushed immediately before.

It never affected a running session. It did mean every single quit produced a
crash report, which is precisely the sort of thing that trains somebody to
ignore crash reports.

## Two people in one room

The worst session yet was not a bug. Both people sat side by side rather than on
a call, so both voices arrived on the microphone, and everything downstream that
depends on two channels quietly stopped meaning anything: attribution collapsed
onto `you:`, the interviewer's questions were archived as the candidate's, and
the problem statement was never pinned, because pinning only fires on the system
channel.

The meters showed it the whole time — `them` reading QUIET beside a busy `you` —
and that was not enough, because a meter tells you a number and not what the
number implies. After 45 seconds of somebody talking into the microphone with
zero frames ever delivered by the tap, jay now says it in words. Once, and only
with `--source both`, and only after long enough that a genuine pause between
questions cannot trigger it.
