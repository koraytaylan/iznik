//! The tree every policy check walks is this checkout's own: not the build
//! output, not the agents' and Makina's worktrees kept inside it, and not any
//! nested checkout, each of which is another tree with its own fixtures.

use std::fs;
use std::path::Path;

use xtask::policy::files_under;
use xtask::policy::links;

/// Writes `contents` at `relative` under `root`, making its parents.
///
/// # Errors
///
/// When a directory or the file cannot be written.
fn put(root: &Path, relative: &str, contents: &str) -> std::io::Result<()> {
    let path = root.join(relative);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, contents)
}

/// Files under `.claude`, `.worktrees`, `target` and a directory holding its
/// own `.git` — directory or worktree file — are not walked; everything else
/// is, and a broken link in the skipped trees is not reported.
///
/// # Panics
///
/// When a skipped file is listed, a kept one is not, or a skipped link is
/// reported.
#[test]
fn policy_walk_skips_other_checkouts_and_build_output() {
    let root = iznik_testkit::scratch::path("policy-walk");
    let broken = "[gone](missing.md)\n";
    put(&root, "README.md", "# kept\n").unwrap();
    put(&root, "docs/kept.md", "# kept\n").unwrap();
    put(&root, ".claude/worktrees/agent/README.md", broken).unwrap();
    put(&root, ".worktrees/task/README.md", broken).unwrap();
    put(&root, "target/doc/README.md", broken).unwrap();
    put(&root, "vendor/nested/.git/HEAD", "ref: refs/heads/main\n").unwrap();
    put(&root, "vendor/nested/README.md", broken).unwrap();
    put(&root, "vendor/worktree/.git", "gitdir: /elsewhere\n").unwrap();
    put(&root, "vendor/worktree/README.md", broken).unwrap();

    let walked: Vec<String> = files_under(&root, &root)
        .unwrap()
        .iter()
        .map(|path| path.strip_prefix(&root).unwrap().display().to_string())
        .collect();
    assert_eq!(walked, vec!["README.md", "docs/kept.md"]);

    let violations = links::check(&root).unwrap();
    assert!(violations.is_empty(), "{violations:?}");
    let _removed = fs::remove_dir_all(&root);
}
