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

**And then it came back, on longer sessions, and this half is *not* fixed.**

Three real deadlocks were found and removed on the way, and each is worth having
gone regardless:

1. The pipeline joined the assistant thread, which waits on a channel whose other
   sender lives in the panel, which waits on the pipeline. The assistant is now
   released rather than joined — a suggestion still in flight at the end of a
   session is one nobody is going to read.
2. The request thread ran `for request in rx`, ending only when the panel drops
   its sender, and eframe does not reliably drop the app struct on macOS under a
   bundle. It now polls and stops on `INTERRUPTED`.
3. The capture threads parked inside `send` once the loop stopped draining the
   frame channel, and `Drop` joined them. They now use `send_timeout` and check
   their stop flag, and the captures are dropped the instant the loop ends
   rather than when the function returns.

After all three, `jay-pipeline` exits cleanly and every worker is released. What
remains is the window itself, and it is not jay's to fix:

```
threads alive : jay-assist  jay-hand-ask  jay-save
main thread   : _CFRunLoopRunSpecific        (idle, not drawing)
pipeline      : gone — it finished and set the flag
```

`Closer::close` stores the flag *and* calls `request_repaint` on the panel's own
context, and the panel still never runs another frame. macOS stops drawing an
unattended window, and a redraw request against a window it has stopped drawing
does not bring it back. Every one of these tests ran for minutes with nobody at
the keyboard, which is exactly the condition that provokes it.

So: **a timed session launched from the bundle still leaves its window open and
its notes unwritten.** A session ended by closing the panel — which is how jay is
actually used — writes them every time. `--terminal` with `--seconds` also works,
having no window to leave open.

The fix worth trying next is to stop routing the notes through the window at all:
write them from the pipeline thread when the session ends on its own, so the
deliverable never waits on a redraw. The ordering against the archive writer
needs care, which is why it is written down rather than rushed in.

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

**Fixed.** `Whisper::load` now runs *after* the captures are open rather than
before, and the frame channel went from 512 to 4096 slots — about 65 seconds a
channel, 8 MB, against the 1.5 GB the model is about to take. Frames pile up
during the load and are segmented in one burst afterwards; nothing downstream
notices, because every stamp comes from `frame.captured_at` rather than from
when the frame was read.

It was worse than "lost", incidentally. The devices were not open at all during
the load, so the audio never existed to be lost.

Verified: a sentence spoken at the instant of launch now lands at `[00:00]`.
About the first second is still missing while the tap itself starts, which is a
much smaller hole and a separate one.

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

**Built, and it does better than the middle ground.** `--attendees`, falling back
to `~/.config/jay/attendees`. The roster is appended to the notes system prompt
— appended, so the standing instructions stay byte-identical and cacheable — and
the model is told to use a name *only* where the transcript makes the speaker
plain, and to keep `them` everywhere else.

Run against the 72-minute seven-person weekly, which is the hardest case
available:

| | without a roster | with one |
| --- | --- | --- |
| named far-side actions | 0 | 4 |
| bare `them` actions | all of them | 5 |
| wrong names | — | none found |

The transcript itself is unchanged and still says `them` throughout. This puts
names on the document somebody reads, not on the audio, and it is honest about
which of the two it is doing.

The remaining ergonomic gap is that the roster has to be typed or maintained by
hand. Pulling it off the calendar event that is running now is the version that
would actually be used every time.

### 8. Per-meeting vocabulary is manual

`--vocab "names, products, jargon"` has to be typed for every session and is
the difference between `Patroni` and `Petrino`. Nobody will do this reliably at
the start of a call they are already late for.

Ideas, cheapest first: a `~/.config/jay/vocab` read by default; a `--vocab-file`
flag; pulling attendee names off the calendar event that is running now.

**Done, the cheap half.** `~/.config/jay/vocab` is read whenever `--vocab` is
absent: one term per line, `#` comments and blank lines ignored. The standing
names live there and only today's peculiar ones need typing.

Pulling attendees off the running calendar event is still the version that would
actually be used, and it is also the thing that would make item 7 tractable,
since a roster is exactly what contextual attribution needs.

---

## Unverified

Ideas without evidence behind them yet. Move them up when a session earns it.

- ~~Fabricated `you:` lines in a real room.~~ **Confirmed, moved to item 15.**
- One-off fabrications that neither repeat nor change script are still not
  caught, and are the last of this family. `I don't believe in gay.` came out of
  a silent room, reads as ordinary English, and clears every filter jay has.
  There may be no cheap signal for it.
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

### 15. Whisper still invents sentences in a quiet room

Confirmed on this machine, in a run where nothing was being said and nothing was
playing:

```
[00:29–00:38] you: I'm not sure what to say. I'm not sure what to say. I'm not
                   sure what to say. I'm not sure what to say. I'm not sure what
                   to say.
[00:38–00:41] you: I'm gonna say sucho. Now I'm gonna say such.
```

Five repetitions of a sentence nobody uttered. The earlier `Skitty? Why is
kitty...` was ambiguous; this is not.

The VAD is what should have stopped it, and did not: something in the room
crossed `SPEECH_THRESHOLD` and whisper, handed a stretch of near-silence it had
agreed was speech, reached for the most fluent thing it knew. The existing
defences — the artefact phrase list, the level floor, the mean-probability
floor — are all aimed at *what came back*, and this got through all three.

The tell that none of them use is **repetition**. A line that says the same
clause five times is not a sentence a person said; it is a decoder looping. That
is cheap to detect and specific enough not to catch real speech, since people
repeat words but not whole clauses verbatim five times running.

**Fixed.** `jay_stt::is_looping`, a sixth `Rejected` variant, checked before the
level and confidence floors because a loop clears both. Splits on sentence
punctuation rather than words, since the unit that loops is the clause, and
allows three consecutive repeats before rejecting.

Three, not two, and the negative test is the important one: `"Yeah, yeah,
yeah."`, `"Good day, good day."` and `"Glad to hear. Glad to hear. Yes. Very
good. Very good."` are all real lines from real transcripts on this machine, and
all three survive. Clauses under twelve characters are ignored entirely, because
"Ah. Ah. Ah. Ah." is a person reacting and binning it would be editing the
meeting rather than transcribing it.

---

### 16. An English-only decoder writing in another script

The other half of item 15, and it has an even sharper tell. Observed on the same
machine, on a `you` channel with nothing being said:

```
[00:19–00:23] you: сака постова дажет неток
[00:54–00:55] you: El cadet es me das trilis.
```

`medium.en` has no business emitting Cyrillic. When it does, it is not
transcribing, it is inventing.

**Fixed.** `jay_stt::is_foreign_script`, a seventh `Rejected` variant. A *script*
check rather than a language one, because script is blunt and hard to get wrong:
Cyrillic, Greek, Arabic, Hebrew, Armenian, Devanagari, Thai, Kana, Hangul and CJK
are all rejected, and Latin is left entirely alone. Accented letters are ordinary
in names — Paola, Müller, café — and binning a line over a diaeresis would be a
worse fault than the one being fixed.

Applied only when the weights are English-only, which is every model jay wires up
except `turbo`. On multilingual weights the same output might be somebody
actually speaking Bulgarian, and the flag is computed where the model is known
rather than in `judge`, which cannot see it.

Note what this does *not* catch: `El cadet es me das trilis` is Latin script, so
it survives. The Cyrillic case is closed and the Romance-language case is not.

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
