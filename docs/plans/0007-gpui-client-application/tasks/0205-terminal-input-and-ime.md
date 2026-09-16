---
id: terminal-input-and-ime
title: "Encode input from live terminal mode: keys, paste, mouse, IME, credit"
workstream: "0002"
kind: task
depends_on:
  - stream-credit
  - terminal-grid-element
gated: true
touches:
  - crates/iznik-client/tests/connection_manager.rs
  - crates/iznik-server/tests/pane.rs
  - crates/iznik-server/tests/fidelity.rs
  - Cargo.toml
  - Cargo.lock
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/src/grid/keyboard.rs
  - .config/nextest.toml
  - crates/iznik-app/tests/regression_window.rs
  - crates/iznik-app/tests/support/credit.rs
  - crates/iznik-app/tests/fixtures/pane_credit.rs
  - crates/iznik-app/tests/support/container.rs
  - crates/iznik-app/README.md
  - crates/iznik-app/src/surface.rs
  - crates/iznik-app/tests/surface.rs
  - crates/iznik-app/src/input.rs
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/vt.rs
  - crates/iznik-app/src/bridge.rs
  - crates/iznik-app/src/grid/mod.rs
  - crates/iznik-app/src/grid/paint.rs
  - crates/iznik-app/src/grid/ime.rs
  - crates/iznik-app/src/grid/interaction.rs
  - crates/iznik-app/tests/fixtures/input_modes.rs
  - crates/iznik-app/tests/support/mod.rs
  - crates/iznik-app/tests/vt_thread.rs
  - crates/iznik-app/tests/grid_element.rs
  - crates/iznik-app/tests/input_encoding.rs
  - crates/iznik-app/tests/ime.rs
  - policy/lexicon/terminal-input-and-ime.txt
  - regression/claims/terminal-input-and-ime.toml
status: done
merged_as: "04668c2"
---
# Encode input from live terminal mode: keys, paste, mouse, IME, credit

Make typing correct: key events encoded by the pane's live terminal mode, paste wrapped or not as bracketed paste stands, mouse events in the reported mode, IME preedit rendered at the cursor with the candidate window anchored, and credit returned as snapshots are consumed.

**Steps:**

1. Write `crates/iznik-app/src/input.rs`: key events → byte encodings selected by the emulator's live mode flags (application cursor keys, keypad mode, modify-other-keys); a table-driven mapping documented per row.
2. Paste: bracketed-paste wrapping follows the mode; clipboard content is UTF-8 and bracketed bytes are never interpreted as control input.
3. Mouse: press, drag, release and wheel encoded per the pane's active mouse mode, including SGR encoding; no encoding at all when the program has not asked.
4. IME: render the preedit string inline at the cursor and drive GPUI's IME cursor area so candidate windows anchor to the composition point; commit text flows the key path.
5. Selection copy: serialize the selected range through the emulator so the clipboard holds what the screen shows, styles notwithstanding.
6. Credit: the grid element returns credit for consumed snapshot bytes through the engine, keeping the contract's obligation — a slow surface stalls only its own pane.
7. Write `crates/iznik-app/tests/input_encoding.rs`: the mode-driven encoding table asserted row by row; paste wrapping per mode; mouse encodings per mode; preedit geometry; credit accounting equal to consumption.
8. Declare this task's claims in `regression/claims/terminal-input-and-ime.toml`.

**Tests:**

- Every mode-flag combination in the table encodes the documented bytes, including the vim-typical application-cursor case. Modified letters retain their unshifted codepoint: the pinned encoder uses CSI-u disambiguation for Control-Shift letters in legacy modes and CSI-27 in modify-other-keys mode two.
- Paste in bracketed mode wraps sanitized text without permitting embedded terminators; outside the mode it sends unframed sanitized text with carriage returns for newlines.
- Credit returned equals bytes consumed; a pane that stops consuming is the only one that stops.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task terminal-input-and-ime` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.

## Wiring and encoder corrections

The owning VT thread is the only place that can read live terminal modes.
Use the pinned emulator's key and mouse encoders with options refreshed from
that terminal for each event; the application maps GPUI events into owned
requests, rather than maintaining a second terminal encoder. The fixture
matrix covers all combinations of cursor mode, keypad mode, DEC 1035 keypad
suppression and the three modify-other-keys settings before application implementation. Its expected
bytes remain fixed when the application path replaces the direct binding
fixture consumer.

This task owns crate registration, VT request/result routing, grid event and
IME integration, bridge input/credit forwarding, and the shared test support
needed to exercise them. Private grid children keep event handling and IME
separate from row geometry. Vocabulary and claims filenames use the actual
task id, as the registry requires.

Bracketed paste must prevent embedded terminators from escaping the pasted
payload. Use the emulator's paste sanitizer and framing; outside bracketed
mode, ordinary text is unframed and newlines follow the encoder's carriage
return convention. This replaces the ambiguous promise that arbitrary remote
programs can never execute pasted text: the client guarantees correct bytes
and delimiter isolation, while the receiving application's treatment of
paste is its own behavior. Native IME candidate-window presentation remains
a display measurement; headless tests must still prove composition state,
commit routing and candidate geometry through GPUI's input-handler API.

DEC 1035 suppresses application-keypad encoding by default in the pinned
emulator. The fixture covers it explicitly; merely feeding `ESC =` does not
justify assuming an application keypad sequence.

## Service checkpoint — 2026-09-16

The 72-case mode fixture is committed before implementation. The VT service
now owns native key/mouse encoders and paste sanitization, returning encoded
input independently of render snapshots. Selection copy uses the native
plain-text formatter and rejects a frame whose sequence, viewport or width
has changed. Tests cover the matrix, Unicode and extended event types,
paste framing, mouse tracking/wheels, invalid geometry, gap recovery and
selection including graphemes, soft wraps and history. The engine bridge
forwards encoded input through its existing input path.

The task remains incomplete: GPUI event mapping, selection gestures,
composition/preedit and candidate geometry, and consumed-snapshot credit
routing still need implementation and acceptance proofs.

### Credit consumption checkpoint

Grid application now queues credit only after accepting a valid snapshot.
Sequence accounting prevents duplicates and local selection redraws from
minting grants. Screen snapshots explicitly rebase the sequence baseline and
carry no stream credit. Pending grants survive a failed submission; the bridge
routes successful submissions through `HostManager::credit` by host and pane.
Headless tests hold one pane while another consumes, exercise duplicate and
stale frames, retry failure, and verify a screen reset. Window integration must
flush consumed frames and handle channel replacement before this task can
claim end-to-end transport backpressure; these accounting tests alone do not
claim that integration is complete.

Composition proofs live in `tests/ime.rs` so the existing grid proof file stays
under the repository file-size limit. They use GPUI input-handler adapters for
UTF-16 edits and geometry, and native keystroke dispatch for registration.

### Composition checkpoint

The grid registers GPUI's entity input handler during paint. UTF-16 requests
edit only the unsent draft, preedit is shaped above cached terminal rows, and
candidate bounds and point queries use that same shaped line at the emulator
cursor. Commit emits `GridInput` through the native key path, while clipboard
paste retains its paste request and sanitization. Three headless tests cover
partial edits, surrogate boundaries, cancellation, one-time commit, candidate
geometry and real GPUI fallback text dispatch. Native candidate presentation,
key mapping and keypad metadata, pointer gestures, clipboard window routing
and transport credit integration remain pending; this task is not complete.

### Keyboard dispatch checkpoint

Named GPUI keys now become owned key requests with layout text, modifiers,
repeat state and matching releases. Shift-only history navigation stays local;
additional-modifier chords reach the terminal. Active composition and explicit
character-input preference bypass raw dispatch. Unknown key identities use
GPUI's text handler rather than inventing a physical key. The existing IME
fixture also covers this keyboard seam, avoiding duplicate VT/window setup.

This does not close the native metadata gap: keypad identity is still lost,
and normalized shifted-symbol names do not reliably preserve physical key,
unshifted codepoint or consumed-modifier metadata. The application preserves
what GPUI supplies; a framework correction is still required for full fidelity.
Pointer gestures, clipboard/window event routing and transport credit proofs
also remain pending, so the task remains planned.

### Pointer integration checkpoint

GPUI press, drag, release and wheel events now carry physical geometry and an
`InputFrame` to the VT owner. Only disabled live mouse tracking returns a
`LocalPointer` selection/history fallback; an empty report in an active mode
(such as an X10 release) stays consumed. Selection fallback requires the same
pane, sequence, dimensions and viewport. Relative wheel history movement
requires the same pane and remains valid as the viewport moves. Shift at drag
start explicitly selects locally through release, including outside the pane;
Shift-wheel explicitly moves local history. Copy uses the same frame identity
with native serialization, including a host-qualified pane check.

The native wheel expansion cap is `VtOptions::maximum_wheel_reports`, default
128 reports per request. Oversize tracked bursts return an input error instead
of allocating or processing an unbounded burst; local history retains its full
relative distance. Tests shorten the limit. The existing wheel navigation
proof now routes through the real owner before asserting local history events.

The window must still route `GridInput` to the owner, apply `LocalPointer`
replies, write clipboard replies, and flush consumed credit. The end-to-end transport backpressure proof remains open.
This task remains planned until those seams are completed and proven.

The scaled pixel fixture follows the pinned native encoder: SGR pixel
coordinates are emitted directly (8, 18), whereas cell reports add one.
The initial fixture incorrectly applied the cell offset to pixel reports;
the encoder source and its own pixel golden demonstrate the correction.

### Surface routing scope

`surface.rs` owns the per-pane GPUI entity joining a cached grid to the VT
owner and engine bridge. It retains grid input/history subscriptions, consumes
VT replies, writes native clipboard results and retries pending credit. The
window shell owns which surfaces exist and drains their shared channels; it
does not duplicate these per-pane routes. Headless surface tests exercise the
actual GPUI subscriptions and clipboard, independently of transport proofs.

### Container backpressure proof

Use the window task's standard-container relay to drive two production
`PaneSurface` instances through the real engine and VT owner. The test holds
one surface's owned VT replies without consuming them, while continuing to
consume its sibling. A fixed flood exceeds the initial per-pane credit window
but stays below the server history ring. The healthy pane must keep responding,
the held pane must stop receiving bytes at its credit bound, and releasing its
queued replies through `PaneSurface::receive` must restart and complete output.
The fixture is committed before the ignored surface-credit case in the
`regression_window` binary, sharing that binary's existing container support.
This tests actual grid consumption and transport credit without adding a
product-only pause switch or pretending an accounting-only proof is transport.

### Verified transport backpressure

The real-container proof now passes. Both panes run a no-echo, noncanonical
reader, so each input byte has exactly one output producer. Holding one pane's
native replies leaves its grid at the old sequence and caps received output
at exactly 256 KiB across two healthy sibling round trips. Releasing the held
replies through the production surface returns their receipts and advances the
grid by exactly the full 512 KiB flood plus its final marker. No synthetic
credit, engine byte event or native snapshot is used.

### Gate fixture completion correction

The native-fixture gate reproduced a fidelity test failure at 44 MiB of a
known 64 MiB file. Its support helper treated one unchanged 25 ms sample as
producer completion. Scheduling silence is not EOF. Keep the corpus, flood
size, byte equality and performance ceiling unchanged; wait for the known
expected sequence instead. This scoped support correction prevents the gate
from rejecting a still-running correct producer as truncated output.

The next gate exposed the same startup broadcast race already corrected in
other pane tests: a prompt can precede subscription. The shared-mirror test
now uses the existing persistent prompt-count helper before sending commands.
Its six-pane command round trips remain unchanged.

A further gate exposed a retry proof that subtracted the event-consumer clock
from scheduled deadlines, collapsing expired waits to zero. Bound each
original wait between submission and observation instead, and require those
intervals to be disjoint. A long configured backoff widens the jitter band
without making the test wait; no production policy or acceptance is relaxed.
