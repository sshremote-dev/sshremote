#!/bin/bash
# Release builds of the commit checked out here, for Linux and Windows (MinGW), into OUT:
#   scripts/release.sh OUT
# OUT gets sshremote-linux-x86_64 and sshremote-windows-x86_64.exe, the program with every part
# in, which scripts/deploy.sh takes as $LINUX_BIN and $WINDOWS_BIN. They build in target-full,
# so the next release only builds what changed.
set -eu
OUT=$(mkdir -p "$1" && cd "$1" && pwd)
cd "$(dirname "$0")/.."

build() { # PLATFORM
  local args=(--profile dist) ext="" dir=target-full/dist
  if [ "$1" = windows-x86_64 ]; then
    args+=(--target x86_64-pc-windows-gnu); ext=".exe"; dir=target-full/x86_64-pc-windows-gnu/dist
  fi
  if ! CARGO_TARGET_DIR=target-full cargo build "${args[@]}" > "$OUT/sshremote-$1.log" 2>&1; then
    echo "FAILED $1: see $OUT/sshremote-$1.log"
    return 1
  fi
  cp "$dir/sshremote$ext" "$OUT/sshremote-$1$ext"
  echo "built $1"
}

# One after the other: both use the one target folder.
build linux-x86_64
build windows-x86_64
echo "all built in $OUT"
