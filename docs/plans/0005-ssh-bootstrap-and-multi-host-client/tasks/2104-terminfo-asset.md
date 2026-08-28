---
id: terminfo-asset
title: "Terminfo Asset"
workstream: "0021"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-client/assets/xterm-ghostty.terminfo"
  - "crates/iznik-client/src/bootstrap/terminfo.rs"
  - "crates/iznik-client/tests/terminfo_asset.rs"
  - "regression/claims/terminfo-asset.toml"
  - "policy/lexicon/terminfo-asset.txt"
status: done
merged_as: ""
---
# Terminfo Asset

The terminal the application renders with is ghostty, so the programs on the remote host should be told exactly that. This task commits the `xterm-ghostty` terminfo source the bootstrap installs, with its origin recorded, so `TERM` on a pane is the truth rather than the nearest lie.

**Steps:**

1. Obtain the terminfo source with `infocmp -x xterm-ghostty` from this machine's ncurses terminfo database, which ships the entry from ncurses 6.5 on, and commit it as `crates/iznik-client/assets/xterm-ghostty.terminfo`.
2. Implement `crates/iznik-client/src/bootstrap/terminfo.rs` exposing it as `XTERM_GHOSTTY_TERMINFO`, with the ncurses version, the machine and the date recorded in the module documentation.
3. Write `crates/iznik-client/tests/terminfo_asset.rs`, and declare this task's claims in `regression/claims/terminfo-asset.toml` as `test` proofs whose `because` says `tic` on this machine is the same `tic` the host image carries.

**Tests:**

- `tic -x -o <temporary directory> <asset>` exits 0 with no warnings.
- `infocmp -x` against the compiled entry reports `xterm-ghostty` and advertises 24-bit color and styled underlines.
- The asset's first line names `xterm-ghostty`.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test terminfo_asset` passes every case above, `timeout 900 cargo xtask claims verify --task terminfo-asset` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
