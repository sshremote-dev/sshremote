#!/bin/sh
# ci-build.sh TARGET FILE: the program as it ships (the dist profile) built for TARGET, as FILE.
set -eu
# A Windows program ships only with its icon and manifest.
export SSHREMOTE_REQUIRE_RESOURCES=1
cargo build --profile dist --locked --target "$1"
ext=""
case "$1" in *windows*) ext=".exe" ;; esac
cp "target/$1/dist/sshremote$ext" "$2"
sha256sum "$2" 2> /dev/null || shasum -a 256 "$2"
