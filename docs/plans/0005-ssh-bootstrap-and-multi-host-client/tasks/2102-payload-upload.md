---
id: payload-upload
title: "Payload Upload"
workstream: "0021"
kind: task
depends_on:
  - host-probe
  - terminfo-asset
gated: false
touches:
  - "crates/iznik-client/src/bootstrap/upload.rs"
  - "crates/iznik-client/tests/payload_upload.rs"
  - "crates/iznik-regression/src/step/upload.rs"
  - "regression/claims/payload-upload.toml"
  - "regression/scenarios/payload-upload/**"
  - "policy/lexicon/payload-upload.txt"
status: planned
merged_as: ""
---
# Payload Upload

The server binary and the terminfo, over the same channel, verified before they are installed and installed atomically, so a dropped link mid-upload leaves no partially written binary that a later run might execute. The bootstrap verifies a digest it computed itself; no manifest is trusted.

**Steps:**

1. Implement `crates/iznik-client/src/bootstrap/upload.rs` — `Artifact`, `ArtifactSet` with digests computed on load, `REMOTE_UPLOAD_SCRIPT`, `upload`, `Installed`, `UploadError`, chunked reading, digest verification by whichever tool the probed operating system provides, stale-partial cleanup, `fsync` and rename, and terminfo compilation when `tic` is available — exactly as the architecture's `payload-upload` section specifies.
2. Implement `crates/iznik-regression/src/step/upload.rs` — `[steps.upload]` with `alias`, `artifacts` and its `expect`.
3. Write `crates/iznik-client/tests/payload_upload.rs` for `ArtifactSet` loading and the script's construction, and the scenarios under `regression/scenarios/payload-upload/` — `installs`, `verifies-the-digest` (a `run` step on the engine pipes a payload whose digest does not match into `ssh host0 sh -c "$REMOTE_UPLOAD_SCRIPT"`), `dropped-mid-upload` (a `run` step on `host0` starts the script reading from a FIFO in the background and writes its pid to a file, a second step writes half the payload, a `kill-process` fault ends it, and the next `upload` succeeds), `installs-terminfo` — from the engine against `/iznik/distribution`.
4. Declare this task's claims in `regression/claims/payload-upload.toml`.

**Tests:**

- `ArtifactSet` loads one artifact per triple from a distribution directory with the digest of each file's bytes, and refuses a directory with no artifact for the requested triple naming the triple.
- The script names the digest the client computed, reads its input in `UPLOAD_CHUNK_LENGTH` chunks, and never names the final path before the rename, asserted on the constructed text.
- In the container: after `upload`, `<prefix>/bin/iznik-server` exists, is executable, prints the bundled version, and no `.partial-*` remains; the mismatched digest is refused by the remote check and nothing is installed; the killed upload leaves nothing at the final name and the next `upload` succeeds and cleans the partial; the terminfo compiles into `<prefix>/terminfo` and `infocmp` there reports `xterm-ghostty`.
- Idempotent: `upload` against an already-installed identical binary succeeds and leaves one file.

- **Done when:** `timeout 600 cargo nextest run --package iznik-client --test payload_upload` passes every pure case, `timeout 900 cargo xtask claims verify --task payload-upload` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
