# Running a mock interview with jay

Written for a two-part loop — 40 minutes of coding, 40 of system design — with
a partner playing interviewer.

## First, run the preflight

macOS permissions fail silently — an ungranted audio tap returns silence, an
ungranted screen capture returns an error nobody reads — so the only way to know
is to try each one and look.

```sh
scripts/bundle.sh debug
open -a "$PWD/target/debug/jay.app" --args check --out check.txt
cat check.txt
```

It exercises the microphone, the system tap, screen capture, the whisper
weights and the `claude` CLI, and it warms the prompt cache while it is at it.
Expect this:

```
  mic       OK   MacBook Pro Microphone
  system    OK   tap running at 48000 Hz
  screen    OK   captured 412 KB
  whisper   OK   …/ggml-small.en.bin
  claude    OK   6.6s, $0.0135 — cache is now warm
```

**If `screen` says FAIL**, jay has just asked macOS for the permission, so it
will now appear in **System Settings › Privacy & Security › Screen & System
Audio Recording**. Tick it, then run the check again. Left unfixed, every button
press sends the conversation and silently no screenshot.

(If it still fails with the toggle on, that is not your setup — it means
something has reintroduced the old subprocess capture. jay captures in process
precisely because a spawned `screencapture` does not inherit the grant.)

## Before they arrive

**The interviewer is on a call**, so their voice arrives on system audio and
yours on the microphone. jay uses that to tell you apart, which matters more
than it sounds: it means your own thinking aloud is recorded as context but
never mistaken for a question, and most of what you say while solving something
is thinking aloud.

That needs `--source both`, and system audio needs the LaunchServices launch
below. Launch it from a shell instead and macOS feeds the tap an unbroken
stream of zeros — no error, no warning, it simply never hears her.

(If you ever do run in the same room, use `--source mic`. jay will say in the
panel that it cannot tell who is speaking and treat every question as worth
answering, which is the honest fallback.)

**Warm the cache.** The first `claude -p` call of the hour costs about
$0.025 and every one after it about $0.003, because the CLI's ~29,000-token
preamble is cached for an hour. Ask it something trivial before you start:

```sh
jay ask --mode coding --hint "warm up"
```

**Write the brief.** Two or three sentences about who you are and what you have
actually operated beats a hundred lines of project index — measured, and the
long version was worse. Keep it under a page.

```sh
jay brief --out brief.md --match indexer --match gateway --match rust
$EDITOR brief.md          # fill in "Who you are", delete the rest
```

## Launching

```sh
scripts/bundle.sh release          # re-run this after every rebuild
open -a "$PWD/target/release/jay.app" --args \
  transcribe --overlay --source both \
  --mode coding --brief brief.md \
  --save part1.txt --seconds 0
```

`scripts/bundle.sh` **copies** the binary, so a bundle built before your last
`cargo build` runs the old code. This has already caught me out once.

`--seconds 0` runs until you close the panel. `--save` is not optional in
practice: `open -a` detaches jay from the terminal and takes stdout with it, so
without it the only record is what fits in the panel — and the debrief
afterwards wants the whole session.

Start a second run with `--mode system-design --save part2.txt` for part two.

## During

**jay never volunteers.** It listens, transcribes and stays quiet until you
press **ask jay**, and there is no setting that changes that. Nothing is spent
you did not ask for, and a panel that only speaks when spoken to is one you can
forget is running.

Pressing it sends the pinned problem, the recent conversation, and a screenshot
of the focused window — so if the question is about the code you are looking at,
look at it before you press. It picks what to answer by walking back for the
most recent thing the interviewer actually asked, rather than taking the last
line, which is usually you mid-sentence.

Switch modes between parts. `--mode coding` gives compiling Rust, the
complexity and the edge cases; `--mode system-design` gives capacity numbers, a
component diagram and the decisions with their tradeoffs. Add `--hint` to either
when you want a nudge instead — approach and complexity, under forty words, no
implementation, and about three times faster.

## Afterwards

The debrief is the part that makes you better, and it is a different mode:

```sh
jay ask --mode rehearsal --brief brief.md --context transcript.txt \
  "Return the size of the largest connected flooded zone in a grid."
```

It leads with what your attempt missed, quoting you, then gives the worked
answer. On a real transcript it caught a *habit* rather than a knowledge gap —
"that hands the wheel to the interviewer" — which is the sort of thing that
costs you an offer and that nobody tells you.

## What to expect from the numbers

| | |
| --- | --- |
| Idle CPU while listening | 12–25% of one core |
| Memory | ~19 MB, flat |
| Whisper `small.en` | comfortably faster than real time on an M3 Pro |
| A hint | ~5s, ~$0.14 |
| A full answer with code or a diagram | ~16–20s, ~$0.20 |

Budget accordingly: `--budget` defaults to $2.00 a session and stops
suggestions once spent. It is checked before a call rather than during one, so
it can overshoot by the cost of whatever was in flight.

## If something goes wrong

**The panel says nothing.** Check the transcript is appearing at all. If it is
not, and you are on `--source both`, you almost certainly launched from a shell
instead of through `open -a`, and macOS is feeding the tap silence.

**Suggestions never fire on their own.** By design. Press the button.

**"still thinking about the last one".** A suggestion takes up to twenty
seconds and only one runs at a time. This is deliberate: the alternative is the
transcriber stalling behind it and losing audio.

**Words are wrong in the transcript.** `--model small` is the default; `--model
base` is faster and worse on jargon. There is no larger option wired up yet.
