# Plan 0007 — The Client Application: GPUI Kit Renderer and Shell — 🚧 In progress

- **Status:** 🚧 In progress.
- **Goal:** Ship the iznik client application: a GPU-rendered, cross-platform terminal app built fully on GPUI Kit, presenting the existing engine's remote workspaces with a command palette over every capability, tabs at the top, sessions at the bottom, and a custom terminal grid element fed by libghostty-vt.
- **Root cause:** Plans 0001-0006 built and proved the entire remote terminal system — server, multiplexer, bootstrap, client engine, C ABI — with no user-facing client; the assumed Swift macOS app was replaced by a decision to go cross-platform Rust on GPUI Kit (gpui-component 0.6.1, rebranded gpui-kit), which provides the chrome while leaving the terminal grid over libghostty-vt as the one component to build.
- **Approach:** Scaffold iznik-app into the workspace with gpui-kit pinned exact and prove the gate runs headless first; bridge HostManager over a tokio runtime into GPUI entities; run client-side libghostty-vt terminals on one dedicated LocalSet thread (the server's mirror-thread pattern); write the one custom grid element against committed headless render budgets; assemble chrome (window, tab bar top, session bar bottom, split dividers) from model deltas; render the command palette as an inventory over the protocol command set and HostManager methods with explanations lifted from the docs; finish with settings, packaging, headless end-to-end proof, and the architecture amendment.
- **Progress:** 18/18 tasks done; 0 blocked; 0 dropped.
- **Integration:** `assembling`; run `000000003851C895D487CE9734`; base `develop` @ `e5c6ad027c10704a49fe3ea849c2c8b8180adbe3`; validation base `e5c6ad027c10704a49fe3ea849c2c8b8180adbe3`; mode —; final integration —.
- **Exceptions:** —.
- **Outcome:** A daily-drivable cross-platform iznik application crate in this workspace: remote hosts bootstrapped and multiplexed over the user's own SSH config, panes rendered by GPUI through libghostty-vt with correct query answers and mode-correct input, Rune-style palette and two tab bars composed from gpui-kit components, all gates passing including headless GPUI tests, display-bound proofs recorded as deferred, and the architecture documents amended to name the client application as this workspace's product front end.

_Last updated: 2026-09-16, against `develop` @ `e5c6ad0`._

## Verified implementation — 2026-09-16

Adoption, engine bridge, VT thread, terminal grid, foreground cleanup,
stream-credit and receive-cancellation corrections are complete. The workspace gates pass; two
display measurements remain explicitly deferred. The grid uses
cached GPUI row scenes, native emulator history and verified decoration geometry.
The rendering benchmark's recorded worst sample is 2.523 ms against its 20 ms
CPU ceiling.

Input and IME are complete: VT-owned mode encoding, composition, normalized
named-key dispatch, pointer gestures, clipboard routing and delivery-bound
credit are proven. Real-container hot/cold resume, layout deltas,
competing-window resize and per-pane transport backpressure proofs pass. The
application shell now starts the bridge and VT owner and renders model-driven
bars. Native display timing and font appearance remain separately documented in
[the rendering note](../../notes/app-render.md).

## Gate correction — foreground cleanup

A reproduced server teardown defect adds the scoped
[foreground cleanup task](tasks/0206-foreground-cleanup.md). Interactive jobs
use a foreground process group distinct from the shell's; the old cleanup
can leave that job alive and prevent the shell reaper from seeing terminal
EOF. This repair is part of making the application gate dependable, not a
relaxation of its acceptance requirements.

The correction is now verified: direct PTY drop, pane drop and close escalation
terminate the announced foreground job and reap the shell. A separate EOF case
proves close remains usable while a child wait is pending. Five claims pass,
and restoring shell-only cleanup makes the new regression proof fail.

## Stream credit correction

The window implementation exposed a delivery-versus-current-channel race in
credit return. The [stream-credit task](tasks/0207-stream-credit.md) adds
opaque delivery receipts and validates them at the host task's wire boundary.
The existing wire protocol and C ABI remain unchanged. Its fixed transition
fixture preceded implementation. Engine, queued-admission and headless app
proofs now pass, including independent-pane credit and failed-submission
accounting. The window and input tasks remain planned.

## Receive cancellation correction

A gate failure and an isolated stress run reproduced incomplete FFI pane
output. The [channel-cancellation task](tasks/0208-channel-cancellation.md)
adds a deterministic cancellation proof and repairs the channel's receive
boundary. A second deterministic proof reproduces zstd failure after repeated
empty idle polls; the decoder now waits for new input after exhaustion. Both
old-source counterexamples fail, all eight compression cases pass, and the
FFI atomic-input case passes thirty isolated runs. The correction is complete
with all five gates passing (538 tests, 70 proven branch claims).
