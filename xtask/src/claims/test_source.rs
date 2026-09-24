//! Whether a test target's source defines a test function: a proof naming a
//! function that was renamed or removed would otherwise select nothing, and
//! be reported only as a missing verdict after a whole run.
//!
//! It is a scan, not a parse. The target's root file is read, and so is every
//! module it pulls in by `mod <name>;` or through `#[path = "…"]`, and the
//! function counts as defined when one of them holds `fn <name>(` or
//! `fn <name><`. That is enough for the tests this repository writes by hand;
//! a target whose tests a macro generates would need its proofs named another
//! way.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// What introduces the path of the module on the line after it.
const PATH_ATTRIBUTE: &str = "#[path = \"";

/// What separates the module path of a test from its function.
const MODULE_SEPARATOR: &str = "::";

/// Whether the test target whose root is `target_root` defines the function the
/// test path `test` ends in.
#[must_use]
pub fn defines_test(target_root: &Path, test: &str) -> bool {
    let function = test.rsplit(MODULE_SEPARATOR).next().unwrap_or(test);
    if function.is_empty() {
        return false;
    }
    let opening = format!("fn {function}(");
    let generic = format!("fn {function}<");
    let mut seen = BTreeSet::new();
    let mut waiting = vec![(target_root.to_path_buf(), true)];
    while let Some((source, is_root)) = waiting.pop() {
        if !seen.insert(source.clone()) {
            continue;
        }
        let Ok(contents) = std::fs::read_to_string(&source) else {
            continue;
        };
        if contents.contains(&opening) || contents.contains(&generic) {
            return true;
        }
        waiting.extend(
            modules_of(&source, &contents, is_root)
                .into_iter()
                .map(|module| (module, false)),
        );
    }
    false
}

/// The files of the modules `text`, the contents of `file`, declares without
/// a body: the one a `#[path]` before it names, or the one cargo's layout puts
/// it in.
fn modules_of(file: &Path, text: &str, is_root: bool) -> Vec<PathBuf> {
    let parent = file.parent().unwrap_or_else(|| Path::new(""));
    let owns_directory = is_root
        || file
            .file_name()
            .is_some_and(|name| name == "mod.rs" || name == "main.rs");
    let children = if owns_directory {
        parent.to_path_buf()
    } else {
        parent.join(file.file_stem().unwrap_or_default())
    };
    let mut named = None;
    let mut found = Vec::new();
    for line in text.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix(PATH_ATTRIBUTE) {
            named = rest.split('"').next().map(|path| parent.join(path));
            continue;
        }
        let declaration = line.strip_prefix("pub ").unwrap_or(line);
        let Some(name) = declaration
            .strip_prefix("mod ")
            .and_then(|rest| rest.strip_suffix(';'))
        else {
            if !line.starts_with("#[") {
                named = None;
            }
            continue;
        };
        if let Some(path) = named.take() {
            found.push(path);
        } else {
            found.push(children.join(format!("{name}.rs")));
            found.push(children.join(name).join("mod.rs"));
        }
    }
    found
}
