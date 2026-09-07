#!/bin/sh
# Sign an immutable staged bundle before replacing the installed app. Do not
# sign a live executable: its running code and TCC's disk identity can differ.
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo 'macOS required' >&2; exit 1; }
bundle=${1:-target/release/bundle/macos/Transcribe.app}
destination=${2:-/Applications/Transcribe.app}
[ -d "$bundle" ] || { echo 'Build the app with npm run package -- --bundles app first.' >&2; exit 1; }
case "$destination" in /*/Transcribe.app) ;; *) echo 'Use an absolute destination ending in /Transcribe.app' >&2; exit 1;; esac
if pgrep -x transcribe >/dev/null; then
  echo 'Quit Transcribe completely (not just its window), then run the installer again.' >&2
  exit 1
fi
identity=${TRANSCRIBE_SIGNING_IDENTITY:-}
if [ -z "$identity" ]; then
  identities=$(security find-identity -v -p codesigning | sed -nE 's/^ *[0-9]+\) ([A-F0-9]{40}) ".*"$/\1/p')
  count=$(printf '%s\n' "$identities" | awk 'NF {n++} END {print n+0}')
  if [ "$count" != 1 ]; then
    echo 'Set TRANSCRIBE_SIGNING_IDENTITY to a valid code-signing identity from security find-identity -v -p codesigning.' >&2
    exit 1
  fi
  identity=$identities
fi
if [ "$identity" = '-' ]; then
  echo 'A certificate-backed identity is required so privacy grants survive rebuilds.' >&2
  exit 1
fi
parent=$(dirname "$destination")
stage=$(mktemp -d "$parent/.transcribe-install.XXXXXX")
cleanup() {
  if [ -d "$stage/previous.app" ] && [ ! -e "$destination" ]; then
    mv "$stage/previous.app" "$destination"
  fi
  rm -rf "$stage"
}
trap cleanup EXIT HUP INT TERM
ditto "$bundle" "$stage/Transcribe.app"
codesign --force --sign "$identity" --timestamp=none --identifier app.transcribe.desktop "$stage/Transcribe.app"
codesign --verify --deep --strict "$stage/Transcribe.app"
if [ -e "$destination" ]; then mv "$destination" "$stage/previous.app"; fi
mv "$stage/Transcribe.app" "$destination"
codesign --verify --deep --strict "$destination"
printf 'Installed and verified %s\n' "$destination"
