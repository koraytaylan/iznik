---
id: fidelity-corpus
title: "Fidelity Corpus"
workstream: "0003"
kind: task
depends_on:
  - workspace-scaffold
gated: false
touches:
  - "crates/iznik-testkit/src/corpus.rs"
  - "crates/iznik-testkit/assets/fidelity-corpus.bin"
  - "crates/iznik-testkit/tests/corpus.rs"
  - "policy/lexicon/fidelity-corpus.txt"
status: planned
merged_as: ""
---
# Fidelity Corpus

The constructs that break naive terminal plumbing, built once and named one by one so that a failure anywhere in six plans says which construct it was. The hand-authored part is committed as a golden because the compression dictionary is trained on these bytes: a silent change to a construct would be a silent change to the wire.

**Steps:**

1. Implement `crates/iznik-testkit/src/corpus.rs` — `Construct`, `constructs()` with every construct the architecture's `fidelity-corpus` section lists, and `generated(seed, length)` — with every non-ASCII byte written as an escape.
2. Commit `crates/iznik-testkit/assets/fidelity-corpus.bin` as the serialized form of `constructs()`, and write `crates/iznik-testkit/tests/corpus.rs`.

**Tests:**

- The golden: serializing `constructs()` yields exactly the committed bytes, and a failure names the first construct that differs.
- Every construct has a distinct name, and the named ones are all present: the Kitty graphics payload, the hyperlink, the clipboard write, the four marks, the working directory, the titles with both terminators, the split CSI, the wide glyph, the lone escape, synchronized output, the alternate-screen round trip, the keyboard-protocol query, the cursor position query.
- Chunk boundaries are part of the data: the split CSI and the lone escape are constructs of two chunks whose boundary falls inside a sequence.
- `generated` is deterministic — the same seed and length yield identical bytes — and produces a different stream for a different seed; its output is printable text with line breaks and no escape byte, so a flood is a flood and not an accidental sequence.
- 64 MiB of generated text is produced in under a second.

- **Done when:** `timeout 600 cargo nextest run --package iznik-testkit --test corpus` passes every case above and `timeout 3600 cargo xtask check` succeeds.
