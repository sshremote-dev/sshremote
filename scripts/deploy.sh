#!/bin/sh
# Builds sshremote.dev (page, downloads, signed registry) and uploads it to the web server.
#   scripts/deploy.sh [SSH-HOST] [WEB-ROOT]
# SSH-HOST "local" writes into WEB-ROOT on this computer.
# The registry comes from the extensions repository beside this one ($EXTENSIONS, by default
# ../sshremote-extensions); its build writes the packages and their index.
# With $RELEASE_DIR (every file of release/platforms.json, as scripts/release.sh leaves them), the
# release's lists are written too (scripts/site.py).
# The lists are signed where the keys are, never on the web server. The release key, on $SIGN_HOST
# ($SIGN_KEY there, relative to its home: the project's Mac), signs what the program reads since
# 0.25.36: download/latest.json and registry/v1/catalog.json. The first key, on $FIRST_SIGN_HOST
# ($FIRST_SIGN_KEY), signs the lists of the versions before (download/index.json with its files,
# registry/v1/index.json); without it, or with that computer off, those stay as they are on the
# server, consistent, for those versions to go on reading. Without $SIGN_HOST, $SIGNING_KEY signs
# here (a test site).
set -eu
HOST=${1:-ubuntu}
ROOT=${2:-/var/www/sshremote.dev}
HERE=$(cd "$(dirname "$0")" && pwd)
KEYS=$HERE/../release/keys
EXT=${EXTENSIONS:-$HERE/../../sshremote-extensions}
OUT=$(mktemp -d)
trap 'rm -rf -- "$OUT"' EXIT
# mktemp's folder is private; what goes up must be readable by the web server.
chmod 755 "$OUT"

python3 "$EXT/build.py" "$OUT"
if [ -n "${RELEASE_DIR:-}" ]; then
  python3 "$HERE/site.py" "$OUT" --files "$RELEASE_DIR"
else
  python3 "$HERE/site.py" "$OUT"
fi
cp "$OUT/registry/v1/index.json" "$OUT/registry/v1/catalog.json"

sign() { # FILE NAMESPACE HOST KEY PUB: FILE.sig beside it (on HOST, or here with KEY when HOST is
  # empty), then the check the program makes with PUB.
  if [ -n "$3" ]; then
    ssh "$3" "mkdir sshremote-sign" 2> /dev/null || true
    scp -q "$1" "$3:sshremote-sign/list.json"
    ssh "$3" "ssh-keygen -q -Y sign -f $4 -n $2 sshremote-sign/list.json"
    scp -q "$3:sshremote-sign/list.json.sig" "$1.sig"
    # Unix shells take rm, Windows' cmd rmdir.
    ssh "$3" "rm -rf sshremote-sign || rmdir /s /q sshremote-sign" > /dev/null 2>&1 || true
  else
    ssh-keygen -q -Y sign -f "$4" -n "$2" "$1"
  fi
  chmod 644 "$1.sig"
  printf 'registry@sshremote.dev namespaces="%s" %s\n' "$2" "$(cut -d' ' -f1,2 "$5")" > "$OUT/.signers"
  ssh-keygen -Y verify -f "$OUT/.signers" -I registry@sshremote.dev -n "$2" -s "$1.sig" < "$1"
  rm -f "$OUT/.signers"
}
# The release key.
RELEASE_HOST=${SIGN_HOST:-}
RELEASE_KEY=${SIGN_KEY:-dev/sign/sshremote_release_ed25519}
[ -n "$RELEASE_HOST" ] || RELEASE_KEY=${SIGNING_KEY:?set SIGN_HOST, or SIGNING_KEY for a test site}
RELEASE_PUB=${SIGNING_PUB:-$KEYS/release.pub}
[ -n "$RELEASE_HOST" ] || RELEASE_PUB=${SIGNING_PUB:-$RELEASE_KEY.pub}
sign "$OUT/registry/v1/catalog.json" sshremote-registry "$RELEASE_HOST" "$RELEASE_KEY" "$RELEASE_PUB"
[ -f "$OUT/download/latest.json" ] && sign "$OUT/download/latest.json" sshremote-release "$RELEASE_HOST" "$RELEASE_KEY" "$RELEASE_PUB"

# The first key, where it can be reached.
first=no
if [ -n "${FIRST_SIGN_HOST:-}" ] && ssh -o ConnectTimeout=10 -o BatchMode=yes "$FIRST_SIGN_HOST" exit 2> /dev/null; then
  FIRST_KEY=${FIRST_SIGN_KEY:-.ssh/sshremote_registry_ed25519}
  sign "$OUT/registry/v1/index.json" sshremote-registry "$FIRST_SIGN_HOST" "$FIRST_KEY" "$KEYS/first.pub"
  [ -f "$OUT/download/index.json" ] && sign "$OUT/download/index.json" sshremote-release "$FIRST_SIGN_HOST" "$FIRST_KEY" "$KEYS/first.pub"
  first=yes
else
  echo "deploy.sh: the first key's lists are left as they are (no \$FIRST_SIGN_HOST, or it is off)"
fi

# Packages and downloads first, the signed lists last, so a list never names a missing file. What
# the first key did not sign this time stays on the server as it was, with the files it names.
# Folders already there keep their owner and permissions: the web server goes on reading them.
put() {
  if [ "$HOST" = local ]; then tar -x -m --no-overwrite-dir -C "$ROOT"; else ssh "$HOST" "tar -x -m --no-overwrite-dir -C '$ROOT'"; fi
}
LISTS="registry/v1/catalog.json registry/v1/catalog.json.sig"
[ -f "$OUT/download/latest.json" ] && LISTS="$LISTS download/latest.json download/latest.json.sig"
FIRST_LISTS="registry/v1/index.json registry/v1/index.json.sig"
FIRST_FILES=""
if [ -f "$OUT/download/index.json" ]; then
  FIRST_LISTS="$FIRST_LISTS download/index.json download/index.json.sig"
  for f in "$OUT"/download/sshremote-*; do FIRST_FILES="$FIRST_FILES download/${f##*/}"; done
fi
paths() { for f in "$@"; do printf './%s\n' "$f"; done; }
skip=""
for f in $LISTS $FIRST_LISTS $FIRST_FILES; do skip="$skip --exclude=./$f"; done
# shellcheck disable=SC2086
tar -C "$OUT" -c $skip . | put
if [ "$first" = yes ]; then
  # shellcheck disable=SC2046,SC2086
  [ -n "$FIRST_FILES" ] && tar -C "$OUT" -c $(paths $FIRST_FILES) | put
  # shellcheck disable=SC2046,SC2086
  tar -C "$OUT" -c $(paths $FIRST_LISTS) | put
fi
# shellcheck disable=SC2046,SC2086
tar -C "$OUT" -c $(paths $LISTS) | put
echo "deployed to $HOST:$ROOT (first key's lists: $first)"
