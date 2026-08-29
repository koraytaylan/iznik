//! The boundary, proven with C.
//!
//! Everything else in this workspace calls the `extern "C"` functions from
//! Rust, which is not the thing an application does: it includes a header, it
//! links an archive, and its compiler agrees with neither of those unless they
//! are right. So this builds the library, reads from the toolchain which
//! system libraries a static archive of it needs, compiles
//! `fixtures/ffi/smoke.c` against the generated header with the system C
//! compiler, and runs it against a daemon standing in this process.
//!
//! Ignored by default because it builds a release-shaped library; `xtask
//! check` runs it where the other `regression_*` cases run.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use iznik_harness::process::{self, Deadline, Output};
use iznik_protocol::command::{SessionCommand, encode_session_command};
use iznik_testkit::stack::{Stack, StackOptions};
use xtask::header::HEADER_PATH;

/// How long the library may take to build, which on a cold cache is the whole
/// of this workspace at release shape.
///
/// Inside the fifteen minutes the claims gate gives a whole run, so that a
/// build that will not finish is reported as a build that did not finish
/// rather than as a gate that timed out with nothing to say about why.
const BUILD_DEADLINE: Deadline = Deadline(Duration::from_mins(10));

/// How long the C compiler is given.
const COMPILE_DEADLINE: Deadline = Deadline(Duration::from_mins(2));

/// How long the program itself is given, once it is built: the architecture's
/// ten seconds, which is a whole session, a pane and a line through a daemon.
const RUN_DEADLINE: Deadline = Deadline(Duration::from_secs(10));

/// How long `nm` is given.
const READ_DEADLINE: Deadline = Deadline(Duration::from_mins(1));

/// The profile the library is built under: release-shaped, without the fat
/// link-time optimization a shipped artifact gets, because what is wanted
/// here is a library and not a small one.
const PROFILE: &str = "regression";

/// The width the session's pane is made at.
const COLUMNS: u16 = 80;

/// And its height.
const ROWS: u16 = 24;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The repository root.
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
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-smoke-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// What a build of the library leaves behind.
#[derive(Debug)]
struct Built {
    /// The static archive an application links.
    archive: PathBuf,
    /// The shared library it may link instead.
    shared: PathBuf,
    /// The system libraries the archive needs beside it, as the toolchain
    /// reports them — never guessed, because the list differs by platform and
    /// by libc.
    native: Vec<String>,
}

/// Builds the library and says where it went.
///
/// # Errors
///
/// When the build fails, or says nothing about where it put what it made.
fn built(root: &Path) -> Result<Built, Failed> {
    let mut build = Command::new("cargo");
    build
        .current_dir(root)
        .arg("build")
        .arg("--package")
        .arg("iznik-ffi")
        .arg("--profile")
        .arg(PROFILE)
        .arg("--locked")
        .arg("--message-format")
        .arg("json");
    let made = process::run(build, BUILD_DEADLINE, Output::Capture)?;
    let said = String::from_utf8_lossy(&made.stdout);
    let (mut archive, mut shared) = (None, None);
    for line in said.lines() {
        let Ok(held) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if held.get("reason").and_then(serde_json::Value::as_str) != Some("compiler-artifact") {
            continue;
        }
        let Some(files) = held.get("filenames").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for named in files.iter().filter_map(serde_json::Value::as_str) {
            let path = PathBuf::from(named);
            match path.extension().and_then(std::ffi::OsStr::to_str) {
                Some("a") if named.contains("libiznik") => archive = Some(path),
                Some("so") if named.contains("libiznik") => shared = Some(path),
                _otherwise => {}
            }
        }
    }
    Ok(Built {
        archive: archive.ok_or("the build says where it put the archive")?,
        shared: shared.ok_or("the build says where it put the shared library")?,
        native: native_libraries(root)?,
    })
}

/// The system libraries a static archive of the library needs beside it.
///
/// # Errors
///
/// When the toolchain will not say.
fn native_libraries(root: &Path) -> Result<Vec<String>, Failed> {
    let mut asked = Command::new("cargo");
    asked
        .current_dir(root)
        .arg("rustc")
        .arg("--package")
        .arg("iznik-ffi")
        .arg("--profile")
        .arg(PROFILE)
        .arg("--locked")
        .arg("--")
        .arg("--print")
        .arg("native-static-libs");
    let said = process::run(asked, BUILD_DEADLINE, Output::Capture)?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&said.stdout),
        String::from_utf8_lossy(&said.stderr)
    );
    let line = text
        .lines()
        .find_map(|line| line.split_once("native-static-libs:"))
        .map(|(_before, libraries)| libraries.trim().to_owned())
        .ok_or("the toolchain says which native libraries the archive needs")?;
    Ok(line
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<String>>())
}

/// Compiles the C program against the header and the archive.
///
/// # Errors
///
/// When no compiler can be found, or the one found refuses what it was given.
fn compiled(root: &Path, made: &Built, into: &Path) -> Result<PathBuf, Failed> {
    let compiler = ["cc", "gcc", "clang"]
        .into_iter()
        .find(|named| on_the_path(named))
        .ok_or("no C compiler: looked for cc, gcc and clang")?;
    let program = into.join("smoke");
    let mut build = Command::new(compiler);
    build
        .arg("-std=c11")
        .arg("-Wall")
        .arg("-Wextra")
        .arg("-Werror")
        .arg("-I")
        .arg(root.join("include"))
        .arg(root.join("xtask/tests/fixtures/ffi/smoke.c"))
        .arg(&made.archive)
        .args(&made.native)
        .arg("-o")
        .arg(&program);
    // A compiler that refuses is a child that exited non-zero, which the
    // runner turns into an error carrying what it said.
    let _built = process::run(build, COMPILE_DEADLINE, Output::Capture)?;
    Ok(program)
}

/// Whether a program is on the path.
fn on_the_path(program: &str) -> bool {
    let mut command = Command::new("sh");
    command.arg("-c").arg(format!("command -v {program}"));
    process::run(command, READ_DEADLINE, Output::Capture).is_ok()
}

/// What `nm` says a file defines.
///
/// # Errors
///
/// When `nm` cannot be run.
fn defines(path: &Path, arguments: &[&str]) -> Result<Vec<String>, Failed> {
    let mut command = Command::new("nm");
    command.args(arguments).arg(path);
    let said = process::run(command, READ_DEADLINE, Output::Capture)?;
    Ok(named_in(&String::from_utf8_lossy(&said.stdout)))
}

/// What `nm` says a file defines, of iznik's own names.
///
/// Asked for through a filter rather than read out of everything `nm` says: a
/// static archive carries an object for every crate in the tree, and what it
/// has to say about all of them is megabytes — of which a reader keeps the
/// end, which is not where these are.
///
/// # Errors
///
/// When `nm` cannot be run.
fn defines_iznik(path: &Path) -> Result<Vec<String>, Failed> {
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("nm --defined-only \"$1\" | grep \" iznik_\" || true")
        .arg("sh")
        .arg(path);
    let said = process::run(command, READ_DEADLINE, Output::Capture)?;
    Ok(named_in(&String::from_utf8_lossy(&said.stdout)))
}

/// The symbol at the end of each of `nm`'s lines.
fn named_in(said: &str) -> Vec<String> {
    said.lines()
        .filter_map(|line| line.split_whitespace().last())
        .map(str::to_owned)
        .collect()
}

/// # Panics
///
/// When a C program cannot do what an application's first afternoon does.
#[test]
#[ignore = "builds the library and compiles a C program against it"]
fn regression_ffi_smoke_runs_a_c_program_against_a_daemon() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let made = built(&root)?;
        let held = scratch("run")?;
        let program = compiled(&root, &made, &held.path)?;
        // What the C program sends, encoded here because the encoding is the
        // protocol's and a C program is not where a schema belongs.
        let asked = encode_session_command(&SessionCommand::CreateSession {
            name: "smoke".to_owned(),
            columns: COLUMNS,
            rows: ROWS,
            working_directory: None,
        })?;
        std::fs::write(held.path.join("command.bin"), &asked)?;
        std::fs::create_dir_all(held.path.join("runtime"))?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
        let mut running = Command::new(&program);
        running
            .current_dir(&held.path)
            .arg(format!("unix:{}", stack.socket().display()));
        // A program that fails, or outlasts the deadline, is an error the
        // runner raises with what the program said in it; what is left to
        // assert is that it did the thing rather than merely exited.
        let done = process::run(running, RUN_DEADLINE, Output::Capture)?;
        assert!(
            String::from_utf8_lossy(&done.stdout).contains("screen first"),
            "the C program ran the whole sequence and said so: {}",
            String::from_utf8_lossy(&done.stdout)
        );
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the shared library exports a name that is not iznik's, or the archive
/// is missing one the header declares.
#[test]
#[ignore = "builds the library"]
fn regression_ffi_smoke_exports_only_iznik_names() {
    let case = || -> Result<(), Failed> {
        let root = root();
        let made = built(&root)?;
        // What a linker can see from outside: only the boundary's own names.
        let exported = defines(&made.shared, &["-D", "--defined-only"])?;
        // Everything the linker itself puts there begins with an underscore
        // — `_init`, `_fini`, `__bss_start` — and is nobody's to remove.
        let strangers: Vec<&String> = exported
            .iter()
            .filter(|named| !named.starts_with("iznik_") && !named.starts_with('_'))
            .collect();
        assert!(
            strangers.is_empty(),
            "the shared library exports only iznik's own names: {strangers:?}"
        );
        // And the archive, which cannot hide anything, at least defines every
        // one the header promises.
        let defined = defines_iznik(&made.archive)?;
        let header = std::fs::read_to_string(root.join(HEADER_PATH))?;
        let promised: Vec<String> = header
            .lines()
            .map(str::trim)
            .filter(|line| !line.starts_with('*') && !line.starts_with("/*"))
            .filter_map(|line| line.split_once('('))
            .filter_map(|(before, _arguments)| {
                let from =
                    before.rfind(|letter: char| !letter.is_alphanumeric() && letter != '_')?;
                before.get(from.saturating_add(1)..).map(str::to_owned)
            })
            .filter(|named| named.starts_with("iznik_"))
            .collect();
        assert!(
            !promised.is_empty(),
            "the header promises functions to look for"
        );
        let missing: Vec<&String> = promised
            .iter()
            .filter(|named| !defined.contains(named))
            .collect();
        assert!(
            missing.is_empty(),
            "the archive defines every function the header declares: {missing:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
