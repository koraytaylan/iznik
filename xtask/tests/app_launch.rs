//! `cargo app`: which servers it builds before running the application.

use std::ffi::OsString;

use xtask::distribution::launch::{default_servers, servers};

/// The arguments as the dispatcher hands them over, the subcommand first.
fn arguments(rest: &[&str]) -> Vec<OsString> {
    std::iter::once("app")
        .chain(rest.iter().copied())
        .map(OsString::from)
        .collect()
}

/// With no flag it builds a server for every Linux architecture — not just
/// this machine's, because a host is whatever it is and a server for the wrong
/// architecture is one the application will refuse to install. Every
/// `--target` replaces that with the servers named; anything else is refused.
///
/// # Panics
///
/// When a list of servers differs.
#[test]
fn app_builds_every_linux_server_by_default() {
    assert_eq!(servers(&arguments(&[])), Some(default_servers()));
    assert_eq!(
        default_servers(),
        vec![
            "x86_64-unknown-linux-musl".to_owned(),
            "aarch64-unknown-linux-musl".to_owned()
        ],
        "both hosts an ssh connection meets are prepared for"
    );
    assert_eq!(
        servers(&arguments(&["--target", "aarch64-unknown-linux-musl"])),
        Some(vec!["aarch64-unknown-linux-musl".to_owned()]),
        "one --target narrows the build to that one"
    );
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
