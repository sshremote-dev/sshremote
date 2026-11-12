#!/bin/sh
# A private headless GNOME session for capture and window tests that must leave the desktop of
# whoever is signed in alone: its own runtime folder, session bus, settings, PipeWire and a
# virtual monitor, all under $GNOME_TEST_DIR (default /tmp/sshremote-gnome).
#   scripts/gnome-session.sh up [WxH]     start it, with the sshremote helper extension enabled and
#                                         a password store of its own
#   scripts/gnome-session.sh run CMD...   run a program in it
#   scripts/gnome-session.sh env          the variables that point a program at it
#   scripts/gnome-session.sh shot FILE    a PNG of its screen, through `sshremote mcp`
#                                         ($SSHREMOTE names the program, by default sshremote)
#   scripts/gnome-session.sh down         stop everything in it
# The ignored GNOME tests in src/session.rs take `env`'s output as SSHREMOTE_GNOME_ENV.
set -eu
DIR=${GNOME_TEST_DIR:-/tmp/sshremote-gnome}
HERE=$(cd "$(dirname "$0")" && pwd)
HELPER=sshremote-helper@thinkbigx.github.io

vars() {
  # DISPLAY and XAUTHORITY are set so that sshremote never borrows the real session's X display.
  echo "XDG_RUNTIME_DIR=$DIR/run XDG_CONFIG_HOME=$DIR/config XDG_CACHE_HOME=$DIR/cache" \
    "XDG_STATE_HOME=$DIR/state XDG_DATA_HOME=$DIR/data XDG_SESSION_TYPE=wayland" \
    "WAYLAND_DISPLAY=gnome-test DBUS_SESSION_BUS_ADDRESS=$(cat "$DIR/bus") DISPLAY=:99 XAUTHORITY=/dev/null"
}

run() {
  # shellcheck disable=SC2046
  exec env -i HOME="$HOME" USER="$(id -un)" PATH="$PATH" LANG="${LANG:-C.UTF-8}" SHELL="${SHELL:-/bin/sh}" \
    $(vars) "$@"
}

down() {
  # Everything whose runtime folder is this one: the bus and the shell's helpers outlive the shell.
  for p in $(pgrep -u "$(id -u)"); do
    [ "$p" = $$ ] && continue
    if tr '\0' '\n' 2>/dev/null < "/proc/$p/environ" | grep -qx "XDG_RUNTIME_DIR=$DIR/run"; then
      kill "$p" 2>/dev/null || true
    fi
  done
  rm -f "$DIR/bus"
}

case "${1:-}" in
up)
  size=${2:-1280x800}
  down
  sleep 1
  # Only what this script makes there, whatever $GNOME_TEST_DIR names.
  rm -rf "$DIR/run" "$DIR/config" "$DIR/cache" "$DIR/state" "$DIR/data" "$DIR/inner.sh" "$DIR"/*.log
  mkdir -p "$DIR/run" "$DIR/config" "$DIR/cache" "$DIR/state" "$DIR/data/gnome-shell/extensions"
  chmod 700 "$DIR/run"
  cp -r "$HERE/../assets/gnome-helper" "$DIR/data/gnome-shell/extensions/$HELPER"
  cat > "$DIR/inner.sh" <<EOF
echo "\$DBUS_SESSION_BUS_ADDRESS" > "$DIR/bus.new"
pipewire > "$DIR/pipewire.log" 2>&1 &
sleep 0.5
wireplumber > "$DIR/wireplumber.log" 2>&1 &
gsettings set org.gnome.shell enabled-extensions "['$HELPER']"
# A password store of its own (unlocked with a throwaway password), for tests that save passwords.
printf test | gnome-keyring-daemon --unlock --components=secrets --foreground > "$DIR/keyring.log" 2>&1 &
gnome-shell --headless --virtual-monitor $size --wayland-display=gnome-test > "$DIR/shell.log" 2>&1 &
mv "$DIR/bus.new" "$DIR/bus"
wait
EOF
  env -i HOME="$HOME" USER="$(id -un)" PATH="$PATH" LANG="${LANG:-C.UTF-8}" XDG_RUNTIME_DIR="$DIR/run" \
    XDG_CONFIG_HOME="$DIR/config" XDG_CACHE_HOME="$DIR/cache" XDG_STATE_HOME="$DIR/state" \
    XDG_DATA_HOME="$DIR/data" XDG_SESSION_TYPE=wayland \
    setsid dbus-run-session -- sh "$DIR/inner.sh" > "$DIR/session.log" 2>&1 < /dev/null &
  for _ in $(seq 1 60); do
    if [ -s "$DIR/bus" ] && gdbus call --address "$(cat "$DIR/bus")" -d org.gnome.Mutter.DisplayConfig \
      -o /org/gnome/Mutter/DisplayConfig -m org.freedesktop.DBus.Peer.Ping > /dev/null 2>&1; then
      # GNOME starts in its overview; windows are tested on the desktop.
      sleep 1
      gdbus call --address "$(cat "$DIR/bus")" -d org.gnome.Shell -o /org/gnome/Shell \
        -m org.freedesktop.DBus.Properties.Set org.gnome.Shell OverviewActive "<false>" > /dev/null 2>&1 || true
      echo "up: $DIR ($size)"
      exit 0
    fi
    sleep 0.5
  done
  echo "GNOME Shell did not start; see $DIR/shell.log" >&2
  exit 1
  ;;
run)
  shift
  run "$@"
  ;;
env)
  vars
  ;;
shot)
  out=$2
  # One screenshot through the MCP server's own tool: initialize, then call it.
  run sh -c '
    printf "%s\n" "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"protocolVersion\":\"2025-03-26\",\"capabilities\":{},\"clientInfo\":{\"name\":\"gnome-session\",\"version\":\"1\"}}}" \
      "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}" \
      "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"screenshot\",\"arguments\":{\"max_width\":0}}}" |
      timeout 60 "$1" mcp --local --capture gnome' sh "${SSHREMOTE:-sshremote}" |
    python3 -c '
import base64, json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("id") == 2:
        for c in m.get("result", {}).get("content", []):
            if c.get("type") == "image":
                open(sys.argv[1], "wb").write(base64.b64decode(c["data"]))
                print("saved", sys.argv[1])
                sys.exit(0)
        sys.exit("no picture: " + line)
sys.exit("no answer from sshremote mcp")' "$out"
  ;;
down)
  down
  echo down
  ;;
*)
  sed -n '2,11p' "$0" | sed 's/^# \{0,1\}//'
  exit 2
  ;;
esac
