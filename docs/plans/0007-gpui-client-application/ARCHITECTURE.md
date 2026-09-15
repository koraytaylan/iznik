# Architecture — Plan 0007

> The concrete deltas, by symbol. The system is described in the root [`ARCHITECTURE.md`](../../../ARCHITECTURE.md); the rules and the way of working are in [`CONTRIBUTING.md`](../../../CONTRIBUTING.md). Read both before your first task. Every task here declares its claims in `regression/claims/<task-id>.toml`; a `test` proof carries its `because`. Display-bound proofs say `platform` and why they are deferred, following `darwin-artifacts`.

## What the application is

A cross-platform desktop application — macOS first, Linux supported — that presents iznik's remote workspaces. One window; the pane grid as the body; a tab bar along the top; a session bar along the bottom; a command palette over everything. It links `iznik-client` directly as a Rust library. The C ABI (`iznik-ffi`) remains the boundary for other front ends and keeps its contract untouched; this application simply does not need to cross it.

```text
iznik-app (gpui)
├─ window shell ─── tab bar (top) · pane grid · session bar (bottom) · palette overlay
│   └─ terminal grid element ──── reads cell snapshots
├─ ui state entities ── model mirror, availability, notifications
├─ bridge ────────────── channels between GPUI and the engine
├─ engine task (tokio) ─ HostManager: hosts, subscriptions, commands, credit
└─ vt thread (LocalSet) ─ libghostty-vt per pane · query responses · snapshots
      └─ iznik-client ── ssh <host> iznik-server --stdio ── iznik-server daemon
```

## Layers and dependencies

| Layer | Crate | Role |
|---|---|---|
| Framework | `gpui` (pinned exact, 0.2.x at adoption) | event loop, GPU renderer, text system, entity model |
| Components | `gpui-kit` 0.6.1 (`gpui-component`, `gpui-base`, `gpui-kit-assets`) | palette, tab bars, inputs, menus, dock/resizable panels, theming |
| Emulator | `libghostty-vt` (the same Zig-built static library the server links) | pane bytes → cell state, modes, query composition |
| Engine | `iznik-client` | `HostManager`: transport, bootstrap, model, reducer, commands |
| Application | `iznik-app` (new workspace member) | everything visible; the only new product code |

Versions are pinned exact in the adoption task and justified in `policy/dependencies.md`; the resolved set must equal the allowlist, in both directions, per the dependency policy. The icon crate the kit's documentation names is adopted with them — icons are not bundled by the kit.

## Threading model

Three owners, three threads, one seam each:

- **The GPUI main thread** owns entities, the event loop, and every element. It never blocks: the engine's answers arrive as events, never as calls.
- **The engine task** runs `HostManager` on a private tokio runtime inside `iznik-app`. `HostManager` is `Send` and its calls are thread-safe; the bridge forwards `ManagerEvent`s to the UI over a channel the main thread drains.
- **The vt thread** is one dedicated thread running a `LocalSet`, owning every client-side `libghostty-vt` terminal. The handles are `!Send`; the server solved this exact problem with its mirror thread and the client copies the pattern. Pane bytes arrive on the vt thread, feed the emulator, and produce cell snapshots delivered to the grid entities over a channel.

The seams are explicit: engine → vt thread (subscribe and input calls), vt thread → grid (snapshots, sequence-tagged), grid → engine (credit returned as bytes are consumed, query responses forwarded via `input`), engine → UI (`ManagerEvent`s). Nothing holds a lock across a channel send; nothing on the main thread touches a terminal handle.

## Decisions

| Decision | Why |
|---|---|
| GPUI Kit, full-in, over hand-rolled or egui chrome. | The only option where the palette, tab bars, splits and IME-correct input already exist at shipping quality (Zed and Longbridge's own apps); the hand-rolled path's failure mode is a toolkit growing by scope creep, and egui buys speed to chrome that still looks like a theme. |
| The grid element stays custom; the kit's terminal view is not adopted. | The client's emulator is `libghostty-vt` — shared with the server, pinned by the fidelity corpus. Two emulators in one client is exactly the second-implementation bug the architecture forbids; the kit's view is read for technique, not linked. |
| The application joins this workspace. | One gate, one dependency allowlist, one lexicon; the architecture amendment is one commit. The separate-repository option stays open because the crate's public surface is the engine, not the app. |
| Client emulator stays. | Theme-true query answers, mode-correct input encoding, and cell state for the renderer all require emulator state in the client; the server's `Screen` being exact depends on both ends running the same engine. |
| One vt thread, mirroring the server. | `libghostty-vt` handles are `!Send`; the mirror-thread pattern is already proven on the server side of the same library. |
| Palette as inventory, not settings system. | The closed action set is where "everything you can do, with explanations" comes from; per-entry argument forms are deferred until a command needs them. |
| Exact pins everywhere. | GPUI's 0.x cadence is the accepted cost; the allowlist makes every upgrade a reviewed edit. |
| Display-bound proofs deferred, named. | A proof that needs a Mac display is recorded like the Darwin artifacts: platform-bound, reported as deferred, never silently missing. |

## 0001 — Adoption and Engine Bridge

### `gpui-adoption`

`crates/iznik-app` joins the workspace as a member written once: manifest, crate root, binary, lexicon and dependency allowlist entries. `gpui-kit` is pinned exact at 0.6.1 with the icon crate the kit names; `iznik-client` and `iznik-protocol` are the engine; `iznik-testkit` is a development dependency for the in-process stack. The binary opens one themed empty window, answers `--help`, and treats a missing display as a runtime condition, not a link failure. `crates/iznik-app/tests/headless_smoke.rs` uses GPUI's test context to build a window with one styled element and assert the element tree, proving the crate's tests run headlessly under nextest on the Linux gate machine before anything builds on it. This task also owns `cargo xtask check` under the new graph: commit the lockfile the pin rewrites, and if the member unmasks a lint in another crate, restructure that crate here — never allow, never leave the workspace red for a later module task.

### `engine-bridge`

`crates/iznik-app/src/bridge.rs` and `host_ui.rs`: a private tokio runtime owns `HostManager`; `events()` drain onto a channel the main thread reads inside its update cycle. The one rule: the main thread never calls the engine on a path that waits on the engine's own task. Host entities mirror connection state, apply snapshots and deltas into `ClientModel`, and turn `ManagerEvent`s — including an upgrade offer — into notifications. The public surface is add, remove, reconnect, upgrade, uninstall, and `HostManager::command`. Tests drive a `unix:` host through `iznik-testkit` inside GPUI's test context.

## 0002 — Terminal Rendering

### `vt-thread`

`crates/iznik-app/src/vt.rs`: one dedicated thread, a `LocalSet`, every pane's `libghostty-vt` terminal owned there. The handle set is feed, resize, set theme colors, request snapshot, close; no other thread touches a handle. Pane output from the engine's subscribe path produces a sequence-tagged cell snapshot (text runs, styles, colors, cursor, alternate-screen state, scrollback extent). Query responses (`on_pty_write` and the dedicated effects the terminal-mirror note records) go back as pane input. Theme colors applied here are the colors a program's queries describe. Tests feed corpus bytes and assert snapshots against the VT oracle.

### `terminal-grid-element`

`crates/iznik-app/src/grid.rs` is the one custom component, and the riskiest, so its contract is written here:

- **Input** is a cell snapshot: columns, rows, the visible cell state (text runs, styles, colors, cursor), the scrollback extent, and the absolute pane sequence it reflects. Snapshots are cheap because the emulator holds state; the element never parses bytes.
- **Rendering** goes through GPUI's text system (shaping included, ligatures included) onto its renderer — no second glyph atlas is built. Damage is tracked per snapshot; an idle pane repaints nothing.
- **Scrollback** is a viewport over the emulator's history; the mouse wheel and keyboard move it, and arrival of new output snaps to bottom only when already at bottom.
- **Budget.** A 10k-cell scrolling grid fed by a live emulator must produce a draw list inside a committed ceiling measured headlessly; the number lands in `docs/notes/app-render.md` beside the machine it was taken on. Frame timings on real displays are recorded there by hand on macOS and Linux and are claims marked deferred, not silently absent.

### `terminal-input-and-ime`

`crates/iznik-app/src/input.rs`: keys encoded from the pane's live mode flags (application cursor keys, keypad, modify-other-keys); paste wrapped or not as bracketed paste stands; mouse press, drag, release and wheel encoded only in the reported mode, SGR included. IME renders the preedit string and drives GPUI's IME cursor area so candidate windows float at the caret. Selection is element state; copy produces bytes through the emulator's serialization so what lands on the clipboard is what the screen shows. Credit is returned as the element consumes snapshots — the contract's mandatory obligation, kept per-pane so a slow surface stalls only itself.

## 0003 — Application Chrome

### `window-shell`

`crates/iznik-app/src/layout.rs` and `window.rs`: the model's layout tree (`Split` with weights, `Leaf(pane)`) mapped to GPUI dock/resizable panels. Each visible pane is a grid element attached to its engine subscription — subscribe on appearance, unsubscribe on close, re-attach on reconnect with screen-first semantics (reset to the arriving screen before further output). Geometry is client-owned and sent as `Resize`; the last resize wins, as the contract states. Focus names a pane to `HostManager::focus`. Host state — connecting, bootstrap progress, failed, upgrading — is a banner over the pane area with the message the engine classifies. A reconnection is a banner and a resumed stream, never a reload.

### `tab-and-session-bars`

`crates/iznik-app/src/bars.rs`: the window shell renders the client model and nothing else. The focused session's tabs run along the top — title, activity badge from marks (running command, exit status, alternate screen), close, overflow into a scrollable strip. Every host's sessions run along the bottom, grouped by host, with the host's connection state as a dot. Switching, closing and empty states (no hosts, no sessions, host failed) are rendered states, never a blank bar. Bars track snapshots and deltas; optimistic effects come through the reducer as the engine defines them.

### `split-chrome`

`crates/iznik-app/src/splits.rs`: divider hit zones over the layout tree. A drag maps pointer motion to integer weights and sends `SetLayout`; the rendered tree follows the returned delta, so a refused or coalesced drag leaves the layout the host holds. Keyboard split, move and equalize are inventory actions. After a layout delta each pane's size is recomputed and sent as `Resize`. The drag preview is local; the authority is the delta.

## 0004 — Command Palette

### `action-inventory`

`crates/iznik-app/src/actions.rs`: every capability is a GPUI action, and the action set is **closed and generated by test**. One action per protocol session command (`CreateSession`, `CreateTab`, `CreatePane`, `MovePane`, `SetLayout`, …), one per `HostManager` capability the UI exposes (add host, reconnect, upgrade, uninstall, doctor). A registry table maps each action to its engine call, its explanation — lifted from the command documentation the workspace already maintains, one source of truth — and its availability predicate ("New Tab" needs a session; "Reconnect" needs a failed host). `crates/iznik-app/assets/default-keybindings.json` binds chords over the same table, so palette and chords cannot disagree. A test fails the build when an action exists without an explanation or an engine target, or when a protocol command or public `HostManager` method is missing from the inventory.

Commands ride the client's existing optimistic path: unambiguous changes apply locally and reconcile; creations wait a round trip; refusals surface as notifications, not dialogs.

### `command-palette`

`crates/iznik-app/src/palette.rs`: the registry rendered. gpui-component's command palette overlay — dimmed, centered, focus-trapped — with fuzzy filter, keyboard navigation and explanation rows. Availability is evaluated against the focused host, session and pane; unavailable entries never list. Dispatch is the action path: a command fired from the palette is indistinguishable on the wire from its keybinding. The palette closes on dispatch; results and refusals surface as notifications in the shell.

## 0005 — Application Platform

### `settings-and-theme`

`crates/iznik-app/src/theme.rs` and `settings.rs`: a struct of colors, font family and size, mapped to both GPUI theming and the vt thread's emulator palette, so query answers and rendering share one source. Settings live under the platform's configuration directory, load at start, watch for change, and apply hot. Invalid settings refuse with the field named and the previous values kept. Keybinding overrides merge over the inventory's default asset; an unknown action or a collision is a refusal.

### `app-packaging`

`crates/iznik-app/src/bundle.rs` and `xtask/src/distribution/app.rs`: given a built binary and the assets, write the macOS `.app` (`Contents/MacOS/iznik-app`, `Contents/Info.plist` with workspace version and GPUI's minimum macOS, icon set) and the Linux layout (binary, `.desktop`, icons). `cargo xtask app-bundle --target <triple>` stages and bundles, reusing the distribution task's staging. Layout, plist fields and version stamping are asserted from staged fixtures on the Linux development machine; codesign and notarization are deferred steps on a Mac, named in `docs/notes/release-checklist.md`.

## 0006 — Proof and Handoff

### `app-end-to-end`

`crates/iznik-app/tests/end_to_end.rs` is the in-process stack as the fixture: the full assembly (window, bars, palette, grid, bridge, vt thread) against `iznik-testkit` over a `unix:` alias — add host, open the palette, create a session and a pane, type, resize, split, drop the link, resume. Assertions read rendered application state (model mirror, element tree, snapshots), never engine internals. Screen-before-output on every attach and resume; credit consumed equals credit returned; query answers describe the application's theme. The run's shape lands in `docs/notes/app-render.md` beside the render budget.

Headless by construction: GPUI's test context renders element trees without a display, so the application crate's tests run under nextest on the Linux gate machine like every other crate's. Fidelity is inherited, not retested — the corpus and VT oracle already pin the emulator; the vt thread's tests feed corpus bytes and assert snapshots, and stop.

### `architecture-amendment`

The root `ARCHITECTURE.md`, `README.md` and `docs/CLIENT.md` currently name a Swift macOS application in another repository. This chore rewrites them so the architecture names the cross-platform GPUI client this plan builds: §1 names `iznik-app` as the one user interface; §2's topology names the application crate; §3's crate table gains `iznik-app`. `docs/CLIENT.md` stays the boundary for other front ends; its audience sentence names this application as its first reader.

### `app-handoff`

Every task's claims file is registered; full coverage runs once. Display-bound and Darwin-bound proofs (`terminal-grid` frame timings, `app-packaging` codesign and notarize) carry `platform` and `because`. The release checklist's application section names bundle, sign, notarize by hand on a Mac, the headless end-to-end on the development machine, and a re-measured render budget on the release machine. Plan `STATUS.md` and the root roll-up stay coordinator-owned.
