#!/usr/bin/env bash
# -----------------------------------------------------------------------
# Local (no container) E2E visual test for iznik-app.
# Uses cage headless on the host machine directly.
# Invoked by run.sh --local, or directly.
#
# Prerequisites: cage, grim, wtype, python3 + Pillow.
# -----------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
GOLDEN_DIR="$SCRIPT_DIR/goldens"
CAPTURE_DIR=""
CAGE_PID=""
XDG_DIR=""
WAYLAND_SOCK=""
UPDATE_GOLDENS=false

STARTUP_SETTLE=4
KEYSTROKE_SETTLE=1
DIFF_THRESHOLD=500

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[0;33m'
BOLD='\033[1m'; RESET='\033[0m'
pass()  { echo -e "  ${GREEN}PASS${RESET}  $1"; }
fail()  { echo -e "  ${RED}FAIL${RESET}  $1"; FAILURES=$((FAILURES + 1)); }
info()  { echo -e "  ${YELLOW}INFO${RESET}  $1"; }
header(){ echo -e "\n${BOLD}$1${RESET}"; }
FAILURES=0; TOTAL=0

cleanup() {
    [ -n "$CAGE_PID" ] && kill -0 "$CAGE_PID" 2>/dev/null && kill "$CAGE_PID" 2>/dev/null && wait "$CAGE_PID" 2>/dev/null || true
    if [ -n "$CAPTURE_DIR" ] && [ -d "$CAPTURE_DIR" ]; then
        [ "$FAILURES" -gt 0 ] && info "captures: $CAPTURE_DIR" || rm -rf "$CAPTURE_DIR"
    fi
    [ -n "$XDG_DIR" ] && [ -d "$XDG_DIR" ] && rm -rf "$XDG_DIR"
}
trap cleanup EXIT

for arg in "$@"; do
    case "$arg" in --update-goldens) UPDATE_GOLDENS=true ;; esac
done

header "Prerequisites"
for tool in cage grim wtype python3 cargo; do
    command -v "$tool" >/dev/null 2>&1 || { echo "Missing: $tool"; exit 1; }
done
python3 -c "from PIL import Image" 2>/dev/null || { echo "Missing: Pillow"; exit 1; }
info "ok"

header "Building iznik-app"
timeout 300 cargo build --package iznik-app 2>&1 | tail -3
APP=""
for dir in "$PROJECT_ROOT/target/debug" "${CARGO_TARGET_DIR:-$HOME/.cache/iznik/target}/debug" "$HOME/.cache/cargo-target/debug"; do
    [ -x "$dir/iznik-app" ] && APP="$dir/iznik-app" && break
done
[ -n "$APP" ] || { echo "Cannot find binary"; exit 1; }
info "binary: $APP"

header "Starting headless cage"
XDG_DIR="$(mktemp -d)"; CAPTURE_DIR="$(mktemp -d)"; mkdir -p "$GOLDEN_DIR"
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
  XDG_RUNTIME_DIR="$XDG_DIR" cage -- "$APP" >"$CAPTURE_DIR/stdout" 2>"$CAPTURE_DIR/stderr" &
CAGE_PID=$!
sleep "$STARTUP_SETTLE"
kill -0 "$CAGE_PID" 2>/dev/null || { fail "app exited"; cat "$CAPTURE_DIR/stderr"; exit 1; }
for f in "$XDG_DIR"/wayland-*; do [ -S "$f" ] && WAYLAND_SOCK="$(basename "$f")" && break; done
[ -n "$WAYLAND_SOCK" ] || { echo "No socket"; exit 1; }
info "running on $WAYLAND_SOCK"

shot() { XDG_RUNTIME_DIR="$XDG_DIR" WAYLAND_DISPLAY="$WAYLAND_SOCK" grim "$CAPTURE_DIR/$1.png" 2>/dev/null; echo "$CAPTURE_DIR/$1.png"; }
key()  {
    IFS='+' read -ra parts <<< "$1"
    local count=${#parts[@]} index=0 wtype_args=()
    for part in "${parts[@]}"; do
        index=$((index + 1))
        if [ "$index" -eq "$count" ]; then
            wtype_args+=(-k "$part")
        else
            wtype_args+=(-M "$part")
        fi
    done
    XDG_RUNTIME_DIR="$XDG_DIR" WAYLAND_DISPLAY="$WAYLAND_SOCK" wtype "${wtype_args[@]}" 2>/dev/null || true
    sleep "$KEYSTROKE_SETTLE"
}
wpct() { python3 -c "from PIL import Image; px=list(Image.open('$1').convert('RGB').getdata()); print(100*sum(1 for r,g,b in px if r>250 and g>250 and b>250)//len(px))"; }
pdiff(){ python3 -c "from PIL import Image; a=list(Image.open('$1').convert('RGB').getdata()); b=list(Image.open('$2').convert('RGB').getdata()); print(sum(1 for x,y in zip(a,b) if x!=y))"; }
ccnt() { python3 -c "from PIL import Image; print(len(set(Image.open('$1').convert('RGB').getdata())))"; }

# The compositor's seat has no keyboard capability until the first virtual
# keyboard is created; that creation races the first real keystroke and can
# drop it. One throwaway keystroke here absorbs the race before any test
# depends on keyboard delivery.
key "Escape"

# The kit's default theme is light, and with no host or session yet the
# initial screen is mostly its background color by design; a percentage of
# white pixels does not distinguish that from a broken blank render. Distinct
# colours below do: anti-aliased text and themed chrome produce many, a
# genuinely blank or solid-color surface produces almost none.
header "Test 1: Renders real content, not a blank canvas"
TOTAL=$((TOTAL+1)); S=$(shot "01_initial"); W=$(wpct "$S"); C=$(ccnt "$S")
info "white: ${W}%, colours: $C"
[ "$C" -lt 50 ] && fail "no styled UI ($C colours)" || pass "styled ($C colours)"

header "Test 2: Palette opens"
B=$(shot "03a"); key "ctrl+shift+p"; A=$(shot "03b")
TOTAL=$((TOTAL+1)); D=$(pdiff "$B" "$A")
info "changed: $D px"
[ "$D" -lt 1000 ] && fail "palette did not open ($D px)" || pass "palette opened ($D px)"

header "Test 3: Typing filters and backspace restores the palette"
key "z"; key "z"; key "z"
TYPED=$(shot "03c_typed")
TOTAL=$((TOTAL+1)); TYPED_DIFF=$(pdiff "$A" "$TYPED")
info "typed diff: $TYPED_DIFF px"
[ "$TYPED_DIFF" -lt 1000 ] && fail "typing did not visibly filter the palette ($TYPED_DIFF px)" || pass "typing filtered the palette ($TYPED_DIFF px)"
key "BackSpace"; key "BackSpace"; key "BackSpace"
DELETED=$(shot "03d_deleted")
TOTAL=$((TOTAL+1)); DELETED_DIFF=$(pdiff "$A" "$DELETED")
info "restored diff: $DELETED_DIFF px"
[ "$DELETED_DIFF" -gt 1000 ] && fail "backspace did not restore the empty query ($DELETED_DIFF px)" || pass "backspace restored the empty query ($DELETED_DIFF px)"

header "Test 4: Escape closes"
key "Escape"; E=$(shot "04")
TOTAL=$((TOTAL+1)); D=$(pdiff "$B" "$E")
info "drift: $D px"
[ "$D" -gt 1000 ] && fail "not restored" || pass "restored"

header "Test 5: Golden"
TOTAL=$((TOTAL+1))
G="$GOLDEN_DIR/initial_state.png"
if [ "$UPDATE_GOLDENS" = true ]; then cp "$S" "$G"; info "updated"
elif [ ! -f "$G" ]; then fail "no golden (--update-goldens)"
else
    GD_RAW=$(compare -metric AE "$G" "$S" null: 2>&1 || true)
    GD="${GD_RAW%% *}"
    [ "${GD:-99999}" -le "$DIFF_THRESHOLD" ] && pass "matches ($GD px)" || fail "differs ($GD px)"
fi

echo ""
header "Results"
[ "$FAILURES" -eq 0 ] && echo -e "  ${GREEN}${BOLD}All $TOTAL passed.${RESET}" || echo -e "  ${RED}${BOLD}$FAILURES/$TOTAL FAILED.${RESET}"
echo ""
exit "$FAILURES"
