//! The reference held to the source: every discriminant `docs/notes/protocol.md`
//! publishes is the constant of that name in this crate, and every constant
//! this crate has is published.
//!
//! A second implementation reads that document and not this code, so a
//! discriminant that moves without the document moving is a wire break nobody
//! would find until two implementations disagreed. Both directions are checked:
//! a value that drifts fails, and so does a new tag nobody wrote down.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use iznik_protocol::frame::{HEADER_LENGTH, MAXIMUM_PAYLOAD_LENGTH};
use iznik_protocol::message::{CHANNEL_CONTROL, NO_DISCRIMINANT, PROTOCOL_VERSION};
use iznik_protocol::model::MAXIMUM_LAYOUT_DEPTH;

/// Anything the reference or the source can fail on.
type Failed = Box<dyn std::error::Error>;

/// The files whose tag modules the reference publishes.
const SOURCES: &[&str] = &["message.rs", "model.rs", "delta.rs", "command.rs"];

/// The reference itself, under the workspace root.
const REFERENCE: &str = "docs/notes/protocol.md";

/// What a markdown table cell is fenced with when it names an item.
const FENCE: char = '`';

/// What separates a tag module from the constant inside it.
const PATH_SEPARATOR: &str = "::";

/// The workspace root, from this crate's own directory.
fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The text between the first pair of backticks in a cell, if it is fenced at
/// both ends and holds nothing else.
fn fenced(cell: &str) -> Option<&str> {
    let trimmed = cell.trim();
    let inner = trimmed.strip_prefix(FENCE)?.strip_suffix(FENCE)?;
    if inner.contains(FENCE) {
        return None;
    }
    Some(inner)
}

/// The number a cell begins with, ignoring the spaces a long number is grouped
/// with and whatever unit follows it in brackets.
fn numbered(cell: &str) -> Option<u64> {
    let digits: String = cell
        .trim()
        .chars()
        .take_while(|glyph| glyph.is_ascii_digit() || *glyph == ' ')
        .filter(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

/// The cells of a markdown table row, or nothing when the line is not one.
fn cells(line: &str) -> Option<Vec<&str>> {
    let trimmed = line.trim();
    let inner = trimmed.strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').collect())
}

/// Every `module::CONSTANT` the reference publishes, with the value it gives
/// it: a row whose second cell is a fenced path and whose third is a number.
fn published(reference: &str) -> BTreeMap<String, u64> {
    let mut found = BTreeMap::new();
    for line in reference.lines() {
        let Some(row) = cells(line) else { continue };
        let (Some(named), Some(valued)) = (row.get(1), row.get(2)) else {
            continue;
        };
        let Some(path) = fenced(named).filter(|path| path.contains(PATH_SEPARATOR)) else {
            continue;
        };
        let Some(value) = numbered(valued) else {
            continue;
        };
        let _replaced = found.insert(path.to_owned(), value);
    }
    found
}

/// Every constant the reference publishes on its own, without a module: a row
/// whose first cell is a fenced name and whose second is a number.
fn published_alone(reference: &str) -> BTreeMap<String, u64> {
    let mut found = BTreeMap::new();
    for line in reference.lines() {
        let Some(row) = cells(line) else { continue };
        let (Some(named), Some(valued)) = (row.first(), row.get(1)) else {
            continue;
        };
        let Some(name) = fenced(named).filter(|name| !name.contains(PATH_SEPARATOR)) else {
            continue;
        };
        let Some(value) = numbered(valued) else {
            continue;
        };
        let _replaced = found.insert(name.to_owned(), value);
    }
    found
}

/// Every `module::CONSTANT` a source file declares, with its value: the tag
/// modules this crate keeps its wire values in.
fn declared(source: &str) -> BTreeMap<String, u64> {
    let mut found = BTreeMap::new();
    let mut module = String::new();
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("mod ")
            && let Some(named) = rest.strip_suffix(" {")
        {
            named.clone_into(&mut module);
        }
        let Some(rest) = line.trim().strip_prefix("pub(super) const ") else {
            continue;
        };
        let Some((name, value)) = rest.split_once(": u8 = ") else {
            continue;
        };
        let Some(value) = value
            .strip_suffix(';')
            .and_then(|digits| digits.parse().ok())
        else {
            continue;
        };
        let _replaced = found.insert(format!("{module}{PATH_SEPARATOR}{name}"), value);
    }
    found
}

/// Every tag constant this crate declares, across the files that have them.
///
/// # Errors
///
/// When a source file cannot be read.
fn every_tag() -> Result<BTreeMap<String, u64>, Failed> {
    let mut found = BTreeMap::new();
    for file in SOURCES {
        let path = workspace().join("crates/iznik-protocol/src").join(file);
        found.extend(declared(&std::fs::read_to_string(path)?));
    }
    Ok(found)
}

/// # Panics
///
/// When a discriminant the reference publishes is not the constant of that
/// name in this crate, or when a tag this crate declares is published nowhere.
#[test]
fn every_discriminant_in_the_reference_is_the_source_constant() {
    let case = || -> Result<(), Failed> {
        let reference = std::fs::read_to_string(workspace().join(REFERENCE))?;
        let published = published(&reference);
        let declared = every_tag()?;
        assert!(!declared.is_empty(), "the crate declares tag constants");

        let wrong: Vec<String> = published
            .iter()
            .filter(|(path, value)| declared.get(*path) != Some(*value))
            .map(|(path, value)| {
                format!(
                    "{path} is {value} in the reference, {:?} here",
                    declared.get(path)
                )
            })
            .collect();
        assert!(wrong.is_empty(), "the reference disagrees: {wrong:?}");

        let unpublished: Vec<&String> = declared
            .keys()
            .filter(|path| !published.contains_key(*path))
            .collect();
        assert!(
            unpublished.is_empty(),
            "a second implementation would never learn of these: {unpublished:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a limit the reference publishes is not this crate's.
#[test]
fn every_limit_in_the_reference_is_the_source_constant() {
    let case = || -> Result<(), Failed> {
        let reference = std::fs::read_to_string(workspace().join(REFERENCE))?;
        let published = published_alone(&reference);
        let limits = [
            ("MAXIMUM_PAYLOAD_LENGTH", u64::from(MAXIMUM_PAYLOAD_LENGTH)),
            ("HEADER_LENGTH", u64::try_from(HEADER_LENGTH)?),
            ("MAXIMUM_LAYOUT_DEPTH", u64::try_from(MAXIMUM_LAYOUT_DEPTH)?),
            ("PROTOCOL_VERSION", u64::from(PROTOCOL_VERSION)),
            ("CHANNEL_CONTROL", u64::from(CHANNEL_CONTROL)),
            ("NO_DISCRIMINANT", u64::from(NO_DISCRIMINANT)),
        ];
        for (name, value) in limits {
            assert_eq!(
                published.get(name),
                Some(&value),
                "{name} is {value} here and {:?} in the reference",
                published.get(name)
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
