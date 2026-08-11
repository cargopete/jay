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

## Frames are 20 ms

320 samples at 16 kHz. It is the frame size Silero VAD is happiest with and
small enough that the latency budget stays honest.

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
