# Plan 0007 — The Client Application: GPUI Kit Renderer and Shell
## 0001 — Adoption and Engine Bridge

### `engine-bridge`

`crates/iznik-app/src/bridge.rs` and `host_ui.rs`: a private tokio runtime owns `HostManager`; `events()` drain onto a channel the main thread reads inside its update cycle. The one rule: the main thread never calls the engine on a path that waits on the engine's own task. Host entities mirror connection state, apply snapshots and deltas into `ClientModel`, and turn `ManagerEvent`s — including an upgrade offer — into notifications. The public surface is add, remove, reconnect, upgrade, uninstall, and `HostManager::command`. Tests drive a `unix:` host through `iznik-testkit` inside GPUI's test context.

This task is where the workspace the app's crate lands in first goes green under the new graph, so its footprint reaches past `crates/iznik-app`: a green `cargo xtask check` is this task's acceptance, and the app's dependency closure is what turns the gates red for reasons no file in the crate can reach — feature unification that scores three of `iznik-server`'s functions past the cognitive-complexity threshold, a `cargo metadata` document past the harness's capture limit, a claims fixture answering to the developer's own Git hooks, and a pane inheriting `TERMINFO` from the daemon's shell. Its `touches` list names each, and the regenerated lockfile the pin needs.

## 0002 — Terminal Rendering

### `vt-thread` wiring correction

Adoption did not declare the emulator or a production tokio dependency in
`iznik-app`. The VT task adds their existing exact pins, registers its module,
and exposes the bridge's pane operations; these are prerequisites to the
service, not new dependencies or changes to the engine contract. Corpus
fixtures already committed by the foundations plan remain authoritative.
Query ownership stays as the root architecture specifies: the server answers
only without subscribers, and the attached application answers from its own
emulator and theme.

### `terminal-grid-element` wiring correction

The renderer consumes owned viewport snapshots. Scroll requests run on the
VT owner and return the emulator's viewport offset; the renderer never
reconstructs history from output or stores a second emulator. Cached GPUI
row entities retain paint subtrees until changed cells, cursor or selection
invalidate that row. The headless budget runs as a short bench-target test
under the default nextest profile, since no `regression` profile exists.

### `stream-credit` correction

Output credit belongs to the stream that delivered it, not whatever channel
currently carries the same pane. The engine attaches an opaque receipt to
each output delivery; clones share one return state. The host task validates
stream ownership again immediately before carrying a queued grant. The app
preserves receipts through VT snapshots and returns them only on accepted
consumption. Channel replacement, disconnect and manager replacement expire
old receipts without changing the wire or the existing C ABI. See the
[scoped correction](tasks/0207-stream-credit.md) for proofs and compatibility.

## 0003 — Application Chrome

## 0004 — Command Palette

## 0005 — Application Platform

## 0006 — Proof and Handoff

# Architecture — The Client Application

## 1. What the application is

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

## 2. Layers and dependencies

| Layer | Crate | Role |
|---|---|---|
| Framework | `gpui` (pinned exact, 0.2.x at adoption) | event loop, GPU renderer, text system, entity model |
| Components | `gpui-kit` 0.6.1 (`gpui-component`, `gpui-base`, `gpui-kit-assets`) | palette, tab bars, inputs, menus, dock/resizable panels, theming |
| Emulator | `libghostty-vt` (the same Zig-built static library the server links) | pane bytes → cell state, modes, query composition |
| Engine | `iznik-client` | `HostManager`: transport, bootstrap, model, reducer, commands |
| Application | `iznik-app` (new workspace member) | everything visible; the only new product code |

Versions are pinned exact in the adoption task and justified in `policy/dependencies.md`; the resolved set must equal the allowlist, in both directions, per the dependency policy. The icon crate the kit's documentation names is adopted with them — icons are not bundled by the kit.

## 3. Threading model

Three owners, three threads, one seam each:

- **The GPUI main thread** owns entities, the event loop, and every element. It never blocks: the engine's answers arrive as events, never as calls.
- **The engine task** runs `HostManager` on a private tokio runtime inside `iznik-app`. `HostManager` is `Send` and its calls are thread-safe; the bridge forwards `ManagerEvent`s to the UI over a channel the main thread drains.
- **The vt thread** is one dedicated thread running a `LocalSet`, owning every client-side `libghostty-vt` terminal. The handles are `!Send`; the server solved this exact problem with its mirror thread and the client copies the pattern. Pane bytes arrive on the vt thread, feed the emulator, and produce cell snapshots delivered to the grid entities over a channel.

The seams are explicit: engine → vt thread (subscribe and input calls), vt thread → grid (snapshots, sequence-tagged), grid → engine (credit returned as bytes are consumed, query responses forwarded via `input`), engine → UI (`ManagerEvent`s). Nothing holds a lock across a channel send; nothing on the main thread touches a terminal handle.

## 4. The terminal grid element

The one custom component, and the riskiest, so its contract is written here:

- **Input** is a cell snapshot: columns, rows, the visible cell state (text runs, styles, colors, cursor), the scrollback extent, and the absolute pane sequence it reflects. Snapshots are cheap because the emulator holds state; the element never parses bytes.
- **Rendering** goes through GPUI's text system (shaping included, ligatures included) onto its renderer — no second glyph atlas is built. Damage is tracked per snapshot; an idle pane repaints nothing.
- **Scrollback** is a viewport over the emulator's history; the mouse wheel and keyboard move it, and arrival of new output snaps to bottom only when already at bottom.
- **Selection** is element state; copy produces bytes through the emulator's serialization so what lands on the clipboard is what the screen shows.
- **Input encoding** runs on the VT owner through the pinned emulator's key, mouse and paste encoders. Each event refreshes mode options from the live terminal; GPUI sends owned requests and never reads a terminal handle. Paste strips unsafe control bytes before framing, preventing embedded terminators from escaping bracketed payloads.
- **IME** renders the preedit string and drives GPUI's IME cursor area, so candidate windows float at the caret. The kit's text input components demonstrate the pattern; the grid follows it.
- **Credit** is returned as the element consumes snapshots — the contract's mandatory obligation, kept per-pane so a slow surface stalls only itself.
- **Budget.** A 10k-cell scrolling grid fed by a live emulator must produce a draw list inside a committed ceiling measured headlessly; the number lands in `docs/notes/app-render.md` beside the machine it was taken on, following the baseline note's discipline. Frame timings on real displays are recorded there by hand on macOS and Linux and are claims marked deferred, not silently absent.

## 5. Actions and the command palette

Every capability the application offers is a GPUI action, and the action set is **closed and generated by test**: one action per protocol session command (`CreateSession`, `CreateTab`, `CreatePane`, `MovePane`, `SetLayout`, …), one per `HostManager` capability the UI exposes (add host, reconnect, upgrade, uninstall, doctor). A registry table maps each action to its engine call, its explanation — lifted from the command documentation the workspace already maintains, one source of truth — and its availability predicate ("New Tab" needs a session; "Reconnect" needs a failed host). The palette is the registry rendered: gpui-component's command palette with fuzzy filter, keyboard navigation and explanation rows. Keybindings bind the same actions, so palette and chords cannot disagree, and a test fails the build when an action exists without an explanation or an engine target.

Commands ride the client's existing optimistic path: unambiguous changes apply locally and reconcile; creations wait a round trip; refusals surface as notifications, not dialogs.

## 6. Chrome from the model

The window shell renders the client model and nothing else: sessions as the bottom bar, the focused session's tabs along the top, the tab's layout tree as dock panels. The server already restores layouts across reconnects; the client owns geometry and sends `Resize` — the last resize wins, as the contract states. Marks drive badges (running command, exit status, alternate screen). Host state changes render as a banner and as the session bar's state dot; a reconnection is a banner and a resumed stream, never a reload.

## 7. Testing and proof

- **Headless by construction.** GPUI's test context renders element trees without a display, so the application crate's tests run under nextest on the Linux gate machine like every other crate's. The adoption task proves this before anything builds on it.
- **The container stack proves lifecycle behavior.** The existing two-container harness owns SSH credentials and the real daemon. Headless GPUI runs on the test host and reaches the fixture through an isolated Unix relay to its engine-container SSH process. Tests that start the fixture live in ignored `regression_*` binaries. In-process model, rendering and encoding proofs remain ordinary integration tests. This corrects the original in-process-stack assumption to match the contributing rules and root architecture.
- **Claims.** Application tasks declare claims proven by tests, each with a `because`; display-bound presentation measurements declare an existing `display` record under `docs/notes/` and a reason; the verifier always reports them deferred. The operating-system filter alone cannot express a manual measurement on a headless machine running that same operating system. A record never counts as an automated GPU proof.
- **Fidelity is inherited, not retested.** The corpus and VT oracle already pin the emulator; the vt thread's tests feed corpus bytes and assert snapshots, and stop.

## 8. Decisions

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

## Gate correction: foreground job ownership

The application gate exposed an existing server teardown race. A shell's
ordinary foreground job has a separate process group, so killing only the
shell group can leave a job holding the terminal open. The scoped
[foreground cleanup task](tasks/0206-foreground-cleanup.md) centralizes forced
cleanup in the PTY owner, including the terminal's reported foreground group.
Pane drop remains nonblocking with respect to a reaper holding the process
mutex. Readiness comes from the foreground child, not the shell's pre-exec
command mark. The correction preserves the existing boundary for detached
jobs that escape both owned groups.
