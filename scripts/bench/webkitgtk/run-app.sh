#!/usr/bin/env bash
# Launch the built app for the bench, on the machine's own desktop, from
# an SSH shell: a throwaway data tree (`XDG_DATA_HOME`, so nobody's
# profile is touched — Tauri derives the whole `app.waveflow` tree, logs
# included, from it), WebKit's remote inspector on a local port, and the
# renderer the run asks for. Outbound HTTP goes to a dead proxy, so no
# update check, lookup or plugin fetch runs under the measurements (the
# loopback the clips are served on is exempt). Run as a transient user
# unit so it outlives the SSH session.
#
#   bash run-app.sh <binary> <sandbox-dir> <gpu|software> [port=9222]
#   bash run-app.sh stop
set -euo pipefail
UNIT=wf-bench-app
if [ "${1:-}" = stop ]; then
  systemctl --user stop "$UNIT" 2>/dev/null || true
  exit 0
fi
BIN="${1:?binary}"
SANDBOX="${2:?sandbox dir}"
RENDERER="${3:-gpu}"
PORT="${4:-9222}"

if pgrep -x waveflow >/dev/null; then
  echo "an instance of waveflow is already running; the single-instance lock would hand the bench over to it" >&2
  exit 1
fi

# The desktop's environment, read from an ordinary client of the session
# (the compositor itself has no WAYLAND_DISPLAY in its own environ).
envfrom=""
for name in gnome-software nautilus gjs xdg-desktop-portal-gnome gsd-media-keys plasmashell; do
  pid=$(pgrep -u "$(id -u)" -x "$name" | head -1 || true)
  if [ -n "$pid" ]; then envfrom=$pid; break; fi
done
if [ -z "$envfrom" ]; then
  echo "no desktop session process found to borrow the display from" >&2
  exit 1
fi
declare -a SETENV=()
while IFS= read -r line; do
  case "$line" in
    DISPLAY=* | WAYLAND_DISPLAY=* | XAUTHORITY=* | XDG_RUNTIME_DIR=* | DBUS_SESSION_BUS_ADDRESS=* | XDG_SESSION_TYPE=* | XDG_CURRENT_DESKTOP=*)
      SETENV+=("--setenv=$line") ;;
  esac
done < <(tr '\0' '\n' < "/proc/$envfrom/environ")

mkdir -p "$SANDBOX/data"
systemctl --user stop "$UNIT" 2>/dev/null || true
# The bench stops the app with a signal, which the skin-recovery marker
# reads as a crash under a non-Studio skin: the next launch would start in
# safe mode. The sandbox is ours, so the marker goes before each launch.
rm -f "$SANDBOX/data/app.waveflow/waveflow/skin-recovery.json"
systemctl --user reset-failed "$UNIT" 2>/dev/null || true
systemd-run --user --unit="$UNIT" --collect --quiet \
  "${SETENV[@]}" \
  --setenv=XDG_DATA_HOME="$SANDBOX/data" \
  --setenv=WEBKIT_INSPECTOR_HTTP_SERVER="127.0.0.1:$PORT" \
  --setenv=WAVEFLOW_RENDERER="$RENDERER" \
  --setenv=HTTPS_PROXY=http://127.0.0.1:9 \
  --setenv=HTTP_PROXY=http://127.0.0.1:9 \
  --setenv=NO_PROXY=127.0.0.1,localhost \
  --property=StandardOutput=append:"$SANDBOX/app.log" \
  --property=StandardError=append:"$SANDBOX/app.log" \
  "$BIN"
for _ in $(seq 1 60); do
  pid=$(systemctl --user show -p MainPID --value "$UNIT")
  if [ "${pid:-0}" != 0 ] && curl -s "http://127.0.0.1:$PORT/" >/dev/null 2>&1; then
    echo "$pid"
    exit 0
  fi
  sleep 1
done
echo "the app did not open its inspector port" >&2
exit 1
