#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
[ "$(uname -s)" = Darwin ] || { echo 'macOS required' >&2; exit 1; }
mkdir -p target
clang -fobjc-arc -Wall -Wextra -Werror -Wno-unused-parameter \
  -framework AppKit -framework ApplicationServices -framework Carbon \
  crates/core/native/hotkey-permissions-test.m -o target/hotkey-permissions-test
./target/hotkey-permissions-test
clang -fobjc-arc -Wall -Wextra -Werror -Wno-unused-parameter \
  -framework AppKit -framework ApplicationServices \
  crates/core/native/insertion-target-test.m -o target/insertion-target-test
./target/insertion-target-test
