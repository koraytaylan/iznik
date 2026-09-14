# Plan 0007 — The Client Application: GPUI Kit Renderer and Shell
## In scope

- **0001 — Adoption and Engine Bridge.**
- **0002 — Terminal Rendering.**
- **0003 — Application Chrome.**
- **0004 — Command Palette.**
- **0005 — Application Platform.**
- **0006 — Proof and Handoff.**

# Scope — The Client Application: GPUI Kit Renderer and Shell

> Build the client this whole system was built for: a GPU-rendered, cross-platform terminal application on GPUI Kit — command palette over everything with explanations, tabs at the top, sessions at the bottom, and one custom component: the terminal grid fed by `libghostty-vt`.

## Why this plan

Plans 0001 through 0006 built a complete remote terminal system with no way to see it: a daemon that owns panes and sessions, a multiplexer that carries every pane over one link, a client engine that bootstraps hosts over SSH and resumes without losing a byte, and a C ABI with a written contract. What is missing is the thing a user runs. The decision this plan records is **full-in on GPUI Kit** (`gpui-kit` 0.6.1, the Longbridge `gpui-component` line on crates.io) rather than a hand-rolled chrome layer over `winit`/`wgpu` or egui: it is the only option where the chrome this product wants — command palette, tab bars, dockable splits, text input with IME already solved — exists as maintained, production-proven components, leaving exactly one component to write by hand.

That one component is the load-bearing one and it is the project's own: a terminal grid element that reads cell state from `libghostty-vt` — the same emulator the server's mirrors run — and paints it through GPUI. The engine keeps answering queries with the application's real theme colors, keeps encoding input according to the pane's live terminal mode, and keeps the shared-engine property that makes the server's `Screen` resynchronization exact. Nothing about adopting GPUI touches `libghostty-vt`; the emulator layer and the framework layer meet only inside the grid element this plan writes.

The application joins this workspace as a crate. The original architecture placed the application in its own repository in Swift; the user has chosen a cross-platform Rust client on GPUI instead, and keeping it in-workspace means one gate (`cargo xtask check`) covers it, the dependency allowlist catches its tree, and the lexicon holds its names. The architecture amendment in the handoff workstream makes that decision official rather than letting the documents drift.

## In scope

- **Adoption.** The `iznik-app` crate scaffolded into the workspace with `gpui-kit` pinned exact, the dependency allowlist and lexicon entries, a windowed skeleton, and — before anything builds on it — proof that a GPUI application crate passes this workspace's gates headlessly on the Linux development machine, via GPUI's own test context.
- **The engine bridge.** A tokio runtime owning `HostManager`, an event channel bridged into GPUI entities, host lifecycle surfaced (add, remove, reconnect, upgrade, uninstall), and the client model rendered as chrome state.
- **Terminal rendering.** The client-side emulator service (one dedicated thread, the mirror-thread pattern the server already proves), and the custom grid element: glyph pipeline, damage tracking, scrollback viewport, selection, IME preedit, credit returned as bytes are consumed, query responses forwarded as input.
- **Chrome.** The window shell, tab bar at the top, session bar at the bottom, split dividers with drag-resize, driven entirely by the model's deltas.
- **The command palette.** GPUI actions forming a closed inventory over the protocol's session commands and the `HostManager` methods, explanations lifted from the same documentation the workspace mandates, availability predicates, keybindings derived from the same action set, and the palette overlay on top.
- **Platform.** Settings and theme (a struct of colors, not a system), packaging as a macOS `.app` bundle and a Linux binary, and the end-to-end headless proof that the application attaches, types, resizes, drops and resumes against the real in-process stack.

## Out of scope

Signing, notarization and distribution automation stay out: the checklist records them as manual steps on a Mac, exactly as the Darwin artifacts are handled today. Predictive local echo stays excluded — `docs/CLIENT.md` reserves its shape and this plan does not spend it. Scrollback search, remote-pairing UX beyond what the protocol already carries, mobile targets, plugin surfaces, and any theme *system* beyond a theme struct stay out. The `iznik` CLI stays structured text and never gains a screen. The emulator stays `libghostty-vt`; gpui-component's own terminal view (backed by alacritty's emulator) is studied for its damage and IME handling but never adopted, because two emulators in one client is the second-implementation bug the fidelity suite exists to prevent. Rendering proofs that need a display are recorded as deferred, following the `darwin-artifacts` pattern, rather than as proven.
