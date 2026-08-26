---
id: framed-link
title: "Framed Link"
workstream: "0002"
kind: task
depends_on:
  - frame-codec
gated: false
touches:
  - "crates/iznik-link/src/framed.rs"
  - "crates/iznik-link/tests/framed.rs"
  - "policy/lexicon/framed-link.txt"
status: done
merged_as: ""
---
# Framed Link

Frames over a duplex byte stream — a unix socket, an SSH child's standard streams, an in-memory pipe — written once, in one crate both product ends and the test client depend on, so the sentence "the two copies are held to the same corpus" never has to be written. A pane byte is copied from the caller's buffer to the socket and nowhere else.

**Steps:**

1. Implement `crates/iznik-link/src/framed.rs` — `FramedLink`, `send`, `next`, `split`, `FrameReader`, `FrameWriter`, `into_parts`, `LinkError` — exactly as the architecture's `framed-link` section specifies, over `tokio::io::AsyncRead + AsyncWrite` with a vectored write per frame and a `FrameDecoder` on the read side.
2. Write `crates/iznik-link/tests/framed.rs` over `tokio::io::duplex`.

**Tests:**

- Round trip: frames on every channel, including an empty payload and one of `MAXIMUM_PAYLOAD_LENGTH`, arrive with the same channel and payload.
- Split resumption: with the duplex buffer sized so reads split at every boundary of a multi-frame stream, `next` yields the identical frames.
- One write per frame: `send` issues exactly one vectored write, asserted with a counting stream wrapper, and never copies the payload into an intermediate buffer, asserted by pointer identity of the slice handed to the write.
- End of stream: after the peer closes, `next` yields `Ok(None)` after the last complete frame; a stream closed mid-frame yields `Closed`.
- Oversize: a header over the maximum yields `Frame(Oversize)` before any payload is read.
- Split halves: after `split`, one task sending a thousand frames while another receives them yields every frame, and dropping the writer ends the reader with `Ok(None)`.
- `into_parts` after a read that captured one and a half frames returns the stream and exactly the half frame's bytes, and a new link built from those parts yields the second frame whole.

- **Done when:** `timeout 600 cargo nextest run --package iznik-link --test framed` passes every case above and `timeout 3600 cargo xtask check` succeeds.
