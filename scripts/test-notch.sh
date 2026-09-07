#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
if [ "$(uname -s)" != Darwin ]; then
  echo 'The native notch tests require an unlocked Mac.' >&2
  exit 1
fi
mkdir -p target/notch-qa
clang -fobjc-arc -mmacosx-version-min=12.0 -Wall -Wextra -Werror -Wno-unused-parameter \
  -framework AppKit -framework QuartzCore -framework ApplicationServices \
  src-tauri/native/notch-test.m -o target/notch-test
./target/notch-test "$PWD/target/notch-qa"
