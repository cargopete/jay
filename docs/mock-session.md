# Running a mock interview with jay

Written for a two-part loop — 40 minutes of coding, 40 of system design — with
a partner playing interviewer.

## Before they arrive

**Decide where the interviewer's voice comes from.** This is the one setting
that changes how jay behaves, and getting it wrong is silent rather than loud.

- **Same room.** Both voices arrive on the microphone. jay cannot tell you
  apart, says so in the panel, and treats every question as worth answering.
  Run with `--source mic`. Nothing else to do.
- **Over a call.** Their voice arrives on system audio and yours on the mic, so
  jay knows who is asking. Better: it will ignore your own thinking aloud, which
  is most of what you say while solving something. Run with `--source both`,
  which needs the LaunchServices dance below.

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

**Same room, panel on top of your editor:**

```sh
jay transcribe --overlay --brief brief.md --mode coding --seconds 0
```

**Over a call**, system audio needs LaunchServices or macOS silently feeds jay
an unbroken stream of zeros — it does not error, it just hears nothing:

```sh
scripts/bundle.sh release
open -a "$PWD/target/release/jay.app" --args \
  transcribe --overlay --source both --brief brief.md --mode coding --seconds 0
```

`--seconds 0` runs until you close the panel.

## During

Press **ask jay** when you want the answer. That sends the pinned problem, the
recent conversation and a screenshot of the focused window — so if the question
is about the code you are looking at, look at it before you press.

Add `--assist` at launch if you also want jay to volunteer suggestions when it
hears a question. It is off by default: listening is free, suggesting is not.

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

**Suggestions never fire on their own.** You did not pass `--assist`. Use the
button, which always works.

**"still thinking about the last one".** A suggestion takes up to twenty
seconds and only one runs at a time. This is deliberate: the alternative is the
transcriber stalling behind it and losing audio.

**Words are wrong in the transcript.** `--model small` is the default; `--model
base` is faster and worse on jargon. There is no larger option wired up yet.
