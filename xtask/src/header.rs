//! `xtask header`: generating `include/iznik.h` with cbindgen, for the golden
//! test that pins the ABI.
//!
//! The header is the contract a native application is built against, and it is
//! generated rather than written so that it cannot drift from the crate it
//! describes. A change to a signature becomes a change to the committed copy
//! in the same commit, which is a thing a reviewer sees; `header_golden.rs`
//! is what makes it one.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::{self, Display, Formatter};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// What this subcommand takes, which is nothing.
const USAGE: &str = "usage: xtask header";

/// Where the generated header is kept, relative to the repository root.
pub const HEADER_PATH: &str = "include/iznik.h";

/// The crate it is generated from, relative to the repository root.
pub const FFI_CRATE: &str = "crates/iznik-ffi";

/// The configuration that shapes it, relative to the repository root.
pub const CONFIGURATION: &str = "cbindgen.toml";

/// How many characters open a link in Rust's documentation syntax.
const LINK_OPENS: usize = 2;

/// And how many close one.
const LINK_CLOSES: usize = 2;

/// Why a header could not be produced or written.
#[derive(Debug)]
pub enum HeaderError {
    /// The configuration could not be read.
    Configuration {
        /// The file it was read from.
        path: PathBuf,
        /// What was wrong with it.
        detail: String,
    },
    /// The crate could not be turned into a header.
    Generate {
        /// What cbindgen said.
        detail: String,
    },
    /// The header could not be written where it belongs.
    Write {
        /// The file it was to be written to.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
}

impl Display for HeaderError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            HeaderError::Configuration { path, detail } => {
                write!(formatter, "{}: {detail}", path.display())
            }
            HeaderError::Generate { detail } => write!(formatter, "the header: {detail}"),
            HeaderError::Write { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
        }
    }
}

impl core::error::Error for HeaderError {}

/// The header this crate's surface makes, as text.
///
/// Takes the repository root, so that a case may point it at a copy of the
/// tree and compare what comes out.
///
/// # Errors
///
/// [`HeaderError::Configuration`] when `cbindgen.toml` cannot be read, and
/// [`HeaderError::Generate`] when the crate cannot be turned into a header.
pub fn generated(root: &Path) -> Result<String, HeaderError> {
    let named = root.join(CONFIGURATION);
    let configuration =
        cbindgen::Config::from_file(&named).map_err(|source| HeaderError::Configuration {
            path: named,
            detail: source,
        })?;
    let bindings = cbindgen::Builder::new()
        .with_crate(root.join(FFI_CRATE))
        .with_config(configuration)
        .generate()
        .map_err(|source| HeaderError::Generate {
            detail: source.to_string(),
        })?;
    let mut written = Vec::new();
    bindings.write(&mut written);
    let header = String::from_utf8(written).map_err(|source| HeaderError::Generate {
        detail: source.to_string(),
    })?;
    Ok(readable(&header, &renames(root)?))
}

/// What each Rust name is called in the header.
///
/// Read from the same table that tells cbindgen, so that a name flattened out
/// of a link and a name on a type cannot come to disagree.
///
/// # Errors
///
/// [`HeaderError::Configuration`] when the file cannot be read or is not the
/// shape this expects.
fn renames(root: &Path) -> Result<BTreeMap<String, String>, HeaderError> {
    let named = root.join(CONFIGURATION);
    let text = std::fs::read_to_string(&named).map_err(|source| HeaderError::Configuration {
        path: named.clone(),
        detail: source.to_string(),
    })?;
    let held: toml::Value = toml::from_str(&text).map_err(|source| HeaderError::Configuration {
        path: named,
        detail: source.to_string(),
    })?;
    let table = held
        .get("export")
        .and_then(|export| export.get("rename"))
        .and_then(toml::Value::as_table);
    Ok(table.map(collected).unwrap_or_default())
}

/// The string entries of a table, as a map.
fn collected(table: &toml::Table) -> BTreeMap<String, String> {
    table
        .iter()
        .filter_map(|(rust, named)| {
            named
                .as_str()
                .map(|called| (rust.clone(), called.to_owned()))
        })
        .collect()
}

/// The header cbindgen makes, read the way a C programmer will read it.
///
/// Rust's own documentation syntax comes through unchanged: a link to another
/// item is `` [`Name`] ``, and a section is headed `# Safety` — neither of
/// which means anything in C, and the name inside a link is the Rust one
/// rather than the name this header gives it. So the links are flattened to
/// the names the header uses and the heading is written as a sentence. The
/// contract is cbindgen's; only how it reads is this.
fn readable(header: &str, names: &BTreeMap<String, String>) -> String {
    let mut written = String::with_capacity(header.len());
    let mut left = header;
    while let Some(at) = left.find("[`") {
        let (before, rest) = left.split_at(at);
        written.push_str(before);
        let inside = rest.get(LINK_OPENS..).unwrap_or_default();
        let Some(end) = inside.find("`]") else {
            // An opening with nothing closing it is not a link, and goes
            // through as it came.
            written.push_str(rest);
            left = "";
            continue;
        };
        let linked = inside.get(..end).unwrap_or_default();
        let called = names.get(linked).map_or(linked, String::as_str);
        written.push('`');
        written.push_str(called);
        written.push('`');
        left = inside
            .get(end.saturating_add(LINK_CLOSES)..)
            .unwrap_or_default();
    }
    written.push_str(left);
    written.replace(" * # Safety", " * Safety:")
}

/// Writes the header where the golden copy lives, and says where that was.
///
/// # Errors
///
/// As [`generated`], and [`HeaderError::Write`] when it cannot be written.
pub fn write(root: &Path) -> Result<PathBuf, HeaderError> {
    let header = generated(root)?;
    let path = root.join(HEADER_PATH);
    if let Some(holding) = path.parent() {
        std::fs::create_dir_all(holding).map_err(|source| HeaderError::Write {
            path: path.clone(),
            source,
        })?;
    }
    std::fs::write(&path, header).map_err(|source| HeaderError::Write {
        path: path.clone(),
        source,
    })?;
    Ok(path)
}

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    match write(&crate::repository_root()) {
        Ok(path) => {
            writeln!(std::io::stdout(), "wrote {}", path.display()).unwrap_or_default();
            ExitCode::SUCCESS
        }
        Err(refusal) => {
            writeln!(std::io::stderr(), "{refusal}").unwrap_or_default();
            ExitCode::FAILURE
        }
    }
}
