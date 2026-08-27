---
id: shell-integration-marks
title: "Shell Integration Marks"
workstream: "0007"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-server/src/terminal/marks.rs"
  - "crates/iznik-server/tests/shell_integration_marks.rs"
  - "crates/iznik-testkit/assets/shell-integration.bash"
  - "regression/claims/shell-integration-marks.toml"
  - "policy/lexicon/shell-integration-marks.txt"
status: done
merged_as: ""
---
# Shell Integration Marks

Prompt marks, working directories, titles and alternate-screen switches become structured events as the bytes pass — never by polling, never by touching the bytes. This is the payoff a native client has over a terminal user interface: jump to the previous command, fold a command's output, badge its exit status, name a tab after its directory. It handles a sequence split across reads at any byte, which is the bug every naive implementation of this has, and it interprets OSC payloads with ghostty's own parser so there is one grammar.

**Steps:**

1. Write `crates/iznik-testkit/assets/shell-integration.bash` — the minimal rc file the architecture describes — and check with `bash --rcfile` on a pseudoterminal that it emits the four marks and the directory report around one command.
2. Implement `crates/iznik-server/src/terminal/marks.rs` — `MarkObserver`, `MarkEvent`, `MAXIMUM_OSC_LENGTH`, the framing scan, and the hand-off of completed OSC payloads to `libghostty_vt::osc::Parser` — exactly as the architecture's `shell-integration-marks` section specifies, emitting `iznik_protocol::message::MarkKind`.
3. Write `crates/iznik-server/tests/shell_integration_marks.rs`.
4. Declare this task's claims in `regression/claims/shell-integration-marks.toml` as `test` proofs with their `because`.

**Tests:**

- Each of OSC 133 `A`, `B`, `C` and `D;<status>`, OSC 7 with a host and a path, OSC 0 and OSC 2 titles terminated by BEL and by ST, and each of the three alternate-screen entry and exit sequences yields exactly one event of the right kind with `sequence` equal to the absolute position of the sequence's first byte and `length` its byte length.
- Every split: a stream containing every mark, split into two chunks at every byte position, yields the identical event list.
- Pass-through: the observer never returns or alters bytes and emits its events synchronously with the chunk that completes them.
- Oversize: an OSC longer than `MAXIMUM_OSC_LENGTH` yields no event, and the next well-formed mark after it is still recognized.
- Noise: a `D` without a status, an OSC 7 that is not a `file://` URL, an unrelated OSC and a private-mode CSI other than the three switches are ignored without error.
- The asset: `bash --rcfile` with the asset on a pseudoterminal, given one command, produces exactly `PromptStart`, `CommandStart`, `CommandExecuted`, `CommandFinished { Some(0) }` and one `WorkingDirectory` through the observer.

- **Done when:** `timeout 600 cargo nextest run --package iznik-server --test shell_integration_marks` passes every case above, `timeout 900 cargo xtask claims verify --task shell-integration-marks` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
