//! Scratch paths for tests, unique to the process and to the call.
//!
//! A fixed name under the temporary directory is shared by every run of the
//! test at once: two worktrees, two `cargo test` invocations, or two tests of
//! one binary running on libtest's threads all read and remove each other's
//! files. The process id separates processes; a counter separates the calls
//! inside one, so two tests of the same binary never meet either. The name is
//! kept short, because some of these directories hold a Unix socket, whose
//! path has a length limit of about a hundred bytes.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// How many scratch paths this process has handed out.
static HANDED_OUT: AtomicU64 = AtomicU64::new(0);

/// A path under the temporary directory, named for `label`, that no other
/// process and no other call in this one is given. Nothing is created: the
/// caller makes a file or a directory there, and removes it.
#[must_use]
pub fn path(label: &str) -> PathBuf {
    let ordinal = HANDED_OUT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("iznik-{label}-{}-{ordinal}", std::process::id()))
}
