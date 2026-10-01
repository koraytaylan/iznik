---
id: file-upload
title: "Paste a copied file into the pane's directory"
workstream: "0007"
kind: task
depends_on:
  - ssh-config-hosts
gated: false
touches:
  - crates/iznik-app/src/upload.rs
  - crates/iznik-app/tests/upload.rs
  - crates/iznik-client/tests/upload_stop.rs
  - crates/iznik-server/tests/file_upload.rs
  - regression/claims/file-upload.toml
status: done
merged_as: ""
---
# Paste a copied file into the pane's directory

A file copied on the machine running the application is written into the pane's directory when it is pasted, and the path the server gives it is what gets typed. The claims in [file-upload.toml](../../../../regression/claims/file-upload.toml) name the proofs: the bytes and a nested directory land on the host, a path that leaves that directory is refused, a clipboard that holds both a file list and a path string pastes the files, the remote path is quoted for a shell, a dropped link resumes, and the uploads list groups a directory, counts bytes, names a rate, keeps a busy paste when cleared, and remembers a stop for that file alone.

**Done when:** `timeout 600 cargo nextest run --package iznik-server --test file_upload --package iznik-app --test upload --package iznik-client --test upload_stop` passes, and `timeout 120 cargo nextest run --package xtask --test claims` loads the registry.
