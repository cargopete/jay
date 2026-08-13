#!/usr/bin/env bash
#
# Read an interviewer script aloud, so jay can be tested without a second person.
#
# Everything `say` produces goes to the default output device, which is exactly
# where a call's audio goes, so the system tap hears it as `them:` and the whole
# pipeline is exercised for real — capture, VAD, attribution, problem pinning.
# The only part this cannot stand in for is a human being surprising you.
#
# Wear headphones or the microphone will hear it too and blame you for it.
#
# Usage:
#   scripts/dictate.sh <script.md> [--pause SECONDS] [--rate WPM] [--from N]
#
# Reads the blockquote lines (`> "..."`) of a markdown interviewer script, which
# is the convention the scripts in ~/Projects/interview-prep already follow, and
# speaks each paragraph with a pause between. Everything outside a blockquote is
# stage direction for the human and is skipped.

set -euo pipefail

SCRIPT=${1:-}
PAUSE=6
RATE=170
FROM=1

if [[ -z $SCRIPT || ! -f $SCRIPT ]]; then
    echo "usage: $0 <script.md> [--pause SECONDS] [--rate WPM] [--from N]" >&2
    exit 2
fi
shift

while [[ $# -gt 0 ]]; do
    case $1 in
        --pause) PAUSE=$2; shift 2 ;;
        --rate)  RATE=$2;  shift 2 ;;
        --from)  FROM=$2;  shift 2 ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
done

# Blockquote content, with the marker and markdown emphasis stripped. Blank
# quoted lines separate paragraphs, and a paragraph is one spoken turn.
#
# One paragraph per output line, assembled in awk. An earlier version used a
# sentinel character and `tr` to split on it, which on BSD `tr` translated the
# letters x, 1 and e instead and turned 12 paragraphs into 429 fragments.
paragraphs=$(
    awk '
        /^> / {
            line = substr($0, 3)
            buf = (buf == "" ? line : buf " " line)
            next
        }
        { if (buf != "") { print buf; buf = "" } }
        END { if (buf != "") print buf }
    ' "$SCRIPT" |
    sed -e 's/\*\*//g' -e 's/\*//g' -e 's/`//g' -e 's/"//g' |
    grep -v '^[[:space:]]*$'
)

total=$(printf '%s\n' "$paragraphs" | wc -l | tr -d ' ')
echo "$total paragraphs, ${PAUSE}s between, ${RATE} wpm. Ctrl-C to stop."
echo

n=0
while IFS= read -r para; do
    n=$((n + 1))
    [[ $n -lt $FROM ]] && continue
    printf '[%2d/%s] %.72s...\n' "$n" "$total" "$para"
    say -r "$RATE" "$para"
    sleep "$PAUSE"
done <<< "$paragraphs"

echo
echo "done."
