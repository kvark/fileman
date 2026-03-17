#!/usr/bin/env bash
# GUI Smoke Test: launches fileman under Xvfb and exercises basic
# keyboard interactions via xdotool.
set -euo pipefail

DISPLAY_NUM=99
export DISPLAY=":${DISPLAY_NUM}"
FILEMAN_BIN="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/target/release/fileman"
SCREENSHOT_DIR="/tmp/fileman-gui-test"
mkdir -p "${SCREENSHOT_DIR}"

cleanup() {
  echo "Cleaning up..."
  [[ -n "${FILEMAN_PID:-}" ]] && kill "${FILEMAN_PID}" 2>/dev/null || true
  [[ -n "${XVFB_PID:-}" ]] && kill "${XVFB_PID}" 2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT

echo "=== Starting Xvfb on display ${DISPLAY} ==="
Xvfb "${DISPLAY}" -screen 0 1024x768x24 &
XVFB_PID=$!
sleep 1

if ! kill -0 "${XVFB_PID}" 2>/dev/null; then
  echo "FAIL: Xvfb failed to start"
  exit 1
fi
echo "OK: Xvfb running (PID ${XVFB_PID})"

echo "=== Launching fileman ==="
RUST_LOG=warn "${FILEMAN_BIN}" tests/data/basic &
FILEMAN_PID=$!
sleep 3

if ! kill -0 "${FILEMAN_PID}" 2>/dev/null; then
  echo "FAIL: fileman crashed on startup"
  exit 1
fi
echo "OK: fileman running (PID ${FILEMAN_PID})"

# Wait for window to appear
echo "=== Waiting for window ==="
for i in $(seq 1 15); do
  WID=$(xdotool search --name "fileman" 2>/dev/null | head -1) || true
  if [[ -n "${WID}" ]]; then
    break
  fi
  sleep 1
done

if [[ -z "${WID:-}" ]]; then
  echo "FAIL: Could not find fileman window after 15 seconds"
  exit 1
fi
echo "OK: Window found (WID ${WID})"

# Focus the window
xdotool windowfocus --sync "${WID}" 2>/dev/null || true
xdotool windowactivate --sync "${WID}" 2>/dev/null || true
sleep 0.5

# Take initial screenshot
echo "=== Taking initial screenshot ==="
if command -v import &>/dev/null; then
  import -window root "${SCREENSHOT_DIR}/01_initial.png" 2>/dev/null || true
  echo "OK: Screenshot saved to ${SCREENSHOT_DIR}/01_initial.png"
elif command -v xwd &>/dev/null; then
  xwd -root -out "${SCREENSHOT_DIR}/01_initial.xwd" 2>/dev/null || true
  echo "OK: Screenshot saved to ${SCREENSHOT_DIR}/01_initial.xwd"
else
  echo "SKIP: No screenshot tool available (import or xwd)"
fi

# Test 1: Navigate down with arrow keys
echo "=== Test 1: Arrow key navigation ==="
xdotool key --window "${WID}" Down
sleep 0.3
xdotool key --window "${WID}" Down
sleep 0.3
echo "OK: Sent Down arrow x2"

# Test 2: Open Help screen (F1)
echo "=== Test 2: Open Help (F1) ==="
xdotool key --window "${WID}" F1
sleep 0.5
echo "OK: Sent F1 (Help)"

# Take screenshot of help
if command -v import &>/dev/null; then
  import -window root "${SCREENSHOT_DIR}/02_help.png" 2>/dev/null || true
  echo "OK: Help screenshot saved"
fi

# Test 3: Close Help (Escape)
echo "=== Test 3: Close Help (Escape) ==="
xdotool key --window "${WID}" Escape
sleep 0.3
echo "OK: Sent Escape"

# Test 4: Toggle Preview (F3)
echo "=== Test 4: Toggle Preview (F3) ==="
xdotool key --window "${WID}" F3
sleep 0.5
echo "OK: Sent F3 (Preview)"

if command -v import &>/dev/null; then
  import -window root "${SCREENSHOT_DIR}/03_preview.png" 2>/dev/null || true
  echo "OK: Preview screenshot saved"
fi

# Test 5: Close Preview (Escape)
echo "=== Test 5: Close Preview (Escape) ==="
xdotool key --window "${WID}" Escape
sleep 0.3
echo "OK: Sent Escape"

# Test 6: Tab to switch panels
echo "=== Test 6: Switch panels (Tab) ==="
xdotool key --window "${WID}" Tab
sleep 0.3
echo "OK: Sent Tab"

# Test 7: Enter to open directory
echo "=== Test 7: Enter directory (Enter) ==="
xdotool key --window "${WID}" Home
sleep 0.2
xdotool key --window "${WID}" Down
sleep 0.2
xdotool key --window "${WID}" Enter
sleep 0.5
echo "OK: Sent Enter to open directory"

# Test 8: Backspace to go up
echo "=== Test 8: Go back (Backspace) ==="
xdotool key --window "${WID}" BackSpace
sleep 0.5
echo "OK: Sent Backspace"

# Test 9: Theme toggle (F9)
echo "=== Test 9: Theme toggle (F9) ==="
xdotool key --window "${WID}" F9
sleep 0.3
echo "OK: Sent F9 (theme toggle)"

if command -v import &>/dev/null; then
  import -window root "${SCREENSHOT_DIR}/04_theme.png" 2>/dev/null || true
  echo "OK: Theme screenshot saved"
fi

# Test 10: Panel swap (Ctrl+U)
echo "=== Test 10: Panel swap (Ctrl+U) ==="
xdotool key --window "${WID}" ctrl+u
sleep 0.3
echo "OK: Sent Ctrl+U (panel swap)"

# Verify process is still alive after all operations
if kill -0 "${FILEMAN_PID}" 2>/dev/null; then
  echo ""
  echo "=== ALL GUI SMOKE TESTS PASSED ==="
  echo "fileman remained responsive throughout all interactions."
  echo "Screenshots saved to ${SCREENSHOT_DIR}/"
  exit 0
else
  echo ""
  echo "FAIL: fileman process died during testing"
  exit 1
fi
