//! The header, held to the crate it describes.
//!
//! `include/iznik.h` is generated, and the copy in the tree is the contract a
//! native application is built against — so what pins it is not that somebody
//! remembered to regenerate it, but that a case regenerates it and compares.
//! A change to a signature becomes a change to the committed header in the
//! same commit, which is a thing a reviewer sees.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};
use xtask::header::{FFI_CRATE, HEADER_PATH, generated};

/// How long the C compiler is given to read a header.
const COMPILE_DEADLINE: Deadline = Deadline(Duration::from_mins(1));

/// How long a copy of the tree is given to yield a header of its own.
const GENERATE_DEADLINE: Duration = Duration::from_mins(2);

/// What the tree is not copied with: the one directory that is large, and the
/// one that is a build.
const UNCOPIED: [&str; 3] = [".git", "target", ".jj"];

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The repository root, which is where the crate this tests lives under.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// A temporary directory of this case's own, removed when the guard drops.
#[derive(Debug)]
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When it cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let path = std::env::temp_dir().join(format!("iznik-header-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// Copies the tree, without what a header does not need and what is large.
///
/// # Errors
///
/// When anything cannot be read or written.
fn copied(from: &Path, to: &Path) -> Result<(), Failed> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let named = entry.file_name();
        if UNCOPIED.iter().any(|skipped| named == *skipped) {
            continue;
        }
        let (source, target) = (entry.path(), to.join(&named));
        if entry.file_type()?.is_dir() {
            copied(&source, &target)?;
        } else {
            let _bytes = std::fs::copy(&source, &target)?;
        }
    }
    Ok(())
}

/// Every `extern "C"` function the crate declares, by name.
///
/// # Errors
///
/// When a source file cannot be read or parsed.
fn declared(crate_root: &Path) -> Result<BTreeSet<String>, Failed> {
    let mut found = BTreeSet::new();
    for entry in std::fs::read_dir(crate_root.join("src"))? {
        let path = entry?.path();
        if path.extension().is_none_or(|named| named != "rs") {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let parsed = syn::parse_file(&text)?;
        for item in parsed.items {
            let syn::Item::Fn(held) = item else {
                continue;
            };
            if held.sig.abi.is_some() {
                let _added = found.insert(held.sig.ident.to_string());
            }
        }
    }
    Ok(found)
}

/// Every function the header declares, by name.
///
/// The name is the identifier the arguments open after, wherever that falls:
/// not the first `iznik_` on a line, since `iznik_client *iznik_client_new`
/// begins with its return type, and not one line at a time, since a signature
/// with enough arguments is wrapped across several. Comments are taken out
/// first, so that a name in prose is not read as a declaration.
fn header_declares(header: &str) -> BTreeSet<String> {
    let code = header
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('*') && !line.starts_with("/*"))
        .collect::<Vec<&str>>()
        .join("\n");
    code.match_indices('(')
        .filter_map(|(at, _open)| {
            let before = code.get(..at)?;
            let from = before.rfind(|letter: char| !letter.is_alphanumeric() && letter != '_')?;
            before.get(from.saturating_add(1)..)
        })
        .filter(|named| named.starts_with("iznik_"))
        .map(str::to_owned)
        .collect()
}

/// Runs a compiler over the header and says what it made of it.
///
/// # Errors
///
/// When no compiler can be found, or one cannot be run.
///
/// # Panics
///
/// When the compiler it found refuses the header.
fn compiles(header: &Path, arguments: &[&str]) -> Result<(), Failed> {
    let compiler = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|named| which(named))
        .ok_or("no C compiler: looked for cc, gcc and clang")?;
    let mut command = Command::new(compiler);
    command.args(arguments).arg(header);
    let done = process::run(command, COMPILE_DEADLINE, Output::Capture)?;
    assert!(
        done.status.success(),
        "{compiler} reads the header: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    Ok(())
}

/// Whether a program is on the path.
fn which(program: &str) -> bool {
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!("command -v {program}"));
    process::run(command, COMPILE_DEADLINE, Output::Capture).is_ok()
}

/// # Panics
///
/// When the committed header is not what the crate makes now.
#[test]
fn header_golden_matches_the_committed_copy() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let made = generated(&root)?;
        let committed = std::fs::read_to_string(root.join(HEADER_PATH))?;
        if made == committed {
            return Ok(());
        }
        let at = made
            .lines()
            .zip(committed.lines())
            .position(|(fresh, held)| fresh != held);
        panic!(
            "the header in the tree is not what the crate makes: \
             run `cargo xtask header`. First line that differs: {at:?}"
        );
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a signature that changed leaves the header unchanged.
#[test]
fn header_golden_notices_a_signature_that_changed() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let held = scratch("changed")?;
        let copy = held.path.join("tree");
        copied(&root, &copy)?;
        // One argument more than the crate declares, in a copy of it.
        let named = copy.join(FFI_CRATE).join("src").join("pane.rs");
        let text = std::fs::read_to_string(&named)?;
        let changed = text.replacen(
            "pub unsafe extern \"C\" fn iznik_pane_focus(\n    client: *mut Client,",
            "pub unsafe extern \"C\" fn iznik_pane_focus(\n    client: *mut Client,\n    watching: bool,",
            1,
        );
        assert_ne!(changed, text, "the copy is the one this case edits");
        std::fs::write(&named, changed)?;
        let began = std::time::Instant::now();
        let made = generated(&copy)?;
        assert!(
            began.elapsed() < GENERATE_DEADLINE,
            "a header comes out of a copy of the tree in {:?}",
            began.elapsed()
        );
        let committed = std::fs::read_to_string(root.join(HEADER_PATH))?;
        assert_ne!(
            made, committed,
            "a signature that changed makes a header that differs"
        );
        let at = made
            .lines()
            .zip(committed.lines())
            .position(|(fresh, kept)| fresh != kept)
            .ok_or("the two differ somewhere in their lines")?;
        assert!(
            made.lines()
                .nth(at)
                .is_some_and(|line| line.contains("iznik_pane_focus")),
            "and the first line that differs is the one whose signature changed: {:?}",
            made.lines().nth(at)
        );
        assert!(
            made.contains("watching"),
            "which now takes the argument the copy gave it"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the header does not stand on its own as C or as C++.
#[test]
fn header_golden_compiles_as_c_and_as_cxx() {
    let case = || -> Result<(), Failed> {
        let header = root().join(HEADER_PATH);
        // `-fsyntax-only`, because what is being asked is whether the header
        // is well formed, not whether anything links.
        compiles(
            &header,
            &["-std=c11", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"],
        )?;
        compiles(
            &header,
            &["-x", "c++", "-Wall", "-Wextra", "-Werror", "-fsyntax-only"],
        )?;
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the header and the crate do not declare the same functions, or a
/// function that owes an obligation does not say so.
#[test]
fn header_golden_declares_every_function_and_no_other() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let header = std::fs::read_to_string(root.join(HEADER_PATH))?;
        let crate_functions = declared(&root.join(FFI_CRATE))?;
        assert!(
            !crate_functions.is_empty(),
            "the crate declares functions to compare against"
        );
        let in_header = header_declares(&header);
        let missing: Vec<&String> = crate_functions
            .iter()
            .filter(|named| !in_header.contains(*named))
            .collect();
        assert!(
            missing.is_empty(),
            "every one is in the header: {missing:?}"
        );
        let extra: Vec<&String> = in_header
            .iter()
            .filter(|named| !crate_functions.contains(*named))
            .collect();
        assert!(
            extra.is_empty(),
            "and the header declares no other: {extra:?}"
        );
        // Every obligation the crate states is in the header, where the
        // person who has to keep it will read it.
        for named in &crate_functions {
            let owed = std::fs::read_dir(root.join(FFI_CRATE).join("src"))?
                .filter_map(Result::ok)
                .filter_map(|entry| std::fs::read_to_string(entry.path()).ok())
                .any(|text| owes(&text, named));
            if !owed {
                continue;
            }
            let stated = header
                .split(&format!("{named}("))
                .next()
                .is_some_and(|before| before.contains("Obligation:"));
            assert!(stated, "{named} carries its obligation into the header");
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Whether a source file states an obligation for one function.
fn owes(text: &str, named: &str) -> bool {
    let Some(at) = text.find(&format!("fn {named}(")) else {
        return false;
    };
    text.get(..at)
        .and_then(|before| before.rfind("///"))
        .and_then(|_doc| text.get(..at))
        .is_some_and(|before| {
            before
                .rsplit("\n\n")
                .next()
                .is_some_and(|paragraph| paragraph.contains("**Obligation:**"))
        })
}
