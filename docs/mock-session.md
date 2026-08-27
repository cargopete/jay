# Running a mock loop

Two rounds, back to back, with a partner playing interviewer over a call.
Everything here has been run against the real prompt path.

## Before they join

**Headphones on.** Not optional, and it is the single most important line in
this document. Their voice leaving your speakers and returning through your
microphone is captured on both channels, and it fails in two ways.

When the microphone copy transcribes cleanly, every question lands twice with
one copy blamed on you. jay now catches that one and drops the microphone copy:

```
[00:11] them: Given a two-dimensional grid of ones and zeros, find the largest island by area.
[00:11] you:  Given a two-dimensional grid of ones and zeros, find the largest island by area.
```

When it does not transcribe cleanly — which is the commoner case, since the
sound has crossed a desk — you get fluent invented English attributed to you,
and nothing catches it, because there is no duplicate to match against. Thirty
seconds of a played question produced four:

```
[00:19] you: I didn't drink no coals.
[00:22] you: E mi da?
[00:29] you: Amortised and...
[00:33] you: The moral of the French logic is this,
```

Twelve minutes of the same room with the speakers silent produced nothing at
all. That is the whole argument for headphones in two measurements.

Then, with something playing so the tap has audio to capture:

```sh
open -n -a ~/Projects/jay/target/release/jay.app --args check --out /tmp/jay-check.txt
cat /tmp/jay-check.txt
```

The `mic` peak wants to be above 0.02 while you speak — a room at rest reads
about 0.003, and exact zeros are a refused permission rather than a quiet room.
The `system` line wants a few hundred frames; zero frames there means nothing
was playing, not that the tap is broken.

## The session

One command for the whole loop. The switch bank moves you between rounds, so
there is no reason to quit and relaunch in the middle:

```sh
open -n -a ~/Projects/jay/target/release/jay.app --args \
  transcribe --overlay --source both --mode coding --seconds 0 \
  --brief ~/Library/Application\ Support/jay/brief.md \
  --vocab "SiloBin, pastebin, base62, Redpanda, idempotent, jemalloc"
```

`-n` matters. Without it, a second launch while any jay is running is silently
discarded: it activates the existing instance, ignores every argument, and
exits 0.

`--vocab` matters more than it looks. Neither `medium` nor `turbo` gets
*pastebin* or *jemalloc* unaided, and no model size rescues a product name.
Add whatever nouns this round turns on.

**Between the rounds**, throw `ROUND` from `CODE` to `DESIGN` on the panel.
That starts a fresh Claude process, so the next press pays the 4.7 second
startup again — which is the price of not quitting mid-interview.

**When the questions about your answer start**, throw `ROUND` to `Q&A`, and
**throw it back before the next round begins**. `Q&A` gives plain prose and is
forbidden from drawing a diagram or stating capacity numbers, which is exactly
right for "walk me through the view path" and exactly wrong for "design me a
pastebin". A real interview was run with it selected from before the problem was
stated and never switched back, so the design round could not produce a design.
The panel now says what each round gives when you throw it; read that line.

**Set `DETAIL` to `NUDGE`** when you want the rep rather than the answer: the
approach and the complexity in under forty words, no implementation.

## What good looks like

Both rounds dry-run against a realistic transcript.

**Coding**, asked the interviewer's follow-up rather than the original problem
— which is the point, since by the time an answer arrives you have started:

> A `visited` grid, or mutate the input in place: when you count a cell, zero
> it out so it can never be reached again.

…then compiling Rust with the invariant in a comment, `O(rows × cols)` time and
stack, and three edge cases. **9.8s.**

**System design**, opening with what the candidate had missed:

> **Missing:** you jumped to storage without saying how the ID is minted, and
> blob-plus-Postgres for a 10 KB paste is two round trips where one would do.

…then 116 writes/s, 11.6k reads/s, 100 GB/day, 36 TB/year, a drawn diagram,
each component in a line, and the decisions with what each traded away — random
7-char base62 with `INSERT … ON CONFLICT DO NOTHING` and a retry, rather than a
coordination service. **14.9s.**

## Afterwards

Every session is archived to `~/Library/Application Support/jay/sessions/`
without being asked. Replay any moment through the real prompt path:

```sh
jay ask --mode rehearsal --brief ~/Library/Application\ Support/jay/brief.md \
  --context ~/Library/Application\ Support/jay/sessions/<session>.md \
  "Find the maximum area of an island in a grid."
```

`rehearsal` is the debrief mode: what your attempt missed, quoted back, then
the full worked answer. It is the only mode with no length cap, because it runs
after the interview where being thorough costs nobody the thread.
