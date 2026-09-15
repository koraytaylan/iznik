---
id: architecture-amendment
title: "Amend the architecture: the client application is this workspace's product"
workstream: "0006"
kind: chore
depends_on:
  - app-end-to-end
gated: false
touches:
  - ARCHITECTURE.md
  - README.md
  - docs/CLIENT.md
  - policy/lexicon/architecture-amendment.txt
status: planned
merged_as: ""
---
# Amend the architecture: the client application is this workspace's product

Rewrite the documents that name a Swift macOS application in another repository, so the architecture names the cross-platform GPUI client built by this plan — the decision recorded once, in its own commit, rather than drifted.

**Steps:**

1. Amend `ARCHITECTURE.md`: §1 names the client application (GPUI on `iznik-client`) as the one user interface, cross-platform, macOS first; the non-goals keep "no terminal user interface in the CLI" and update "no layout engine on the server" wording if needed; §2's topology names the application crate; §3's crate table gains `iznik-app` with its dependency line.
2. Amend the root README: the build-a-MacOS-application section becomes build-the-application, keeping the contract pointer (`docs/CLIENT.md` remains the boundary for other front ends) and documenting `cargo xtask app-bundle`.
3. Amend `docs/CLIENT.md` only where it addresses the Swift developer by assumption: the contract is unchanged, its audience sentence now names the GPUI application as its first reader and any other front end as equally bound.
4. Add `policy/lexicon/architecture-amendment.txt` for any new word the amendments introduce.

**Tests:**

- Every relative link added or moved resolves (`policy_links`).
- No document names a command the binaries do not answer (`readme_commands`).
- The architecture, the README and the contract agree on where the application lives and what the C ABI is for.

- **Done when:** `timeout 900 cargo nextest run --package xtask --test policy_links --test readme_commands` passes.
