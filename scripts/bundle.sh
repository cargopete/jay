#!/usr/bin/env bash
# Wrap the jay binary in a minimal .app bundle.
#
# This is not vanity packaging. macOS grants system-audio capture on the basis
# of an Info.plist usage description and a code signature, and a bare CLI
# binary has neither. Without a bundle the CoreAudio tap starts, reports
# success, and then delivers an unbroken stream of zeros, which is a
# spectacularly unhelpful way to be told no.
#
# Usage: scripts/bundle.sh [debug|release]

set -euo pipefail

PROFILE="${1:-debug}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BIN="$ROOT/target/$PROFILE/jay"
APP="$ROOT/target/$PROFILE/jay.app"

if [[ ! -x "$BIN" ]]; then
    echo "no binary at $BIN. Build it first:" >&2
    echo "  cargo build${PROFILE:+ --profile $PROFILE}" >&2
    exit 1
fi

rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS"

cat > "$APP/Contents/Info.plist" <<'PLIST'
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>jay</string>
    <key>CFBundleDisplayName</key>
    <string>jay</string>
    <key>CFBundleIdentifier</key>
    <string>dev.cargopete.jay</string>
    <key>CFBundleExecutable</key>
    <string>jay</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>0.1.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>14.4</string>
    <key>NSMicrophoneUsageDescription</key>
    <string>jay transcribes what you say, on this machine, so it can help.</string>
    <key>NSAudioCaptureUsageDescription</key>
    <string>jay transcribes the audio your machine is playing, on this machine, so it can follow a call or a recording.</string>
</dict>
</plist>
PLIST

cp "$BIN" "$APP/Contents/MacOS/jay"

# Ad-hoc signature. TCC needs a stable code identity to attach a permission
# grant to; unsigned binaries get asked about again and again, or not at all.
codesign --force --sign - --identifier dev.cargopete.jay "$APP" >/dev/null 2>&1

echo "$APP"
