#!/bin/sh
# Builds sshremote.dev (page, downloads, signed registry) and uploads it to the web server.
#   scripts/deploy.sh [SSH-HOST] [WEB-ROOT]
# SSH-HOST "local" writes into WEB-ROOT on this computer.
# The registry comes from the extensions repository beside this one ($EXTENSIONS, by default
# ../sshremote-extensions); its build writes the packages and their index.
# Downloads are taken from $WINDOWS_BIN and $LINUX_BIN (SSH Remote) and $TERMINAL_WINDOWS_BIN and
# $TERMINAL_LINUX_BIN (SSH Remote Terminal) when set.
# The native packs' programs come from $HELPERS (see helper_pack.py there).
# The index is signed with $SIGNING_KEY here, or, with $SIGN_HOST set, on that computer with its
# key ($SIGN_KEY there, relative to its home), so the key never has to be on the web server.
set -eu
HOST=${1:-ubuntu}
ROOT=${2:-/var/www/sshremote.dev}
KEY=${SIGNING_KEY:-$HOME/.ssh/sshremote_registry_ed25519}
PUB=${SIGNING_PUB:-$KEY.pub}
HERE=$(cd "$(dirname "$0")" && pwd)
EXT=${EXTENSIONS:-$HERE/../../sshremote-extensions}
OUT=$(mktemp -d)
trap 'rm -rf -- "$OUT"' EXIT
# mktemp's folder is private; what goes up must be readable by the web server.
chmod 755 "$OUT"

set -- "$OUT"
[ -n "${WINDOWS_BIN:-}" ] && set -- "$@" --download "sshremote-windows-x86_64.exe=$WINDOWS_BIN"
[ -n "${LINUX_BIN:-}" ] && set -- "$@" --download "sshremote-linux-x86_64=$LINUX_BIN"
[ -n "${TERMINAL_WINDOWS_BIN:-}" ] && set -- "$@" --download "sshremote-terminal-windows-x86_64.exe=$TERMINAL_WINDOWS_BIN"
[ -n "${TERMINAL_LINUX_BIN:-}" ] && set -- "$@" --download "sshremote-terminal-linux-x86_64=$TERMINAL_LINUX_BIN"
python3 "$EXT/build.py" "$OUT"
python3 "$HERE/site.py" "$@"

INDEX="$OUT/registry/v1/index.json"
if [ -n "${SIGN_HOST:-}" ]; then
  SIGN_KEY=${SIGN_KEY:-.ssh/sshremote_registry_ed25519}
  ssh "$SIGN_HOST" "mkdir sshremote-sign" 2>/dev/null || true
  scp -q "$INDEX" "$SIGN_HOST:sshremote-sign/index.json"
  ssh "$SIGN_HOST" "ssh-keygen -q -Y sign -f $SIGN_KEY -n sshremote-registry sshremote-sign/index.json"
  scp -q "$SIGN_HOST:sshremote-sign/index.json.sig" "$INDEX.sig"
  # Unix shells take rm, Windows' cmd rmdir.
  ssh "$SIGN_HOST" "rm -rf sshremote-sign || rmdir /s /q sshremote-sign" >/dev/null 2>&1 || true
else
  ssh-keygen -q -Y sign -f "$KEY" -n sshremote-registry "$INDEX"
fi
# The check the program makes, before anything goes up.
printf 'registry@sshremote.dev namespaces="sshremote-registry" %s\n' "$(cut -d' ' -f1,2 "$PUB")" > "$OUT/.signers"
ssh-keygen -Y verify -f "$OUT/.signers" -I registry@sshremote.dev -n sshremote-registry -s "$INDEX.sig" < "$INDEX"
rm -f "$OUT/.signers"

# Packages and downloads first, the signed index last, so the index never names a missing file.
# Folders already there keep their owner and permissions: the web server goes on reading them.
put() {
  if [ "$HOST" = local ]; then tar -x -m --no-overwrite-dir -C "$ROOT"; else ssh "$HOST" "tar -x -m --no-overwrite-dir -C '$ROOT'"; fi
}
tar -C "$OUT" -c --exclude registry/v1/index.json --exclude registry/v1/index.json.sig . | put
tar -C "$OUT" -c registry/v1/index.json registry/v1/index.json.sig | put
echo "deployed to $HOST:$ROOT"
