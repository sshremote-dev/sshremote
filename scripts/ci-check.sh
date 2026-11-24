#!/bin/sh
# ci-check.sh FILE: the built program runs where it is meant to: it says its version and shows a
# synthetic desktop through its own agent.
set -eu
"$1" --version
out=$("$1" --local --capture synthetic --probe 3 2>&1) || true
echo "$out"
echo "$out" | grep -q "^sshremote: [1-9][0-9]* frames" || { echo "ci-check.sh: no frames from $1" >&2; exit 1; }
