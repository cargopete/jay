# Improvements

A running list. Each entry says what was observed, on what evidence, and what
would fix it. Ideas without evidence go at the bottom, under *Unverified*, and
move up when a session proves them.

Sessions referenced here live in `~/Library/Application Support/jay/sessions/`.

---

## Observed on 2026-09-01, first run on a fresh machine

Setup: MacBook Pro built-in microphone and speakers, no headphones, output
volume 75. Other side dictated through `scripts/dictate.sh`. One 120-second
session, `2026-09-01-123426.md`.

### 1. The panel outlives the clock, and the notes never get written

`--seconds 120` stops the capture on time, but the window stays open
afterwards and the notes are only written when it closes. Measured: the process
was still alive at 3:22 with `--seconds 120` set, holding no child process and
no open socket, and a sentence spoken loudly at 3:20 produced no new transcript
line. So capture had ended cleanly some ninety seconds earlier and nothing said
so.

From the outside this is indistinguishable from a session still recording. A
user who starts a timed session and walks away comes back to a panel that looks
busy and a directory with no notes in it.

Fix: when the capture loop exits of its own accord, either send
`ViewportCommand::Close` or write the notes at that point and put
`recording finished` in the panel. The second is better, because the transcript
is already complete and there is no reason to make the notes wait on a mouse.

**Fixed**, and it took two goes because there were two deadlocks stacked on each
other.

The panel now watches an `AtomicBool` that `run_pipeline` raises the moment the
transcript is complete, and closes itself. The flag has to be raised from
*inside* `run_pipeline`, before it joins the assistant thread, or the pipeline
waits on the panel and the panel waits on the pipeline.

Behind that sat a second one, and it only ever appeared through the `.app`
bundle, which is the only way jay is really launched. The request-handling thread
ran `for request in rx`, which ends only when the panel drops its sender — and
eframe does not reliably drop the app struct on macOS when `run_native` returns
under a bundle. So that thread parked forever holding a clone of the question
channel, which the assistant waits on, which `run_pipeline` joins, which `main`
joins. It now polls with a timeout and stops on `INTERRUPTED` instead of trusting
a channel to close.

A 25-second session through the bundle: closed itself after 39 seconds, notes
written, nobody touched the panel.

### 2. There is no minimise button

`crates/jay-ui/src/lib.rs:903` has only `×`, which closes the session outright.
During a real meeting the panel is in the way and the only way to move it out of
the way is to end the recording.

Fix: a second small button beside `×` sending
`ViewportCommand::Minimized(true)`. Cheap. The thing to be careful about is that
minimising must not be mistakable for stopping, so the meters and the recording
state need to survive it and the title bar wants to still say it is recording
when restored.

### 3. Echo duplicates stay in the human-readable transcript

Confirmed working as designed, and still the biggest readability problem.

Every dictated line was captured twice, once per channel, at a microphone peak
of 0.0408 against a documented speech level of about 0.02. jay caught all four
and dropped them from the model's context, so the notes are protected. But the
transcript a person reads still contains this:

```
[00:03–00:11] them: Right, the thing I wanted to talk about is the rate limiter…
[00:03–00:11] you:  Right, the thing I wanted to talk about is the rate limiter…
[00:12] (1 earlier "you" copy of "Right, the thing I wanted…" was this room, not you; dropped from context)
```

Three lines where one was said. In a 75-minute meeting that is an unreadable
document, and the target here is a transcript someone would actually read.

Fix, without deleting anything: keep the record complete but stop rendering it
flat. Mark the retracted copy inline on the line it duplicates rather than
giving it its own `you:` entry and its own notice, so the reader sees one
utterance with a note attached. The archive keeps everything; the rendering
stops repeating itself.

### 4. Degraded echo is a fabrication waiting to happen

The same sentence, the two channels, one session:

```
them: I think we should shard by Tenant rather than buying a bigger box.
you:  I think we should shard by ten and rather than buying a bigger box
```

`Tenant` against `ten and`. Here it merely garbled, and the matcher still caught
it. One notch further degraded and it stops resembling the original closely
enough to match, at which point it survives as a sentence attributed to a person
who did not say it. This is the documented failure mode, and this session shows
the exact halfway state.

Fix: nothing new is needed if headphones are worn. If the no-headphones path is
to be supported properly, the matcher wants to be fuzzy rather than exact, since
the copy it needs to catch is by definition the one that transcribed badly.

### 5. Audio is lost while the model loads

A sentence spoken before `medium.en` finished loading never reached the
transcript at all. The session clock is deliberately started after the load so
the stamps stay honest, which is right, but the audio in that window is simply
gone.

Fix: open the captures first and buffer into a ring while the model loads, then
decode the backlog once it is ready. Costs a few seconds of memory and removes
the "start it a minute early or lose the opening" rule, which is exactly the
kind of rule nobody remembers under pressure.

### 6. A mute hotkey

Already on the README's list. It is now the top item rather than the fourth,
because the no-headphones workflow depends entirely on muting yourself when not
speaking, and the switch currently needs a mouse and a focused window. A mute
you cannot reach is a mute you do not use.

`CGEventTap` plus an Accessibility permission. The permission is worth the
prompt.

---

## Toward a transcript someone would read

The bar is a transcript that reads like a document rather than a log.

### 7. The far side is one speaker

jay's separation is physical, so one person and six are both `them`. This is the
single largest gap against tools that diarize, and it is a different project
rather than a tuning exercise. Worth writing down that it is known rather than
letting it be rediscovered.

Middle ground, if full diarization is too much: let the session be told the
attendees up front and have the notes say "someone on the call" rather than
`them`, which at least stops the notes reading as though there were two people
in every meeting.

### 8. Per-meeting vocabulary is manual

`--vocab "names, products, jargon"` has to be typed for every session and is
the difference between `Patroni` and `Petrino`. Nobody will do this reliably at
the start of a call they are already late for.

Ideas, cheapest first: a `~/.config/jay/vocab` read by default; a `--vocab-file`
flag; pulling attendee names off the calendar event that is running now.

---

## Unverified

Ideas without evidence behind them yet. Move them up when a session earns it.

- Fabricated `you:` lines in a real room. This session produced
  `[01:32–01:33] you: Skitty? Why is kitty...` with nothing dictating at the
  time. **Needs confirming with Chief whether that was actually said.** If it
  was not, it is the first fabricated line observed on this machine and belongs
  above rather than here.
- Resident memory read 304 MB during this session against 1.87 GB documented in
  the README. Either the figure has moved or the model is mapped rather than
  resident. Not a problem either way, but the README should not be quoting a
  number that is six times the truth.
- A written-as-you-go notes file rather than one written at the end, so a
  session that dies mid-meeting still leaves something.

---

## After the real meeting

Session `2026-09-01-124404.md`. A 19-minute onboarding call, built-in
microphone and speakers, no headphones. 68 `them` lines, 55 `you` lines, 37
notices, zero `?` lines.

### 9. Echo arrives interleaved, not duplicated, and the matcher cannot see it

This is the finding of the session and it is worse than the documented
duplicate-line problem.

The matcher catches a mic copy that resembles a system line. It caught 31 of
them here and did its job. But at conversational pace the microphone does not
receive a clean copy of the other person's sentence. It receives the tail of
their sentence, then the speaker's own reply, and the VAD hands whisper **one
utterance containing both people**. The result is not a duplicate of anything,
so nothing matches, and it is archived as a single `you:` line.

```
[13:50–14:10] you: only ship the delta over time. … You will have plenty of
                   time to explore on your own. …
[13:59–14:24] them: have plenty of time to explore on your own. There's a lot of
                    variations of it as well. …
```

Everything in that `you:` line was said by the other person. 27 of 55 `you:`
lines begin mid-sentence on a lowercase word, which is the signature: an
utterance that starts in the middle of somebody else's clause. Reading them,
the majority of the `you` channel in this session is the far side bleeding
back.

The consequence is the one the README already fears in another context:
**attribution collapses, and the notes will assign the wrong person's
commitments.** Everything the manager undertook to do reads as though the
candidate undertook it.

Fixes, in order of honesty:

- **Headphones.** Removes the cause. Nothing else here comes close, and this
  session is the evidence rather than the assertion.
- **Detect the condition and say so.** jay already warns when the system
  channel is silent beside a busy mic. The mirror case deserves a warning too:
  if a large fraction of mic utterances overlap a system utterance in time,
  the room is feeding back and the panel should say so in words, once, early.
  A ratio of overlapping-to-total on the mic channel over the first two
  minutes would catch it.
- **Suppress at the segmenter rather than after the transcript.** A mic
  utterance whose window sits inside a system utterance's window is echo
  regardless of what the words turn out to be. Cheaper than text matching and
  it catches the blended case, which text matching structurally cannot.

### 10. Room noise before the call became speech

The first four `you:` lines, before anyone had spoken, were
`This is what's done by gums.`, `Bye.`, `I should get up then.` and `Hello.`
The last is probably real. The rest are the documented invention-from-silence.
The artefact filter caught `(speaking in foreign language)` and `(laughs)`
correctly, so the list is working; these were simply not on it.

### Still open from the test session

The `Skitty? Why is kitty...` line remains unconfirmed.

---

## Tried and rejected: the platform echo canceller

`kAudioUnitSubType_VoiceProcessingIO` is the obvious fix and it does not work
here. Written up because the next person to have this idea should not have to
spend the afternoon finding out, and because the reason is structural rather
than a bug to be fixed.

Implemented in `crates/jay-audio/macos/voice_mic.m` and
`crates/jay-audio/src/voice_mic.rs`, selectable with `--mic-path plain|aec|bypass`
on both `listen` and `transcribe`. The code is kept, because the measurement is
worth being able to repeat.

**It takes the `them` channel with it.** Same dictation, same speakers, same
volume, one flag apart:

| `--mic-path` | `them` lines | echo on `you` |
| --- | --- | --- |
| `plain` | present, verbatim | present, caught by the guard |
| `aec` | **none at all** | none |

The system tap is a `CATapDescription` process tap built around the default
output device. That device does not change while the voice unit runs — checked,
it stays `MacBook Pro Speakers` throughout — so the tap is pointed correctly and
still receives nothing. Whatever mode the voice unit puts the output device
into, a process tap cannot see through it.

Losing the far side to cancel the echo is not a trade worth making. The far side
is the thing jay is for.

**The gain control cannot be switched off either.** `kAUVoiceIOProperty_VoiceProcessingEnableAGC`
returns `noErr` on both element 0 and element 1 and changes nothing: a silent
room reads 0.0669 RMS with processing on against 0.0014 with it bypassed. Every
level threshold jay owns — the 0.003 resting room, the 0.02 speech level, the
0.012 brief-and-faint floor — is meaningless on that path. Even had the tap
survived, this would have needed recalibrating from scratch.

### 11. What to do instead: cancel in software, against the tap

The honest fix left standing. jay already captures the exact far-end signal,
which is a better reference than most echo cancellers ever get — they estimate
what the speakers were fed, and jay has a recording of it.

An NLMS adaptive filter over the two 16 kHz mono streams, mic as the desired
signal and the system tap as the reference. The hard parts are the delay
estimate between tap and microphone, which is one cross-correlation at startup
and then tracking, and double-talk detection so the filter stops adapting while
both people speak.

Real work, and it stays inside Rust with no new frameworks and nothing that can
take the far side away.

### 12. The cheap interim: close the microphone utterance when the far side speaks

Worth having whether or not 11 gets built, because it addresses the actual
damage rather than its cause.

The wreckage is not that the echo exists, it is that the microphone segmenter
never reaches its exit condition, so utterances run to `MAX_UTTERANCE` and are
cut at 25 seconds with both speakers inside them. Nineteen of fifty-five.

If the microphone segmenter is told when the system channel is speaking, it can
close its current utterance and hold rather than accumulating. Half duplex: an
interjection made while the other person is talking is lost. That sounds worse
than it is, because today such an interjection is *already* lost, buried in a
25-second chunk attributed to the wrong person and blended with their words.

**Built.** `SpeechSegmenter::push_gated`, on by default, `--no-echo-gate` to
turn it off when wearing headphones. One line does the work: a gated frame is
still fed to the recurrent detector but cannot count as speech, so it neither
opens an utterance nor keeps one open.

Measured on the same dictation, same speakers, same volume as everything above:

| | before | after |
| --- | --- | --- |
| `them` lines | 4, verbatim | 4, verbatim |
| duplicate `you` copies | 4 | **0** |
| leaked `you` lines | 4 | 1 |

The gate says so in the panel when it first fires and reports its total at the
end of the session, because a mechanism that removes speech must leave a mark.

### 13. The gate loses the race at the start of a turn

The one line that still leaks, and it is the same line every time:

```
[00:00–00:02] you:  What we talk about is the rate limiter. It is destroyed.
[00:00–00:05] them: It is distributed across nine nodes and the counters drift under load
```

`ENTRY_FRAMES` is 2, so the system segmenter takes two frames to open, and for
those ~64 ms the gate is down and the microphone opens on the echo instead. It
then holds that fragment until its own hangover expires. The text-side echo
guard does not catch it either, because the microphone's copy of the first
fraction of a sentence is too garbled to match the system's copy of the whole
one.

Fixes worth weighing: gate on the raw per-frame VAD probability rather than on
the segmenter's opened state, which removes the two-frame lag at the cost of
opening the gate during every pause inside a sentence; or let a system utterance
retroactively withdraw any microphone utterance that opened within the last
half second, which is exactly the retraction machinery `echo.rs` already has for
the aligned case.

**Confirmed on a real call, and it is the dominant residual.** Session
`2026-09-02-090317`, 44:30, onboarding 1:1 on speakers:

| | 2026-09-01, no gate | 2026-09-02, gate on |
| --- | --- | --- |
| microphone utterances at the 25s cap | 19 of 55 | **0 of 114** |
| longest microphone utterance | 25s (the cap) | 18s |
| `you` lines that are really the far side | most of them | 52 of 114, all ≤2s |
| `you` lines that are really Chief | a minority, blended | 62 of 114, clean |

The cap is no longer being hit at all, which was the whole point: the segmenter
now sees silence and produces real sentence boundaries. What is left is 52
one-and-two-second fragments, each sitting immediately before the `them` line
whose opening words it contains:

```
[00:41–00:42] you:  art that you're in like.
[00:41–00:50] them: architecture and sharing and not incure these egress costs …
```

**Fixed.** Not by retracting them afterwards, which could not have worked: the
microphone's copy of half a word transcribes to something that looks nothing
like the system's copy of the whole sentence, so there is no text to match on.

The cause was one frame. Both segmenters need `ENTRY_FRAMES` before they open,
both hear the far side's first syllable in the same frame, and which one opens
first is decided by the order the two frames happen to come off the channel.
`SpeechSegmenter::is_or_becoming_speech` reports `in_speech || speech_run > 0`,
so the gate closes on the system channel's *first* speech frame rather than on
its second. One frame of margin, and it costs a stray noise frame gating the
other channel for 32 ms.

Measured over eight dictated turns, which is eight chances to leak:

| | before | after |
| --- | --- | --- |
| `them` lines | 10 | 10 |
| `you` lines | one per turn | **1**, plus one `?` fragment |

### 14. Notes survive a damaged transcript better than the transcript does

Worth writing down because it changes where effort should go.

Yesterday's notes were assigned correctly — every `you` and `them` action on the
right person — off a transcript where most of the `you` channel was the other
speaker. The model read meaning rather than trusting the channel labels. Today's
notes are better still: `Decisions: none` where that was true, four actions each
cited, two genuinely open questions.

So the notes are not the weak link and have never been. The transcript is what
somebody actually reads back, and it is the half that shows the damage.
