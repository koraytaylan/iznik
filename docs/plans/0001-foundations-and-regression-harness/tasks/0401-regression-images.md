---
id: regression-images
title: "Regression Images"
workstream: "0004"
kind: task
depends_on:
  - gate-runner
gated: false
touches:
  - "regression/images/**"
  - "crates/iznik-harness/src/images.rs"
  - "crates/iznik-harness/tests/regression_images.rs"
  - "xtask/src/regression.rs"
  - "policy/lexicon/regression-images.txt"
status: planned
merged_as: ""
---
# Regression Images

The two machines every claim is proven on: a host that is nothing but a root `sshd` and an unprivileged user with a login shell, exactly as a real host is arranged, and an engine as bare as a machine somebody just installed. Neither carries a toolchain, a source tree or an iznik binary, so a broken bootstrap cannot hide behind an image that already had what the bootstrap should have delivered.

**Steps:**

1. Write `regression/images/Containerfile.host` — base pinned by digest; `openssh-server`, `procps`, `ncurses-bin`; the `/run/sshd` privilege-separation directory; `sshd_config` with `UsePAM no`, `PasswordAuthentication no` and `PubkeyAuthentication yes`; user `iznik` (uid 1000, `/bin/bash`, a `.bashrc` that sets `PS1='$ '` and nothing else, and a `.bash_profile` that sources it); an entrypoint that runs `sshd -D -e` as root — and `regression/images/Containerfile.engine` — the same base; `openssh-client` with `ssh-agent` and `ssh-add` deleted after installation; user `iznik` as `USER`; no `~/.ssh` — exactly as the architecture's `regression-images` section specifies.
2. Implement `iznik_harness::images` — `image_tag`, `ensure_images`, `Images`, `ImagesError`, `IMAGE_BUILD_DEADLINE` — building through `iznik_harness::process::run`, and fill the `images` form of the `xtask regression` subcommand.
3. Write `crates/iznik-harness/tests/regression_images.rs`, every test `#[ignore]`.

**Tests:**

- `image_tag` is a pure function of a Containerfile's bytes: the same content yields the same tag, one changed byte yields a different one. This case is not ignored; it needs no podman.
- Building twice is a no-op the second time, asserted by elapsed time under a stated bound and by `podman image inspect` reporting one image per tag.
- The engine image has `ssh` and neither `ssh-agent`, `ssh-add`, `sshd` nor a `~/.ssh` directory, and runs as uid 1000; the host image has `sshd`, `tic`, `infocmp`, `ps` and a `bash` login shell for uid 1000, and its `sshd` starts as root and accepts a connection — each asserted by a command run inside a container from the image.
- A missing `podman` produces `ImagesError` naming the program, and the failure message of every ignored test names it.

- **Done when:** `timeout 900 cargo nextest run --package iznik-harness --test regression_images --run-ignored all` passes every case above and `timeout 3600 cargo xtask check` succeeds.
