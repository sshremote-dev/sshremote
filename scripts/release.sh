#!/bin/bash
# Release builds of the commit checked out here, for Linux and Windows (MinGW), into OUT:
#   scripts/release.sh OUT
# OUT gets remote-PLATFORM (SSH Remote: window and remote parts), core-PLATFORM (SSH Remote
# Terminal: the window only) and the registry's helper programs web-PLATFORM and mcp-PLATFORM,
# with .exe on Windows: what scripts/deploy.sh takes as downloads and $HELPERS. Each part has a
# target folder of its own (target-NAME), so the next release only builds what changed.
set -eu
OUT=$(mkdir -p "$1" && cd "$1" && pwd)
cd "$(dirname "$0")/.."

build() { # NAME TARGET FEATURES
  local name=$1 target=$2 features=$3 ext="" plat=linux-x86_64 dir
  local args=(--profile dist --no-default-features --features "$features")
  if [ "$target" = windows ]; then
    args+=(--target x86_64-pc-windows-gnu); ext=".exe"; plat=windows-x86_64
    dir=target-$name/x86_64-pc-windows-gnu/dist
  else
    dir=target-$name/dist
  fi
  CARGO_TARGET_DIR=target-$name cargo build "${args[@]}" > "$OUT/$name-$plat.log" 2>&1
  cp "$dir/sshremote$ext" "$OUT/$name-$plat$ext"
  echo "built $name $plat"
}

# Two at a time; any that fails fails the whole run, with its log named.
fail=0
pair() {
  build "$1" "$t" "$2" & local a=$!
  build "$3" "$t" "$4" & local b=$!
  wait $a || { echo "FAILED $1 $t: see $OUT"; fail=1; }
  wait $b || { echo "FAILED $3 $t: see $OUT"; fail=1; }
}
for t in linux windows; do
  pair remote gui,remote core gui
  pair web web mcp mcp
done
[ $fail = 0 ] || exit 1
echo "all built in $OUT"
