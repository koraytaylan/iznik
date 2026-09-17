#!/usr/bin/env bash
# -----------------------------------------------------------------------
# End-to-end visual test harness for iznik-app in Podman containers.
#
# Topology:
#   [app container]  ──podman network──  [host0]  [host1]
#    cage + iznik-app                     sshd      sshd
#    grim, wtype                          (iznik user, key auth)
#
# The app container runs iznik-app inside a headless Cage compositor.
# The host containers run sshd with key-only auth, exactly like the
# existing regression fixture. SSH credentials are generated fresh per
# run and injected into both sides.
#
# Prerequisites: podman with netavark backend.
# Usage:
#   ./regression/e2e/run.sh                  # run tests
#   ./regression/e2e/run.sh --update-goldens # save new golden images
#   ./regression/e2e/run.sh --local          # run locally, no containers
# -----------------------------------------------------------------------
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
IMAGES_DIR="$PROJECT_ROOT/regression/images"
GOLDEN_DIR="$SCRIPT_DIR/goldens"
CAPTURE_DIR=""
UPDATE_GOLDENS=false
LOCAL_MODE=false

# Container names carry the PID to prevent collisions.
PREFIX="iznik-e2e-$$"
NETWORK="${PREFIX}-net"
APP_CONTAINER="${PREFIX}-app"
HOST_CONTAINERS=()
HOST_COUNT=2

# Mount point inside containers (matches existing regression convention).
MOUNT_POINT="/iznik"

# Timing.
STARTUP_SETTLE=5
KEYSTROKE_SETTLE=1
HOST_READY_TIMEOUT=30
CONTAINER_TIMEOUT=120
DIFF_THRESHOLD=500

# -----------------------------------------------------------------------
# Colours
# -----------------------------------------------------------------------
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
BOLD='\033[1m'
RESET='\033[0m'

pass()  { echo -e "  ${GREEN}PASS${RESET}  $1"; }
fail()  { echo -e "  ${RED}FAIL${RESET}  $1"; FAILURES=$((FAILURES + 1)); }
info()  { echo -e "  ${YELLOW}INFO${RESET}  $1"; }
header(){ echo -e "\n${BOLD}$1${RESET}"; }

FAILURES=0
TOTAL=0

# -----------------------------------------------------------------------
# Cleanup
# -----------------------------------------------------------------------
cleanup() {
    header "Cleanup"
    # Remove containers.
    for name in "$APP_CONTAINER" "${HOST_CONTAINERS[@]}"; do
        if podman container exists "$name" 2>/dev/null; then
            podman rm --force --time 0 "$name" >/dev/null 2>&1 || true
            info "removed container $name"
        fi
    done
    # Remove network.
    if podman network exists "$NETWORK" 2>/dev/null; then
        podman network rm --force "$NETWORK" >/dev/null 2>&1 || true
        info "removed network $NETWORK"
    fi
    # Remove temp dirs.
    if [ -n "${SSH_DIR:-}" ] && [ -d "$SSH_DIR" ]; then
        rm -rf "$SSH_DIR"
    fi
    if [ -n "$CAPTURE_DIR" ] && [ -d "$CAPTURE_DIR" ]; then
        if [ "$FAILURES" -gt 0 ]; then
            info "captures preserved at: $CAPTURE_DIR"
        else
            rm -rf "$CAPTURE_DIR"
        fi
    fi
}
trap cleanup EXIT

# -----------------------------------------------------------------------
# Arguments
# -----------------------------------------------------------------------
for arg in "$@"; do
    case "$arg" in
        --update-goldens) UPDATE_GOLDENS=true ;;
        --local)          LOCAL_MODE=true ;;
        *) echo "Unknown argument: $arg"; exit 1 ;;
    esac
done

# -----------------------------------------------------------------------
# Local mode: run without containers (for development)
# -----------------------------------------------------------------------
if [ "$LOCAL_MODE" = true ]; then
    exec "$SCRIPT_DIR/run.local.sh" "$@"
fi

# -----------------------------------------------------------------------
# Prerequisites
# -----------------------------------------------------------------------
header "Checking prerequisites"
for tool in podman cargo jq; do
    command -v "$tool" >/dev/null 2>&1 || { echo "Missing: $tool"; exit 1; }
done
info "all tools present"

# -----------------------------------------------------------------------
# Build iznik-app and iznik-server binaries
# -----------------------------------------------------------------------
header "Building binaries"
timeout 300 cargo build --package iznik-app --package iznik-server 2>&1 | tail -3

# Ask cargo where it just built, rather than guessing across candidate
# target directories: a stale binary left over at an earlier-checked
# candidate (e.g. from a previous CARGO_TARGET_DIR) would otherwise be
# picked over the one this build just produced, silently testing old code.
CARGO_TARGET_DIRECTORY="$(cargo metadata --no-deps --format-version=1 | jq -r .target_directory)"
APP_BINARY="$CARGO_TARGET_DIRECTORY/debug/iznik-app"
SERVER_BINARY="$CARGO_TARGET_DIRECTORY/debug/iznik-server"
[ -x "$APP_BINARY" ] || { echo "Cannot find iznik-app binary at $APP_BINARY"; exit 1; }
[ -x "$SERVER_BINARY" ] || { echo "Cannot find iznik-server binary at $SERVER_BINARY"; exit 1; }
info "app binary: $APP_BINARY"
info "server binary: $SERVER_BINARY"

# -----------------------------------------------------------------------
# Build container images
# -----------------------------------------------------------------------
header "Building container images"
podman build --quiet --tag iznik-e2e-app  -f "$IMAGES_DIR/Containerfile.app"  "$IMAGES_DIR" 2>&1 | tail -1
podman build --quiet --tag iznik-e2e-host -f "$IMAGES_DIR/Containerfile.host" "$IMAGES_DIR" 2>&1 | tail -1
info "images built"

# -----------------------------------------------------------------------
# Generate per-run SSH credentials
# -----------------------------------------------------------------------
header "Generating SSH credentials"
SSH_DIR="$(mktemp -d)"
CAPTURE_DIR="$(mktemp -d)"
mkdir -p "$GOLDEN_DIR"

ssh-keygen -t ed25519 -f "$SSH_DIR/id_ed25519" -N "" -q
info "keypair generated"

# -----------------------------------------------------------------------
# Create the podman network
# -----------------------------------------------------------------------
header "Creating network"
podman network create "$NETWORK" >/dev/null 2>&1
info "network: $NETWORK"

# -----------------------------------------------------------------------
# Start host containers (sshd with key auth)
# -----------------------------------------------------------------------
header "Starting host containers"
for index in $(seq 0 $((HOST_COUNT - 1))); do
    alias="host${index}"
    name="${PREFIX}-${alias}"
    HOST_CONTAINERS+=("$name")

    podman run --detach --init \
        --name "$name" \
        --hostname "$alias" \
        --network "$NETWORK" \
        --timeout "$CONTAINER_TIMEOUT" \
        iznik-e2e-host >/dev/null

    # Inject the public key via podman cp (stdin piping doesn't work reliably).
    podman exec --user root "$name" sh -c \
        "mkdir -p /home/iznik/.ssh && chmod 700 /home/iznik/.ssh && chown iznik:iznik /home/iznik/.ssh"
    podman cp "$SSH_DIR/id_ed25519.pub" "$name:/home/iznik/.ssh/authorized_keys"
    podman exec --user root "$name" sh -c \
        "chmod 600 /home/iznik/.ssh/authorized_keys && chown iznik:iznik /home/iznik/.ssh/authorized_keys"

    # Copy the server binary for the app to bootstrap.
    podman cp "$SERVER_BINARY" "$name:/usr/local/bin/iznik-server"
    podman exec --user root "$name" chmod 755 /usr/local/bin/iznik-server

    info "$alias started"
done

# Wait for sshd to be ready.
header "Waiting for hosts"
for index in $(seq 0 $((HOST_COUNT - 1))); do
    alias="host${index}"
    name="${PREFIX}-${alias}"
    waited=0
    while [ "$waited" -lt "$HOST_READY_TIMEOUT" ]; do
        if podman exec "$name" pgrep -x sshd >/dev/null 2>&1; then
            info "$alias sshd ready"
            break
        fi
        sleep 1
        waited=$((waited + 1))
    done
    if [ "$waited" -ge "$HOST_READY_TIMEOUT" ]; then
        fail "$alias sshd not ready within ${HOST_READY_TIMEOUT}s"
        exit 1
    fi
done

# -----------------------------------------------------------------------
# Prepare the staged directory (binaries + test script + SSH keys)
# -----------------------------------------------------------------------
STAGED_DIR="$(mktemp -d)"
cp "$APP_BINARY" "$STAGED_DIR/iznik-app"
cp "$SERVER_BINARY" "$STAGED_DIR/iznik-server"
cp "$SSH_DIR/id_ed25519" "$STAGED_DIR/id_ed25519"
cp "$SSH_DIR/id_ed25519.pub" "$STAGED_DIR/id_ed25519.pub"
chmod 600 "$STAGED_DIR/id_ed25519"

# Write the in-container test driver.
cat > "$STAGED_DIR/drive.sh" << 'DRIVE_SCRIPT'
#!/bin/sh
# Driven inside the app container by the outer harness.
set -eu

IZNIK="$1"
CAPTURE="$2"
ACTION="${3:-screenshot}"

export XDG_RUNTIME_DIR="/tmp/xdg"
mkdir -p "$XDG_RUNTIME_DIR" "$CAPTURE"

case "$ACTION" in
    start)
        # Start cage with a long-lived dummy child to keep the compositor alive.
        WLR_BACKENDS=headless \
        WLR_LIBINPUT_NO_DEVICES=1 \
            cage -- sleep 86400 >"$CAPTURE/cage.stdout" 2>"$CAPTURE/cage.stderr" &
        echo $! > "$CAPTURE/cage.pid"
        sleep 2

        # Find the wayland socket.
        SOCK=""
        for candidate in "$XDG_RUNTIME_DIR"/wayland-*; do
            [ -S "$candidate" ] && SOCK="$(basename "$candidate")" && break
        done
        echo "$SOCK" > "$CAPTURE/wayland.sock"

        if [ -z "$SOCK" ]; then
            echo "NO_SOCKET"
            cat "$CAPTURE/cage.stderr" >&2
            exit 1
        fi

        # Launch iznik-app as a Wayland client of the running cage.
        WAYLAND_DISPLAY="$SOCK" "$HOME/bin/iznik-app" \
            >"$CAPTURE/app.stdout" 2>"$CAPTURE/app.stderr" &
        echo $! > "$CAPTURE/app.pid"
        sleep 3

        if ! kill -0 "$(cat "$CAPTURE/app.pid")" 2>/dev/null; then
            echo "APP_DIED"
            cat "$CAPTURE/app.stderr" >&2
            exit 1
        fi

        echo "STARTED"
        ;;

    screenshot)
        SOCK="$(cat "$CAPTURE/wayland.sock")"
        NAME="${4:-capture}"
        WAYLAND_DISPLAY="$SOCK" grim "$CAPTURE/${NAME}.png" 2>/dev/null
        echo "$CAPTURE/${NAME}.png"
        ;;

    key)
        SOCK="$(cat "$CAPTURE/wayland.sock")"
        KEY="$4"
        OLDIFS="$IFS"
        IFS='+'
        set -- $KEY
        IFS="$OLDIFS"
        COUNT=$#
        INDEX=0
        MODS=""
        for PART in "$@"; do
            INDEX=$((INDEX + 1))
            if [ "$INDEX" -eq "$COUNT" ]; then
                WAYLAND_DISPLAY="$SOCK" wtype $MODS -k "$PART" 2>/dev/null || true
            else
                MODS="${MODS}${MODS:+ }-M $PART"
            fi
        done
        sleep 1
        ;;

    stop)
        APP_PID="$(cat "$CAPTURE/app.pid" 2>/dev/null || true)"
        CAGE_PID="$(cat "$CAPTURE/cage.pid" 2>/dev/null || true)"
        [ -n "$APP_PID" ] && kill "$APP_PID" 2>/dev/null || true
        [ -n "$CAGE_PID" ] && kill "$CAGE_PID" 2>/dev/null || true
        echo "STOPPED"
        ;;

    *)
        echo "Unknown action: $ACTION"
        exit 1
        ;;
esac
DRIVE_SCRIPT
chmod 755 "$STAGED_DIR/drive.sh"

# -----------------------------------------------------------------------
# Start the app container
# -----------------------------------------------------------------------
header "Starting app container"
# Pass /dev/dri so wlroots can enumerate DRM devices for rendering.
DRI_FLAGS=""
if [ -d /dev/dri ]; then
    DRI_FLAGS="--device /dev/dri --group-add keep-groups"
fi
podman run --detach --init \
    --name "$APP_CONTAINER" \
    --hostname "app" \
    --network "$NETWORK" \
    --timeout "$CONTAINER_TIMEOUT" \
    --mount "type=bind,source=$STAGED_DIR,destination=$MOUNT_POINT,ro=true" \
    $DRI_FLAGS \
    iznik-e2e-app \
    sleep infinity >/dev/null

# Inject SSH keys and drive script via podman cp (mount is read-only).
podman exec --user root "$APP_CONTAINER" sh -c "
    mkdir -p /home/iznik/.ssh /home/iznik/bin
    chown iznik:iznik /home/iznik/.ssh /home/iznik/bin
"
podman cp "$SSH_DIR/id_ed25519" "$APP_CONTAINER:/home/iznik/.ssh/id_ed25519"
podman exec --user root "$APP_CONTAINER" sh -c "
    chown iznik:iznik /home/iznik/.ssh/id_ed25519
    chmod 600 /home/iznik/.ssh/id_ed25519
"
podman cp "$STAGED_DIR/drive.sh" "$APP_CONTAINER:/home/iznik/bin/drive.sh"
podman cp "$STAGED_DIR/iznik-app" "$APP_CONTAINER:/home/iznik/bin/iznik-app"
podman exec --user root "$APP_CONTAINER" sh -c "
    chown iznik:iznik /home/iznik/bin/drive.sh /home/iznik/bin/iznik-app
    chmod 755 /home/iznik/bin/drive.sh /home/iznik/bin/iznik-app
"
podman exec --user iznik "$APP_CONTAINER" sh -c "
    printf 'Host *\n  StrictHostKeyChecking no\n  UserKnownHostsFile /dev/null\n  ConnectTimeout 5\n' > ~/.ssh/config
    chmod 600 ~/.ssh/config
"
info "app container started with SSH credentials"

# Verify SSH connectivity from app container to hosts.
for index in $(seq 0 $((HOST_COUNT - 1))); do
    alias="host${index}"
    result=$(podman exec --user iznik "$APP_CONTAINER" \
        ssh -o BatchMode=yes "iznik@${alias}" "echo ok" 2>/dev/null || echo "FAIL")
    if [ "$result" = "ok" ]; then
        info "SSH to $alias: connected"
    else
        fail "SSH to $alias: cannot connect"
    fi
done

# -----------------------------------------------------------------------
# Helper: run a command inside the app container
# -----------------------------------------------------------------------
app_exec() {
    podman exec --user iznik "$APP_CONTAINER" "$@"
}

app_drive() {
    app_exec sh /home/iznik/bin/drive.sh "$MOUNT_POINT" "/tmp/captures" "$@"
}

# Copy a file out of the app container.
app_copy_out() {
    podman cp "$APP_CONTAINER:$1" "$2"
}

# -----------------------------------------------------------------------
# Launch the app inside the container
# -----------------------------------------------------------------------
header "Launching iznik-app inside cage"
LAUNCH_RESULT=$(app_drive start 2>&1 || true)
if [ "$LAUNCH_RESULT" != "STARTED" ]; then
    echo "App failed to start."
    echo "drive.sh output: $LAUNCH_RESULT"
    echo "--- cage/app stderr ---"
    app_exec cat /tmp/captures/app.stderr 2>/dev/null || echo "(no stderr file)"
    echo "--- cage/app stdout ---"
    app_exec cat /tmp/captures/app.stdout 2>/dev/null || echo "(no stdout file)"
    echo "--- end ---"
    exit 1
fi
info "app running inside cage"

# The compositor's seat has no keyboard capability until the first virtual
# keyboard is created; that creation races the first real keystroke and can
# drop it. One throwaway keystroke here absorbs the race before any test
# depends on keyboard delivery.
app_drive key "Escape" >/dev/null 2>&1 || true

# =======================================================================
# TEST 1: Renders real content, not a blank canvas
# =======================================================================
# The kit's default theme is light, and with no host or session yet the
# initial screen is mostly its background color by design; a percentage of
# white pixels does not distinguish that from a broken blank render. Distinct
# colours below do: anti-aliased text and themed chrome produce many, a
# genuinely blank or solid-color surface produces almost none.
header "Test 1: Renders real content, not a blank canvas"
TOTAL=$((TOTAL + 1))
app_drive screenshot "01_initial"
app_copy_out "/tmp/captures/01_initial.png" "$CAPTURE_DIR/01_initial.png"

WHITE=$(python3 -c "
from PIL import Image
img = Image.open('$CAPTURE_DIR/01_initial.png').convert('RGB')
px = list(img.getdata())
w = sum(1 for r,g,b in px if r>250 and g>250 and b>250)
print(100*w//len(px))
")
COLORS=$(python3 -c "
from PIL import Image
print(len(set(Image.open('$CAPTURE_DIR/01_initial.png').convert('RGB').getdata())))
")
info "white pixels: ${WHITE}%, distinct colours: $COLORS"
if [ "$COLORS" -lt 50 ]; then
    fail "only $COLORS distinct colours — no styled UI"
else
    pass "styled chrome present ($COLORS colours)"
fi

# =======================================================================
# TEST 2: Command palette visibly opens on Ctrl+Shift+P
# =======================================================================
header "Test 2: Command palette opens"
app_drive screenshot "03a_before"
app_drive key "ctrl+shift+p"
app_drive screenshot "03b_after"

app_copy_out "/tmp/captures/03a_before.png" "$CAPTURE_DIR/03a_before.png"
app_copy_out "/tmp/captures/03b_after.png" "$CAPTURE_DIR/03b_after.png"

TOTAL=$((TOTAL + 1))
DIFF=$(python3 -c "
from PIL import Image
a = list(Image.open('$CAPTURE_DIR/03a_before.png').convert('RGB').getdata())
b = list(Image.open('$CAPTURE_DIR/03b_after.png').convert('RGB').getdata())
print(sum(1 for x,y in zip(a,b) if x!=y))
")
info "pixels changed: $DIFF"
if [ "$DIFF" -lt 1000 ]; then
    fail "palette did not visibly open ($DIFF pixels changed)"
else
    pass "palette visibly opened ($DIFF pixels changed)"
fi

# =======================================================================
# TEST 3: Typing filters the palette; backspace deletes what was typed
# =======================================================================
header "Test 3: Typing filters and backspace restores the palette"
app_drive key "z"
app_drive key "z"
app_drive key "z"
app_drive screenshot "03c_typed"
app_copy_out "/tmp/captures/03c_typed.png" "$CAPTURE_DIR/03c_typed.png"

TOTAL=$((TOTAL + 1))
TYPED_DIFF=$(python3 -c "
from PIL import Image
a = list(Image.open('$CAPTURE_DIR/03b_after.png').convert('RGB').getdata())
b = list(Image.open('$CAPTURE_DIR/03c_typed.png').convert('RGB').getdata())
print(sum(1 for x,y in zip(a,b) if x!=y))
")
info "typed diff: $TYPED_DIFF pixels"
if [ "$TYPED_DIFF" -lt 1000 ]; then
    fail "typing did not visibly filter the palette ($TYPED_DIFF pixels changed)"
else
    pass "typing filtered the palette ($TYPED_DIFF pixels changed)"
fi

app_drive key "BackSpace"
app_drive key "BackSpace"
app_drive key "BackSpace"
app_drive screenshot "03d_deleted"
app_copy_out "/tmp/captures/03d_deleted.png" "$CAPTURE_DIR/03d_deleted.png"

TOTAL=$((TOTAL + 1))
DELETED_DIFF=$(python3 -c "
from PIL import Image
a = list(Image.open('$CAPTURE_DIR/03b_after.png').convert('RGB').getdata())
b = list(Image.open('$CAPTURE_DIR/03d_deleted.png').convert('RGB').getdata())
print(sum(1 for x,y in zip(a,b) if x!=y))
")
info "restored diff: $DELETED_DIFF pixels"
if [ "$DELETED_DIFF" -gt 1000 ]; then
    fail "backspace did not restore the empty query ($DELETED_DIFF pixels changed)"
else
    pass "backspace restored the empty query ($DELETED_DIFF pixels changed)"
fi

# =======================================================================
# TEST 4: Escape closes the palette
# =======================================================================
header "Test 4: Escape closes palette"
app_drive key "Escape"
app_drive screenshot "04_escaped"
app_copy_out "/tmp/captures/04_escaped.png" "$CAPTURE_DIR/04_escaped.png"

TOTAL=$((TOTAL + 1))
DRIFT=$(python3 -c "
from PIL import Image
a = list(Image.open('$CAPTURE_DIR/03a_before.png').convert('RGB').getdata())
b = list(Image.open('$CAPTURE_DIR/04_escaped.png').convert('RGB').getdata())
print(sum(1 for x,y in zip(a,b) if x!=y))
")
info "drift from pre-palette state: $DRIFT pixels"
if [ "$DRIFT" -gt 1000 ]; then
    fail "window did not return to pre-palette state"
else
    pass "palette closed"
fi

# =======================================================================
# TEST 5: App can connect to host0 via SSH (host is reachable)
# =======================================================================
header "Test 5: SSH connectivity from app to hosts"
for index in $(seq 0 $((HOST_COUNT - 1))); do
    alias="host${index}"
    TOTAL=$((TOTAL + 1))
    result=$(app_exec ssh -o BatchMode=yes "iznik@${alias}" "echo ok" 2>/dev/null || echo "FAIL")
    if [ "$result" = "ok" ]; then
        pass "app can reach $alias via SSH"
    else
        fail "app cannot reach $alias via SSH"
    fi
done

# =======================================================================
# TEST 6: iznik-server is available on hosts
# =======================================================================
header "Test 6: iznik-server is installed on hosts"
for index in $(seq 0 $((HOST_COUNT - 1))); do
    alias="host${index}"
    name="${PREFIX}-${alias}"
    TOTAL=$((TOTAL + 1))
    if podman exec "$name" test -x /usr/local/bin/iznik-server 2>/dev/null; then
        pass "iznik-server present on $alias"
    else
        fail "iznik-server missing on $alias"
    fi
done

# =======================================================================
# TEST 7: Golden image comparison
# =======================================================================
header "Test 7: Golden image comparison"
TOTAL=$((TOTAL + 1))
GOLDEN="$GOLDEN_DIR/initial_state.png"
if [ "$UPDATE_GOLDENS" = true ]; then
    cp "$CAPTURE_DIR/01_initial.png" "$GOLDEN"
    info "golden updated"
elif [ ! -f "$GOLDEN" ]; then
    fail "no golden image (run with --update-goldens)"
else
    GDIFF_RAW=$(compare -metric AE "$GOLDEN" "$CAPTURE_DIR/01_initial.png" null: 2>&1 || true)
    GDIFF="${GDIFF_RAW%% *}"
    if [ "${GDIFF:-99999}" -le "$DIFF_THRESHOLD" ]; then
        pass "golden matches ($GDIFF pixels differ)"
    else
        fail "golden differs: $GDIFF pixels (threshold: $DIFF_THRESHOLD)"
    fi
fi

# =======================================================================
# Stop the app
# =======================================================================
app_drive stop >/dev/null 2>&1 || true

# =======================================================================
# Summary
# =======================================================================
echo ""
header "Results"
if [ "$FAILURES" -eq 0 ]; then
    echo -e "  ${GREEN}${BOLD}All $TOTAL tests passed.${RESET}"
else
    echo -e "  ${RED}${BOLD}$FAILURES of $TOTAL tests FAILED.${RESET}"
    info "captures: $CAPTURE_DIR"
fi
echo ""
exit "$FAILURES"
