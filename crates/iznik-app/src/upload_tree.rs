//! A pasted path expanded into the files and directories underneath it.
//!
//! A directory is listed depth-first, parents before their children, each
//! level by name. A symbolic link is refused rather than followed, so a
//! paste cannot walk out of the tree it named or loop. The name a host is
//! given uses `/` between components, whatever this machine uses.

use std::path::{Path, PathBuf};

/// One file or directory a paste will send.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The path on this machine.
    pub local: PathBuf,
    /// The name relative to the pane's directory. A directory ends in `/`.
    pub name: String,
    /// Whether this creates a directory and sends no file bytes.
    pub directory: bool,
    /// Whether the path the host gives back is typed into the pane.
    ///
    /// Set for a path the person pasted, and clear for what was found inside
    /// a directory, so a folder types its own path once.
    pub type_path: bool,
}

/// The files and directories `paths` expands to, in the order they are sent.
///
/// # Errors
///
/// A string a person can be shown when a path is missing, is a link, is not
/// a file or a directory, or repeats a name.
pub fn collect(paths: &[PathBuf]) -> Result<Vec<Item>, String> {
    if paths.is_empty() {
        return Err("the clipboard has no file".to_owned());
    }
    let mut items = Vec::new();
    let mut names = std::collections::HashSet::new();
    for path in paths {
        let name = file_name(path)?;
        if !names.insert(name.clone()) {
            return Err(format!("two pasted files are both named {name}"));
        }
        push(path, &name, true, &mut items)?;
    }
    Ok(items)
}

/// Adds `path` under `name`, then everything inside it when it is a directory.
///
/// # Errors
///
/// A string a person can be shown when `path` cannot be listed.
fn push(path: &Path, name: &str, type_path: bool, items: &mut Vec<Item>) -> Result<(), String> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(format!(
            "{} is a link, and a link is not uploaded",
            path.display()
        ));
    }
    if metadata.is_dir() {
        items.push(Item {
            local: path.to_path_buf(),
            name: format!("{name}/"),
            directory: true,
            type_path,
        });
        for child in children(path)? {
            let child_name = file_name(&child)?;
            let nested = format!("{name}/{child_name}");
            push(&child, &nested, false, items)?;
        }
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    items.push(Item {
        local: path.to_path_buf(),
        name: name.to_owned(),
        directory: false,
        type_path,
    });
    Ok(())
}

/// The entries of a directory, by name.
///
/// # Errors
///
/// A string a person can be shown when the directory cannot be listed, or an
/// entry has no name this host can use.
fn children(path: &Path) -> Result<Vec<PathBuf>, String> {
    let mut found = Vec::new();
    let listing = std::fs::read_dir(path)
        .map_err(|error| format!("{} could not be read: {error}", path.display()))?;
    for entry in listing {
        let entry =
            entry.map_err(|error| format!("{} could not be read: {error}", path.display()))?;
        found.push(entry.path());
    }
    found.sort_by(|left, right| {
        let left_name = file_name(left).unwrap_or_default();
        let right_name = file_name(right).unwrap_or_default();
        left_name.cmp(&right_name)
    });
    Ok(found)
}

/// The single component `path` will be given on the host.
///
/// # Errors
///
/// A string a person can be shown when the path has no usable file name.
fn file_name(path: &Path) -> Result<String, String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && !name.contains('/') && !name.contains('\\'))
        .map(str::to_owned)
        .ok_or_else(|| format!("{} has no file name the host can use", path.display()))
}
