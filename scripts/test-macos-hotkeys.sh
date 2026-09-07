#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo 'macOS required' >&2; exit 1; }
mkdir -p target
clang -fobjc-arc -Wall -Wextra -Werror \
  -framework AppKit -framework ApplicationServices -framework Carbon \
  crates/core/native/hotkey-labels-test.m -o target/hotkey-labels-test
./target/hotkey-labels-test
