---
id: static-library-and-header
title: "Static Library and Header"
workstream: "0028"
kind: task
depends_on:
  - pane-byte-pipe
gated: false
touches:
  - cbindgen.toml
  - "include/iznik.h"
  - "xtask/src/header.rs"
  - "xtask/tests/header_golden.rs"
  - "xtask/tests/regression_ffi_smoke.rs"
  - "xtask/tests/fixtures/ffi/**"
  - "regression/claims/static-library-and-header.toml"
  - "policy/lexicon/static-library-and-header.txt"
status: done
merged_as: ""
---
# Static Library and Header

A client crashing in someone else's Xcode because a struct grew a field is the failure this task prevents: the header is generated, golden-pinned, and exercised from a C program against a real local daemon, so the ABI is proven with C rather than promised.

**Steps:**

1. Write `cbindgen.toml` and implement `xtask::header` — generate `include/iznik.h` from `iznik-ffi` — and fill the `xtask header` subcommand; commit the generated header as the golden.
2. Write `xtask/tests/fixtures/ffi/smoke.c` performing the sequence the architecture's `static-library-and-header` section lists against the `unix:` alias it receives as its argument, and `xtask/tests/regression_ffi_smoke.rs`, `#[ignore]`, which builds the library under the `regression` profile, reads the link line from `--print native-static-libs`, compiles the program with the system C compiler under a deadline, and runs it against an in-process `Stack`.
3. Write `xtask/tests/header_golden.rs`, and declare this task's claims in `regression/claims/static-library-and-header.toml` as `test` proofs with their `because`.

**Tests:**

- Golden: the generated header equals `include/iznik.h` byte for byte; a deliberate signature change in a synthetic copy of the crate produces a differing header and the test names the first differing line.
- The header compiles standalone as C11 with `-Wall -Wextra -Werror` and as C++ with `-x c++`.
- Every `extern "C"` function in `iznik-ffi` appears in the header and the reverse; every function with an obligation carries an `Obligation:` line.
- Smoke: `smoke.c` compiled against the header and `libiznik.a` runs the full sequence against a local daemon and exits 0, receiving the typed line through the output callback, in under ten seconds beyond the build.
- Symbols: `libiznik.so` exports only `iznik_` names, and `libiznik.a` defines every `iznik_` function the header declares, both asserted with `nm`.

- **Done when:** `timeout 600 cargo nextest run --package xtask --test header_golden` and `timeout 1200 cargo nextest run --package xtask --test regression_ffi_smoke --run-ignored all` pass every case above, `timeout 1200 cargo xtask claims verify --task static-library-and-header` reports every claim proven, and `timeout 3600 cargo xtask check` succeeds.
