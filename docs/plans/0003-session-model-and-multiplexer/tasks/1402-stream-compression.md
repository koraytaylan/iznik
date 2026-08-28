---
id: stream-compression
title: "Stream Compression"
workstream: "0014"
kind: task
depends_on: []
gated: false
touches:
  - "crates/iznik-protocol/src/dictionary.rs"
  - "crates/iznik-protocol/assets/compression-dictionary.bin"
  - "crates/iznik-link/src/compression.rs"
  - "crates/iznik-link/tests/compression.rs"
  - "regression/claims/stream-compression.toml"
  - "policy/lexicon/stream-compression.txt"
status: done
merged_as: ""
---
# Stream Compression

Terminal output compresses extremely well and the link is the scarce resource, so this is not premature — but compression that improves throughput while adding perceptible keystroke latency would be a bad trade, and two committed numbers are how that is caught. It lives in `iznik-link`, once, under the framing both ends already share.

**Steps:**

1. Train the dictionary with `zstd --train` on the fidelity corpus golden, commit it as `crates/iznik-protocol/assets/compression-dictionary.bin`, and expose it from `crates/iznik-protocol/src/dictionary.rs` with the training command and corpus hash in the module documentation.
2. Implement `crates/iznik-link/src/compression.rs` — `ZstdStream`, `compressed`, the per-frame flush, `MINIMUM_CORPUS_RATIO`, `SMALL_FRAME_LATENCY_CEILING` — exactly as the architecture's `stream-compression` section specifies.
3. Write `crates/iznik-link/tests/compression.rs`, the latency case named `small_frame_latency`.
4. Declare this task's claims in `regression/claims/stream-compression.toml` as `test` proofs with their `because`.

**Tests:**

- Round trip: every frame of the fidelity corpus through a compressed link pair over `tokio::io::duplex` arrives byte-identical, whatever the read boundaries.
- Leftover: a plain link that read past a `Hello` into the peer's first compressed bytes hands them to `compressed` through `into_parts`, and the first compressed frame arrives whole.
- The ratio: the corpus compresses by at least `MINIMUM_CORPUS_RATIO`, with the measured ratio in the assertion message.
- The latency: a one-byte frame through the pair adds at most `SMALL_FRAME_LATENCY_CEILING` at the 99th percentile over ten thousand samples, with the median and the tail in the assertion message.
- Streaming, not per frame: a thousand one-byte frames compress to fewer bytes than a thousand independent compressions of the same frames.
- Flushed, not buffered: a single frame sent through the pair is readable on the other side without any further frame being sent.

- **Done when:** `timeout 600 cargo nextest run --package iznik-link --test compression` passes every case above with both numbers reported, `timeout 900 cargo xtask claims verify --task stream-compression` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
