#!/usr/bin/env bash
#
# Run the esctest VT-conformance suite against the native Sampa binary and print the
# pass/fail summary. Sampa implements DECRQCRA (rectangular-area checksum), which is
# how esctest reads the screen back to verify rendered contents.
#
# Unlike the origin (Tauri/webview) build, this is a native winit+wgpu binary; under
# Xvfb it uses the X11 backend, so xdotool window cleanup works. esctest itself talks
# to the terminal purely over the PTY (stdout → render, stdin ← our DECRQCRA replies),
# launched via `sampa -e python3 esctest.py`.
#
# Requirements: python3, git, xdotool, Xvfb. esctest2 is fetched (pinned) on first run.
#
# Usage: tools/conformance/run-esctest.sh [--bin PATH] [--include REGEX] [--display :N]

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
ESCTEST_TOP="$HERE/.esctest"
ESCTEST_DIR="$ESCTEST_TOP/esctest"          # contains esctest.py
ESCTEST_REPO="https://github.com/ThomasDickey/esctest2.git"
ESCTEST_COMMIT="664be3cf2c1e3f06bc93a8bafb48a0db83c607db"   # pinned

BIN="$ROOT/target/release/sampa"
INCLUDE=".*"
DISPLAY_ARG=""

while [ $# -gt 0 ]; do
  case "$1" in
    --bin)     BIN="$2"; shift 2 ;;
    --include) INCLUDE="$2"; shift 2 ;;
    --display) DISPLAY_ARG="$2"; shift 2 ;;
    *) echo "unknown arg: $1" >&2; exit 2 ;;
  esac
done

for t in python3 git xdotool Xvfb; do
  command -v "$t" >/dev/null || { echo "$t required" >&2; exit 1; }
done
[ -x "$BIN" ] || { echo "Sampa binary not found/executable: $BIN (build with: cargo build --release)" >&2; exit 1; }

# Fetch the pinned esctest2 on first use.
if [ ! -d "$ESCTEST_DIR" ]; then
  echo "Fetching esctest2 (pinned $ESCTEST_COMMIT) ..."
  git clone --quiet "$ESCTEST_REPO" "$ESCTEST_TOP" || exit 1
  git -C "$ESCTEST_TOP" checkout --quiet "$ESCTEST_COMMIT" || exit 1
fi

# A native window needs a display; use the caller's, else spin up a private Xvfb.
OWN_XVFB=""
if [ -n "$DISPLAY_ARG" ]; then
  export DISPLAY="$DISPLAY_ARG"
else
  export DISPLAY=":99"
  if ! xdpyinfo -display :99 >/dev/null 2>&1; then
    Xvfb :99 -screen 0 1400x900x24 >/dev/null 2>&1 &
    OWN_XVFB=$!
    sleep 1
  fi
fi
# Force winit onto X11 (so xdotool can see/clean the window) and off Wayland.
unset WAYLAND_DISPLAY
export WINIT_UNIX_BACKEND=x11

LOG="$ESCTEST_TOP/esctest-$(printf '%s' "$INCLUDE" | tr -c 'A-Za-z0-9' _).log"
rm -f "$LOG"

for w in $(xdotool search --name '^esctestrun$' 2>/dev/null); do xdotool windowkill "$w" 2>/dev/null; done

# --window-id 0 makes esctest skip xwininfo; --xterm-checksum 334 selects the raw
# (non-negated) checksum convention Sampa replies with, empty cells compared as space.
nohup "$BIN" --title "esctestrun" --working-directory "$ESCTEST_DIR" \
  -e python3 esctest.py \
     --expected-terminal xterm --xterm-checksum 334 --max-vt-level 4 \
     --window-id 0 --timeout 1 --no-print-logs --logfile "$LOG" \
     --include "$INCLUDE" \
  > "$ESCTEST_TOP/last-run.out" 2>&1 &

echo "Running esctest (include='$INCLUDE') on DISPLAY=$DISPLAY ..."
for _ in $(seq 1 900); do
  sleep 1
  grep -q "passed," "$LOG" 2>/dev/null && break
done

for w in $(xdotool search --name '^esctestrun$' 2>/dev/null); do xdotool windowkill "$w" 2>/dev/null; done
[ -n "$OWN_XVFB" ] && kill "$OWN_XVFB" 2>/dev/null

SUMMARY="$(grep -E "\*\*\*.*passed" "$LOG" 2>/dev/null | tail -1)"
if [ -z "$SUMMARY" ]; then
  echo "No summary — esctest did not finish. See $LOG and $ESCTEST_TOP/last-run.out" >&2
  exit 1
fi
echo
echo "==== esctest summary (include='$INCLUDE') ===="
echo "$SUMMARY"
echo
echo "Failing tests grouped by feature:"
awk '/Failing tests:/{f=1;next} f && /Tests\./' "$LOG" | sed 's/\..*//' | sort | uniq -c | sort -rn | head -30
echo
echo "Full log: $LOG"
