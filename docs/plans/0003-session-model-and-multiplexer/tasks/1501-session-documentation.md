---
id: session-documentation
title: "Session Documentation"
workstream: "0015"
kind: task
depends_on:
  - session-commands
  - multiplexer-assembly
  - stream-compression
gated: false
touches:
  - ARCHITECTURE.md
  - "crates/iznik-protocol/README.md"
  - "crates/iznik-link/README.md"
  - "crates/iznik-server/README.md"
  - "docs/notes/protocol.md"
  - "crates/iznik-protocol/tests/protocol_reference.rs"
  - "policy/lexicon/session-documentation.txt"
status: done
merged_as: ""
---
# Session Documentation

The macOS repository will read one document to speak to this server. This task writes it from the golden fixtures — which remain the arbiter — and brings the crate documentation and the root architecture into line with what landed.

**Steps:**

1. Write `docs/notes/protocol.md`: framing; every control message with its discriminant and byte layout, error codes and mark kinds included; the model, delta and command encodings; sequence semantics; the subscribe, resume and screen rules; the credit protocol; compression negotiation and the leftover-bytes rule — each layout cross-checked against a line of the fixtures it describes.
2. Write `crates/iznik-protocol/tests/protocol_reference.rs`, which parses the reference and the source and asserts every discriminant agrees.
3. Update the `iznik-protocol`, `iznik-link` and `iznik-server` READMEs to document every module, and the root `ARCHITECTURE.md` §4 and §5.2–5.6 to say what is true, including the measured compression numbers and the keystroke latency figure.

**Tests:**

- The documentation and links policy checks pass.
- Every discriminant value in `docs/notes/protocol.md` equals the constant of the same name in `crates/iznik-protocol/src/message.rs`, asserted by a test that parses both.

- **Done when:** `timeout 3600 cargo xtask check` succeeds and `timeout 600 cargo nextest run --package iznik-protocol -E 'test(protocol_reference)'` proves every discriminant in `docs/notes/protocol.md` matches the source.
