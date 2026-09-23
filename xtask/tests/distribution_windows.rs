//! The Windows triples are refused, with the reason, and leave nothing behind.
//!
//! A Windows server is not distributed: its daemon listens on unauthenticated
//! loopback TCP and the client cannot start it. These cases hold
//! `xtask distribution` to saying so rather than building one, or calling the
//! triple unknown.

use std::process::Command;

use xtask::distribution::windows::{REFUSAL, TARGETS};
use xtask::distribution::{
    BINARY, DISTRIBUTION_DIRECTORY, DistributionError, build, target_directory, workspace_root,
};

/// Every Windows triple is refused with the reason, not built and not called
/// unknown, and no artifact is left where one would have been written.
///
/// # Panics
///
/// When a triple is built, called unknown, or leaves a file behind.
#[test]
fn every_windows_triple_is_refused_with_the_reason() {
    let root = workspace_root();
    assert!(!TARGETS.is_empty(), "the Windows triples are still named");
    for target in TARGETS {
        match build(&root, target) {
            Err(DistributionError::Build {
                target: refused,
                detail,
            }) => {
                assert_eq!(refused, *target);
                assert_eq!(detail, REFUSAL);
            }
            other => panic!("{target}: expected the refusal, got {other:?}"),
        }
        let placed = target_directory(&root)
            .join(DISTRIBUTION_DIRECTORY)
            .join(target)
            .join(BINARY);
        assert!(
            !placed.exists(),
            "{target}: nothing is left at {}",
            placed.display()
        );
    }
}

/// The refusal names both reasons a person would need to change it.
///
/// # Panics
///
/// When either reason is missing.
#[test]
fn the_refusal_names_the_listener_and_the_launch() {
    assert!(REFUSAL.contains("loopback TCP without authentication"));
    assert!(REFUSAL.contains("POSIX shell"));
    assert!(REFUSAL.contains("Windows client itself is still built"));
}

/// The command a person runs fails, prints the refusal, and does not offer
/// the Windows triples in its usage line.
///
/// # Panics
///
/// When the command succeeds, is silent about why, or offers Windows.
#[test]
fn the_command_refuses_and_does_not_offer_windows() {
    let refused = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["distribution", "--target", "x86_64-pc-windows-msvc"])
        .output()
        .unwrap();
    assert!(!refused.status.success());
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(said.contains(REFUSAL), "stderr: {said}");

    let usage = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["distribution"])
        .output()
        .unwrap();
    let offered = String::from_utf8_lossy(&usage.stderr);
    assert!(
        offered.contains("x86_64-unknown-linux-musl"),
        "stderr: {offered}"
    );
    assert!(!offered.contains("windows"), "stderr: {offered}");
}
