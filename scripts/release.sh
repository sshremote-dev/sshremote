#!/bin/bash
# Release builds of the commit checked out here, for Linux and Windows (MinGW), into OUT:
#   scripts/release.sh OUT
# OUT gets sshremote-linux-x86_64 and sshremote-windows-x86_64.exe, the program with every part
# in, which scripts/deploy.sh takes as $LINUX_BIN and $WINDOWS_BIN. With $GITHUB_TOKEN they are
# the files GitHub Actions built, with build attestations, for the commit's tag (pushed first:
# scripts/ci_files.py waits for that run); without it they build here, in target-full, so the
# next release only builds what changed. With $MAC_HOST (a Mac reached by ssh, with
# rustup, Xcode's command line tools and CMake; $MAC_PATH adds to its PATH), the commit is built
# there too (scripts/macos.sh, in ~/sshremote-build, kept between releases): OUT gets
# sshremote-macos.zip, deploy.sh's $MAC_ZIP.
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

if [ -n "${GITHUB_TOKEN:-}" ]; then
  python3 scripts/ci_files.py "$(git rev-parse HEAD)" "$OUT"
else
  # One after the other: both use the one target folder.
  build linux-x86_64
  build windows-x86_64
fi
if [ -n "${MAC_HOST:-}" ]; then
  git archive --format=tar.gz --prefix=src/ HEAD | ssh "$MAC_HOST" 'rm -rf ~/sshremote-build/src && mkdir -p ~/sshremote-build/tree && tar xzf - -C ~/sshremote-build && rsync -a --delete --exclude target ~/sshremote-build/src/ ~/sshremote-build/tree/'
  if ssh "$MAC_HOST" "cd ~/sshremote-build/tree && PATH=\$HOME/.cargo/bin:${MAC_PATH:-}:\$PATH sh scripts/macos.sh ~/sshremote-build/out" > "$OUT/sshremote-macos.log" 2>&1; then
    scp -q "$MAC_HOST:sshremote-build/out/sshremote-macos.zip" "$OUT/sshremote-macos.zip"
    echo "built macos"
  else
    echo "FAILED macos: see $OUT/sshremote-macos.log"
    exit 1
  fi
fi
echo "all built in $OUT"
