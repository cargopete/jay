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

## The system tap runs and returns silence (open)

Current state, recorded so it is not rediscovered from scratch.

The tap starts, reports 48 kHz, and its IOProc fires steadily: 276 frames in
ten seconds with a 4 ms queue lag and zero drops. Every sample is zero, during
playback that is plainly audible from the speakers.

Ruled out so far:

- **Not a missing clock.** Adding the default output device as
  `kAudioAggregateDeviceMainSubDeviceKey` and to the sub-device list changed
  nothing. The IOProc was firing before that fix and after it.
- **Not the plain-binary Info.plist.** Wrapping the binary in a signed `.app`
  with `NSAudioCaptureUsageDescription` (see `scripts/bundle.sh`) also changed
  nothing.

What the evidence points at: `tccutil reset AudioCapture dev.cargopete.jay`
succeeds, so macOS does track an `AudioCapture` grant for the bundle. But
`log show --predicate 'subsystem == "com.apple.TCC"'` shows **no request from
jay at all**. The process is never asking, so it is never prompted, and an
unauthorised tap is fed silence rather than an error.

The likely cause is TCC responsibility attribution: a binary launched from a
shell inherits the responsible process of whatever owns that shell, so the
grant would have to belong to the terminal rather than to jay. The next things
to try are launching via `open -a` so LaunchServices gives the app its own
identity, and granting audio recording to the terminal directly.

Worth noting for the design: a tap that returns silence looks exactly like a
quiet room. The `listen` command reports peak RMS and shouts when every frame
is zero precisely so this class of failure cannot masquerade as success.

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

## Backend is the Claude Max subscription via the local CLI

Not an API key. The `claude` CLI already holds the OAuth session, so headless
invocation uses the subscription. Worth noting the metering rules here have
been in flux and should be checked against current policy rather than assumed.
