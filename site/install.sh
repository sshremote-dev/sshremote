#!/bin/sh
# SSH Remote for Linux and macOS, for this user:
#   curl -fsSL https://sshremote.dev/install.sh | sh
# Takes the current version and its SHA-256 from https://sshremote.dev/download/index.json,
# downloads that version from GitHub Releases and checks it. Linux: ~/.local/bin/sshremote, where
# the viewer also looks for it on a host over SSH. macOS: SSH Remote.app in /Applications (in
# ~/Applications where /Applications cannot be written). $SSHREMOTE_INSTALL_DIR puts it there
# instead.
# To remove it: `sshremote remote-access uninstall` if the service was installed, then delete the
# program, and ~/.config/sshremote for its settings.
set -eu

fail() {
  echo "sshremote install: $*" >&2
  exit 1
}

main() {
  case "$(uname -s)" in
    Linux)
      [ "$(uname -m)" = x86_64 ] || fail "the Linux build is for x86_64 (this is $(uname -m)); build it from source: https://github.com/sshremote-dev/sshremote"
      name=sshremote-linux-x86_64
      ;;
    Darwin) name=sshremote-macos.zip ;;
    *) fail "no build for $(uname -s); on Windows, in PowerShell: irm https://sshremote.dev/install.ps1 | iex" ;;
  esac
  index=$(curl -fsSL https://sshremote.dev/download/index.json) || fail "could not read https://sshremote.dev/download/index.json"
  # One "key":value to a line, however the list is laid out.
  fields=$(printf '%s' "$index" | tr -d ' \n\r\t' | tr '{},[]' '\n\n\n\n\n')
  version=$(printf '%s\n' "$fields" | sed -n 's/^"version":"\([^"]*\)"$/\1/p' | head -n 1)
  sum=$(printf '%s\n' "$fields" | awk -v want="\"name\":\"$name\"" '$0 == want { found = 1 } found && /^"sha256":/ { gsub(/"/, ""); sub(/^sha256:/, ""); print; exit }')
  [ -n "$version" ] && [ -n "$sum" ] || fail "sshremote.dev lists no $name"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  echo "Downloading SSH Remote $version from GitHub Releases..."
  curl -fL --progress-bar -o "$tmp/$name" "https://github.com/sshremote-dev/sshremote/releases/download/v$version/$name" || fail "the download failed"
  if command -v sha256sum > /dev/null; then
    got=$(sha256sum "$tmp/$name" | cut -d ' ' -f 1)
  else
    got=$(shasum -a 256 "$tmp/$name" | cut -d ' ' -f 1)
  fi
  [ "$got" = "$sum" ] || fail "the download's SHA-256 ($got) is not the one sshremote.dev lists; nothing was changed"
  if [ "$name" = sshremote-macos.zip ]; then
    apps=${SSHREMOTE_INSTALL_DIR:-/Applications}
    if [ ! -w "$apps" ]; then
      apps="$HOME/Applications"
      mkdir -p "$apps"
    fi
    ditto -x -k "$tmp/$name" "$tmp/app"
    rm -rf "$apps/SSH Remote.app"
    mv "$tmp/app/SSH Remote.app" "$apps/"
    echo "SSH Remote $version is in $apps/SSH Remote.app"
    echo "Its program, for SSH and the command line: $apps/SSH Remote.app/Contents/MacOS/sshremote"
  else
    bin=${SSHREMOTE_INSTALL_DIR:-$HOME/.local/bin}
    mkdir -p "$bin"
    # Beside it first, then renamed over it: a copy that runs keeps running.
    cp "$tmp/$name" "$bin/.sshremote.new"
    chmod 755 "$bin/.sshremote.new"
    mv -f "$bin/.sshremote.new" "$bin/sshremote"
    echo "SSH Remote $version is in $bin/sshremote"
    case ":$PATH:" in
      *":$bin:"*) ;;
      *) echo "Add $bin to PATH to run it as sshremote" ;;
    esac
  fi
}

main "$@"
