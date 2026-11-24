#!/bin/sh
# SSH Remote for Linux and macOS, for this user:
#   curl -fsSL https://sshremote.dev/install.sh | sh
# Takes the current version for this system and processor, and its SHA-256, from
# https://sshremote.dev/download/latest.json, downloads it from GitHub Releases and checks it.
# Linux: ~/.local/bin/sshremote, where
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
    Linux) os=linux ;;
    Darwin) os=macos ;;
    *) fail "no build for $(uname -s); on Windows, in PowerShell: irm https://sshremote.dev/install.ps1 | iex" ;;
  esac
  case "$(uname -m)" in
    x86_64 | amd64) arch=x86_64 ;;
    aarch64 | arm64) arch=aarch64 ;;
    *) arch=$(uname -m) ;;
  esac
  index=$(curl -fsSL https://sshremote.dev/download/latest.json) || fail "could not read https://sshremote.dev/download/latest.json"
  # Each file's entry on a line of its own, however the list is laid out.
  entries=$(printf '%s' "$index" | tr -d ' \n\r\t' | sed 's/},{/}\n{/g; s/"files":\[/\n/')
  version=$(printf '%s\n' "$index" | tr -d ' \n\r\t' | sed -n 's/.*"version":"\([^"]*\)".*/\1/p')
  entry=$(printf '%s\n' "$entries" | grep "\"os\":\"$os\"" | grep -E "\"arch\":\"($arch|universal)\"" | head -n 1)
  [ -n "$entry" ] || fail "no build for $os on $arch; build it from source: https://github.com/sshremote-dev/sshremote"
  field() { printf '%s\n' "$entry" | sed -n "s/.*\"$1\":\"\([^\"]*\)\".*/\1/p"; }
  name=$(field name)
  sum=$(field sha256)
  url=$(field url)
  [ -n "$version" ] && [ -n "$sum" ] && [ -n "$name" ] || fail "sshremote.dev lists $name without its version or SHA-256"
  case "$url" in https://*) ;; *) fail "$name is listed at $url, which is not https" ;; esac
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  echo "Downloading SSH Remote $version ($arch) from GitHub Releases..."
  curl -fL --progress-bar -o "$tmp/$name" "$url" || fail "the download failed"
  if command -v sha256sum > /dev/null; then
    got=$(sha256sum "$tmp/$name" | cut -d ' ' -f 1)
  else
    got=$(shasum -a 256 "$tmp/$name" | cut -d ' ' -f 1)
  fi
  [ "$got" = "$sum" ] || fail "the download's SHA-256 ($got) is not the one sshremote.dev lists; nothing was changed"
  if [ "$os" = macos ]; then
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
