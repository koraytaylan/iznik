//! Every crate root but `iznik-ffi`'s forbids unsafe code: `iznik-ffi` is the
//! C boundary and the one crate where `unsafe` exists, and a binary's
//! `main.rs` is a crate root too.

use std::path::Path;

use super::{CRATE_ROOTS, PolicyError, Violation, read, relative, workspace_members};

/// The attribute every crate root but the boundary's carries.
const FORBID: &str = "#![forbid(unsafe_code)]";

/// The one crate root allowed to omit it, relative to the root.
const BOUNDARY: &str = "crates/iznik-ffi/src/lib.rs";

/// The rule a crate root without the attribute breaks.
const RULE: &str = "unsafe-boundary";

/// The unsafe boundary check.
///
/// # Errors
///
/// [`PolicyError`] when the root manifest or a crate root cannot be read.
pub fn check(root: &Path) -> Result<Vec<Violation>, PolicyError> {
    let mut violations = Vec::new();
    let boundary = root.join(BOUNDARY);
    for member in workspace_members(root)? {
        for crate_root in CRATE_ROOTS {
            let path = member.join(crate_root);
            if !path.exists() || path == boundary {
                continue;
            }
            if !read(&path)?.contains(FORBID) {
                violations.push(Violation {
                    path: relative(root, &path),
                    line: None,
                    rule: RULE,
                    detail: format!("the crate root does not carry `{FORBID}`"),
                });
            }
        }
    }
    Ok(violations)
}
