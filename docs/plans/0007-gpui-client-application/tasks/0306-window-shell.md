---
id: window-shell
title: "Assemble the window: pane grid from the layout tree, resize, focus, banners"
workstream: "0003"
kind: task
depends_on:
  - stream-credit
  - engine-bridge
  - terminal-input-and-ime
gated: true
touches:
  - .config/nextest.toml
  - Cargo.lock
  - crates/iznik-app/Cargo.toml
  - crates/iznik-app/tests/regression_window.rs
  - crates/iznik-app/tests/fixtures/window_lifecycle.rs
  - crates/iznik-app/tests/fixtures/window_cold.rs
  - crates/iznik-app/tests/support/container.rs
  - crates/iznik-harness/src/fixture.rs
  - crates/iznik-harness/README.md
  - crates/iznik-app/src/bridge.rs
  - crates/iznik-app/src/host_ui.rs
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/README.md
  - crates/iznik-app/src/layout.rs
  - crates/iznik-app/src/window.rs
  - crates/iznik-app/tests/window_shell.rs
  - crates/iznik-app/tests/surface.rs
  - crates/iznik-app/tests/support/engine.rs
  - policy/lexicon/window-shell.txt
  - regression/claims/window-shell.toml
status: done
merged_as: "04668c2"
---
# Assemble the window: pane grid from the layout tree, resize, focus, banners

Build the window shell: the model's layout tree rendered as panes, each pane a grid element attached to its engine subscription; client-owned geometry sent as `Resize`; focus routed to the engine and the scheduler; host state rendered as banners — and every path headless-testable.

**Steps:**

1. Write `crates/iznik-app/src/layout.rs`: the layout tree (`Split` with weights, `Leaf(pane)`) mapped to GPUI dock/resizable-panel layout, per the model deltas the bridge already applies.
2. Write `crates/iznik-app/src/window.rs`: the shell — pane area, attachment lifecycle (subscribe on appearance, unsubscribe on close, retain the emulator on reconnect: continue a hot resume from the held sequence, or reset to an arriving authoritative screen before its following output), and the resize path that sends `HostManager::resize` with the pane's computed geometry.
3. Focus: keyboard and pointer focus name a pane to `HostManager::focus`; unfocused panes keep rendering from their snapshots.
4. Host state as chrome: connecting, bootstrap progress, failed, upgrading — rendered as a banner over the pane area with the message the engine classifies, and reconnect offered as the action it already is.
5. Write `crates/iznik-app/tests/window_shell.rs`: container fixture — create session and panes by command, see the tree render, resize a split, observe the delta round-trip, drop the link, observe the banner, observe hot resume without resetting the held emulator and cold resume with a screen reset before further output.
6. Declare this task's claims in `regression/claims/window-shell.toml`.

**Tests:**

- Layout tree changes arrive as deltas and the rendered tree matches without rebuilds of unrelated panes.
- A hot reconnect preserves the emulator and continues at its sequence; a cold reconnect applies the authoritative screen before subsequent output, per the contract.
- Resize is client-owned: the pane's geometry follows the window, and the last resize wins between surfaces.

- **Done when:** `timeout 1800 cargo nextest run --package iznik-app` passes every case above, `timeout 900 cargo xtask claims verify --task window-shell` reports every claim proven, `timeout 600 cargo nextest run --package iznik-app --test regression_window --run-ignored all` passes the container cases, and `timeout 3600 cargo xtask check` succeeds.

## Integration corrections

The root architecture permits a hot `Resume` without a `Screen`; the original
screen-on-every-reconnect instruction contradicted that contract. Retain the
VT owner and surface across transport loss. Only a real `Screen` resets them.
The window and input task are implemented together where their acceptance
depends on shared routing; neither is marked complete until its full tests pass.

Register the layout and window modules in the crate root. Headless tests can
prove model-to-component translation and retained entity identity without a
server. Transport, reconnect and competing-client geometry proofs use the
repository container fixture, as the contributing rules require.

## Layout checkpoint

The translator uses kit resizable panels for every split. Model weights set
the initial flex shares; kit measurements retain those ratios as the window
resizes. A layout revision gives changed model trees fresh divider state
while the caller supplies the same pane entities. The headless test measures
nested axes, unequal weights, window resizing and new model weights.

This is not a completed window: model-driven attachment, focus, geometry
submission, host banners, channel replacement and container lifecycle proofs
remain pending. The task remains planned.

The shell also exposes the bridge's existing unsubscribe/focus capabilities
and uses an explicit HostUi event-ingestion method so terminal messages are
routed before the same event updates the model mirror. These changes avoid a
second engine owner or a duplicate model reducer in the shell.

## Shell routing checkpoint

`WindowShell` owns `HostUi` and the shared VT thread, retaining host-qualified
pane entities through layout changes and transport loss. It selects an
existing model tab, subscribes on appearance, unsubscribes when hidden, and
closes the native pane only after model removal. Input and clipboard routing
remain in `PaneSurface`. Focus calls the existing engine scheduler path.
Measured complete-cell geometry submits changed sizes; authoritative model
dimensions resize initialized native terminals. Kit alerts render classified
host states and local failures, with reconnect through the engine operation.

The update cadence is an option, as is the maximum number of ready messages
read from each owner per update. Headless tests disable the periodic task and
shorten the drain budget to one, proving model ingestion, retained surface and
native content, model dimension routing, and the actual connection banner.
Shared unattached-engine test paths moved to `tests/support/engine.rs`.

This remains a checkpoint: the binary still opens its adoption view. Real
attachment, competing-client resize, cold/hot reconnect, and credit across
channel replacement require the container fixture and stronger lifecycle
accounting before startup integration. Neither task is complete.

## Container proof boundary

The original in-process testing sentence conflicts with the contributing
rules and root architecture. Lifecycle proofs therefore run in the existing
two-container fixture. GPUI remains headless on the developer machine; an
isolated Unix relay carries its production engine frames to an SSH process
inside the fixture's engine container. Credentials and the remote daemon
stay inside the fixture. No developer SSH state or running server is used.

Add the existing workspace harness as an application development dependency;
this changes no external package versions. The harness supplies the streaming
container command so container naming and user selection remain owned by the
fixture. The test owns process cancellation and deadlines. Container tests
live in the ignored `regression_window` binary, while existing in-process
model/layout tests remain ordinary tests. Commit the lifecycle fixture before
implementing its runner. Run the ignored binary explicitly as additional
acceptance; a default nextest run cannot prove container cases.

## Container hot-resume checkpoint

The ignored production-window proof now creates a real session, receives its
process output and survives a controlled relay cut. Before the cut it scrolls
the native terminal into history; after resumed bytes arrive, that viewport
and the same GPUI pane entity remain. An authoritative reset would return the
viewport to bottom, so this assertion distinguishes hot resume from repainting
a server screen. Warm execution passes in under eight seconds. Cold resume,
real split deltas, competing-client geometry and pane backpressure remain open.

## Container lifecycle and geometry checkpoint

The cold fixture holds its producer until the window has disconnected, then
writes five mebibytes against the four-mebibyte default ring. Recovery replaces
the scrolled-back viewport with the authoritative bottom screen; subsequent
process output advances from that new sequence baseline. Both resume cases
also inspect the actual visible disconnect banner.

A third fixture case creates a split through a real command, changes its
weights through `SetLayout`, and retains the original pane entity. It then
opens an independent production window on the same container host. Resizing
each window in turn drives the real prepaint/engine path, and both windows'
models and native terminal dimensions converge on the latest writer.
All three cases pass together; input metadata and per-pane backpressure remain
separate unfinished acceptance work before application startup integration.
