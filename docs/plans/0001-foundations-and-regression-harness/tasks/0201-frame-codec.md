---
id: frame-codec
title: "Frame Codec"
workstream: "0002"
kind: task
depends_on:
  - workspace-scaffold
gated: false
touches:
  - "crates/iznik-protocol/src/frame.rs"
  - "crates/iznik-protocol/tests/frame_golden.rs"
  - "crates/iznik-protocol/tests/fixtures/frame.jsonl"
  - "crates/iznik-testkit/src/golden.rs"
  - "policy/lexicon/frame-codec.txt"
status: planned
merged_as: ""
---
# Frame Codec

Every byte between client and server travels in a frame: a little-endian length, a channel, a payload. The decoder must yield identical frames however the underlying stream is split, because a TCP read, an SSH channel and a pseudoterminal each split wherever they like. The golden fixture is the contract; the code is held to it.

**Steps:**

1. Author `crates/iznik-protocol/tests/fixtures/frame.jsonl` first: one line per case with `description`, `channel`, `payload_hex` and `frame_hex`, covering an empty payload, a one-byte payload, the control channel, channel 255, a payload of exactly `MAXIMUM_PAYLOAD_LENGTH`, and one byte over it.
2. Implement `iznik_testkit::golden::lines(path) -> Result<Vec<serde_json::Value>, GoldenError>`, the one JSONL loader every golden test in the workspace uses, with an error that names the file and the line that failed to parse.
3. Implement `crates/iznik-protocol/src/frame.rs` — `MAXIMUM_PAYLOAD_LENGTH`, `HEADER_LENGTH`, `FrameHeader`, `encode`, `FrameDecoder`, `Frame`, `FrameError` — exactly as the architecture's `frame-codec` section specifies.
4. Write `crates/iznik-protocol/tests/frame_golden.rs` asserting the fixture in both directions and the split-resumption property.

**Tests:**

- Every fixture line encodes to exactly `frame_hex` and decodes from it to exactly `channel` and `payload_hex`, with a failure naming the case's `description`.
- Split resumption: for a stream of several fixture frames, feeding it to a decoder in every possible split into two pushes, and in one-byte pushes, yields the identical sequence of frames.
- Oversize: a header whose length exceeds the maximum yields `FrameError::Oversize` carrying that length and does not allocate the payload.
- Need more bytes: a partial header and a partial payload each yield `Ok(None)` and then the frame once the rest arrives.
- Compaction: after many frames the decoder's buffer does not grow without bound, asserted by capacity after a long stream.
- The golden loader reports a malformed line with its file and line number.

- **Done when:** `timeout 600 cargo nextest run --package iznik-protocol --test frame_golden` passes every case above and `timeout 3600 cargo xtask check` succeeds.
