//! The contract, held to the header it describes.
//!
//! `docs/CLIENT.md` is what a developer who cannot read this repository is
//! given, and where it and the implementation disagree the contract wins and
//! the implementation is the bug. That is only true of a contract that says
//! what the header says: a name the contract does not mention is a thing
//! nobody was told about, a name it mentions that is not there is a promise
//! nothing keeps, and an obligation stated in one and not the other is the
//! two disagreeing about who has to do what.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use xtask::header::HEADER_PATH;

/// What an obligation begins with, in both documents.
const MARKER: &str = "**Obligation:**";

/// Where the contract lives, relative to the repository root.
const CONTRACT_PATH: &str = "docs/CLIENT.md";

/// The one name in the header that is not part of the surface: the guard that
/// keeps it from being included twice.
const INCLUDE_GUARD: &str = "IZNIK_H";

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The repository root.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Every name of iznik's own in some text.
///
/// Both spellings, because the header has both: the functions and types are
/// `iznik_` and the codes and the kinds are `IZNIK_`.
fn named(text: &str) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let letters: Vec<char> = text.chars().collect();
    let mut at = 0;
    while at < letters.len() {
        let rest: String = letters.get(at..).unwrap_or_default().iter().collect();
        let lower = rest.starts_with("iznik_");
        let upper = rest.starts_with("IZNIK_");
        if !lower && !upper {
            at = at.saturating_add(1);
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|letter| letter.is_alphanumeric() || *letter == '_')
            .collect();
        at = at.saturating_add(name.chars().count().max(1));
        if name != INCLUDE_GUARD {
            let _added = found.insert(name);
        }
    }
    found
}

/// Every obligation a text states, each as one line with its wrapping taken
/// out.
///
/// The header wraps at what a comment allows and the contract at what a
/// paragraph allows, so the two are compared by their words: that is what
/// "verbatim" can mean between a C comment and a document.
fn obligations(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (at, _marker) in text.match_indices(MARKER) {
        let rest = text.get(at..).unwrap_or_default();
        let mut held: Vec<String> = Vec::new();
        for (which, line) in rest.lines().enumerate() {
            // The first line begins at the marker itself; the ones after it
            // carry whatever their document wraps them in — a comment's
            // asterisk, a quotation's angle — and end at a blank one.
            if which == 0 {
                held.push(line.trim().to_owned());
                continue;
            }
            let said = line.trim().trim_start_matches(['*', '>']).trim();
            // A blank line ends a paragraph, and what is left of `*/` when
            // its asterisk is taken off ends a comment.
            if said.is_empty() || said == "/" {
                break;
            }
            held.push(said.to_owned());
        }
        found.push(collapsed(&held.join(" ")));
    }
    found
}

/// Some text with every run of spaces made one.
fn collapsed(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// # Panics
///
/// When the contract and the header do not name the same things.
#[test]
fn contract_names_everything_the_header_declares_and_no_other() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let header = std::fs::read_to_string(root.join(HEADER_PATH))?;
        let contract = std::fs::read_to_string(root.join(CONTRACT_PATH))?;
        let declared = named(&header);
        let written = named(&contract);
        assert!(!declared.is_empty(), "the header declares names to compare");
        let missing: Vec<&String> = declared.difference(&written).collect();
        assert!(
            missing.is_empty(),
            "everything the header declares is in the contract: {missing:?}"
        );
        let invented: Vec<&String> = written.difference(&declared).collect();
        assert!(
            invented.is_empty(),
            "and the contract names nothing the header does not declare: {invented:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When an obligation the header states is not in the contract in the same
/// words.
#[test]
fn contract_states_every_obligation_the_header_does() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let header = std::fs::read_to_string(root.join(HEADER_PATH))?;
        let contract = std::fs::read_to_string(root.join(CONTRACT_PATH))?;
        // Collapsed for the looking, whole for the reading: an obligation is
        // found in one long line and read out of the document's own.
        let looking = collapsed(&contract);
        let owed = obligations(&header);
        assert!(
            !owed.is_empty(),
            "the header states obligations to look for"
        );
        for said in &owed {
            assert!(
                looking.contains(said.as_str()),
                "the contract states this in the same words: {said}"
            );
        }
        // And states no obligation of its own that the header does not: a
        // developer holding to one the library never promised is holding to
        // something nothing keeps.
        for said in obligations(&contract) {
            assert!(
                owed.contains(&said),
                "and states none the header does not: {said}"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
