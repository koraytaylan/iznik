//! `cargo app`: which servers it builds before running the application.

use std::ffi::OsString;

use xtask::distribution::launch::{native_server, servers};

/// The arguments as the dispatcher hands them over, the subcommand first.
fn arguments(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("app")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// With no flag it builds this machine's own musl server; every `--target`
/// replaces that with the servers named; anything else is refused.
///
/// # Panics
///
/// When a list of servers differs.
#[test]
fn app_builds_the_native_server_by_default() {
    assert_eq!(servers(&arguments(&[])), Some(vec![native_server()]));
    assert!(native_server().ends_with("-unknown-linux-musl"));
    assert_eq!(
        servers(&arguments(&[
            "--target",
            "aarch64-unknown-linux-musl",
            "--target",
            "x86_64-unknown-linux-musl"
        ])),
        Some(vec![
            "aarch64-unknown-linux-musl".to_owned(),
            "x86_64-unknown-linux-musl".to_owned()
        ])
    );
    assert_eq!(servers(&arguments(&["--target"])), None);
    assert_eq!(servers(&arguments(&["--release"])), None);
}
