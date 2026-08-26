//! Every scenario under `regression/scenarios/` as one nextest test named `scenario::<task-id>::<name>`, over `libtest-mimic`.
//!
//! Filled by task `scenario-driver` of plan 0001; until then the binary registers no test, and it exists because a `[[test]]` target the manifest declares must have its file.

/// The test binary's entry point: a stub until task `scenario-driver` replaces its body.
fn main() {}
