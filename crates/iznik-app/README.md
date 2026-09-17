# iznik-app

The application iznik was built to feed: a GPU-rendered cross-platform
terminal client on GPUI Kit. One window; the pane grid as the body; a tab
bar along the top; a session bar along the bottom; and a command palette
over everything. It links `iznik-client` as a Rust library and
`iznik-protocol` for the wire's own types; the C ABI stays the boundary for
other front ends.

What exists today is the window and the engine behind it. The binary opens
one GPUI window holding the crate's themed `EmptyView`, answers `--help`
without touching a display, and refuses no display at link time — a machine
with no X11 or Wayland session is a condition the running application
reports, not a build that fails. `EmptyView` paints its surface from the
kit's active theme, background and foreground included, through the 0.3.5
`gpui` line that `gpui-kit` 0.6.1 pins exact.

## Modules

| Module | Holds |
|---|---|
| `bridge` | The engine as the window sees it: a `HostManager` on the tokio runtime it owns, one channel carrying every `ManagerEvent` to the window's thread, and the operations whose calls wait on a host's own task performed off it. |
| `bars` | Model-driven tab and session bars rendered above the pane area. |
| `actions` | Closed action inventory shared by default keybindings and the command palette. |
| `palette` | Fuzzy, availability-aware command palette projection. |
| `grid::interaction` | Keyboard and pointer dispatch, matching releases, frame-bound selection, and live-mode history fallback. |
| `grid::keyboard` | Normalized GPUI keystrokes mapped into owned terminal key requests. |
| `grid::ime` | Unsent UTF-16 composition, cursor-relative preedit shaping, candidate geometry and GPUI input-handler registration. |
| `grid::paint` | GPUI shaping, cell fills, text decorations, cursor painting, and cached row entities. |
| `grid` | Custom GPUI row painting from owned snapshots, cached row damage, selection geometry, and viewport requests to the VT owner. |
| `window` | Model-driven pane lifetime, shared owner polling, focus/geometry submission, and kit host-state banners; transport lifecycle proofs and startup integration remain pending. |
| `layout` | Model split directions and weights rendered through kit resizable panels with caller-owned leaf entities. |
| `splits` | Pure divider weight and equalization helpers for authoritative layouts. |
| `settings` | Validated settings state retaining shared theme and keybinding overrides. |
| `settings_window` | The settings window: a second OS window over the shell's live theme and the closed keybinding inventory. |
| `theme` | Application theme mapped into terminal emulator defaults. |
| `bundle` | Deterministic Linux and macOS application layout writers. |
| `surface` | Per-pane grid subscriptions, native clipboard delivery, engine input forwarding and consumption-credit retry. |
| `vt` | One `LocalSet` thread owning client emulators, sequence-checked pane feeds, theme-aware query answers, and owned cell snapshots with damage and credit. |
| `host_ui` | The window's own state: per-host connection state, the client model mirror updated from snapshots and deltas through `iznik-client`'s reducer, the upgrade a host returns with its connection, and the notices a surface shows. |
| `lifecycle` | Cross-window application lifecycle glue, such as quitting when a named window closes. |

## The engine bridge

`EngineBridge` builds the manager and starts two threads of its own: one that
reads what the manager says and puts it on a channel, and one that performs
the operations whose manager calls wait for a host's task to end. The
window's thread drains the channel in its update cycle and never waits on any
of it.

The one rule is that the window's thread never calls the engine on a code
path that waits on the engine's own tasks. `HostManager::remove_host`,
`HostManager::upgrade` and `HostManager::uninstall` each wait, inside the
manager, for one host's task — and a task part way through a bootstrap does
not look at its orders until the bootstrap is over, which may be minutes.
Called from the thread that draws frames, that is a window that has stopped
drawing for as long as the slowest host somebody named takes. Those three are
therefore asked for and answered at once, and what they did arrives as
`EngineEvent::Finished`.

`EngineState` is the whole of what a window knows, as a value with no engine
in it and no thread behind it: a case can feed it a snapshot and a thousand
deltas without a daemon anywhere. It reads a snapshot or a delta by turning
the payload back into the message the wire carried and handing it to
`iznik_client::reduce::reduce`, so the mirror a window draws from converges
where the engine converges by construction rather than by two
implementations agreeing.

## Headless tests

GPUI's test context renders windows without a display, so this crate's tests
run under nextest on the Linux gate machine like any other crate's:
`cargo nextest run --package iznik-app`. The smoke test builds one themed
window and asserts the element tree it produces; the bridge's cases drive the
engine against the `iznik-testkit` in-process stack over a `unix:` alias —
add host, observe the state transitions, create a session by command, see it
in the mirror, and see a refusal surface as a notice — with the whole of it
inside GPUI's own test context.

## Client terminal service

`VtThread` owns every non-Send libghostty-vt handle on one dedicated thread.
`EngineBridge::feed_terminal` routes subscription screens and output to it;
`EngineBridge::terminal_event` forwards its query replies as input and requests
a fresh screen after a sequence gap. The grid consumes owned snapshots and
returns their `consumed_bytes` through `EngineBridge::credit`. Screen replay
consumes no stream credit and never replays historical query effects.

Snapshots retain graphemes, widths, styles, resolved colors, cursor state,
alternate-screen state and history extent. Both row and global damage are
reset after snapshot extraction. Themes change emulator defaults while
preserving program OSC overrides. A gap invalidates the pane until a server
screen replaces it; no output is guessed across a discontinuity.

## Terminal grid

`TerminalGrid` consumes owned `TerminalSnapshot` values. Equal row draw lists
reuse GPUI's cached paint subtrees, including when sibling rows change.
Narrow text runs retain font ligatures; wide graphemes are anchored at their
own terminal columns so fallback glyph advances cannot shift following text.
Selection and cursor geometry use the same cell metrics as row layout.

The grid emits `GridScroll` for wheel and Shift-PageUp/PageDown/Home/End
navigation. The window routes these requests as `VtCommand::Scroll` and
applies the returned viewport snapshot. The VT owner alone retains history;
new output follows the bottom only when the viewport was already there.
The headless CPU budget and pending display measurements are recorded in
[the rendering note](../../docs/notes/app-render.md).

`input` owns keyboard, paste and pointer requests and invokes the pinned native encoders on the VT thread, refreshing modes for every event. Encoded input is forwarded by `bridge` independently of render snapshots.

`GridInput` carries committed composition and clipboard paste to the VT owner.
The platform can edit only the unsent draft; remote output is never exposed as
editable text. Preedit updates send no bytes, an empty marked replacement
cancels, and commit clears the draft before emitting one native key request.
Candidate bounds and pointer offsets use the same shaped line as painting.
The window must subscribe to these owned events and route clipboard results;
that window integration is still pending.

Pointer events include a frame identity and local gesture. The VT owner returns
`LocalPointer` only when live tracking is disabled; a filtered program event
never becomes selection or history accidentally. The window applies those
replies with `TerminalGrid::apply_pointer`. Shift drag and Shift-wheel explicitly
choose local behavior. `copy_selection` requests native serialization with the
same frame identity. Wheel expansion is bounded by `VtOptions`, with oversized
tracked bursts returned as input errors rather than truncated silently.

Delivery receipts accompany engine output through the bridge, native snapshot
and accepted grid frame. The grid retains their identities across retries
and screen resets; duplicated frames add no second receipt, and screen
reconstruction earns no credit. The engine admits each current receipt once
at its transport boundary. Offline fixtures without receipts retain their
count-based accounting path.

The ignored `regression_window` test runs the production headless window through an isolated Unix relay to SSH inside the standard two-container fixture. It creates a real session and proves hot resume retains a client-owned history viewport and the pane entity. Run it with `cargo nextest run --package iznik-app --test regression_window --run-ignored all`.

The same container suite forces cold resume by expiring the remote history ring, observes the real disconnect banner, applies real split-layout deltas, and verifies that two independent windows converge on the last client-owned resize.

Its surface-credit case holds one pane's owned native replies at the real transport credit bound while a sibling stays interactive; consuming those replies through `PaneSurface` releases exactly the complete pending output.
