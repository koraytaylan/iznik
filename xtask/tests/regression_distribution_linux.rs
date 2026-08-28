//! The Linux artifacts, held to what a bootstrap needs of them: statically
//! linked so a host's libc version does not matter, stripped and small enough
//! to upload, reproducible so a digest is worth having, and — for the one this
//! machine can run — able to say its version inside the host container.
//!
//! Every case is ignored by default: each builds a release artifact, which
//! takes minutes and is not something the ordinary gate should do.

use std::path::Path;

use iznik_harness::fixture::{Fixture, FixtureOptions};
use iznik_harness::process::Deadline;
use iznik_harness::staging;
use xtask::distribution::{
    ARTIFACT_SIZE_CEILING, Artifact, BINARY, CHECKSUMS, MANIFEST, build, digest_of, workspace_root,
};

/// The triple this machine can also run.
const NATIVE: &str = "x86_64-unknown-linux-musl";

/// The triple it can only build.
const CROSS: &str = "aarch64-unknown-linux-musl";

/// Where the ELF magic is, and what it is.
const ELF_MAGIC: &[u8] = b"\x7fELF";

/// Where the machine field is in an ELF header.
const MACHINE_AT: usize = 18;

/// The machine value of x86-64.
const MACHINE_X86_64: u16 = 62;

/// The machine value of `AArch64`.
const MACHINE_AARCH64: u16 = 183;

/// How long the container step may take.
const CONTAINER_DEADLINE: core::time::Duration = core::time::Duration::from_mins(1);

/// How long the ordinary staging build may take, which this borrows a tree
/// from rather than laying one out from nothing.
const STAGING_DEADLINE: core::time::Duration = core::time::Duration::from_mins(20);

/// How long throwing away one package's build products may take.
const CLEAN_DEADLINE: core::time::Duration = core::time::Duration::from_mins(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// Where the program header table's offset is.
const PROGRAM_HEADERS_AT: usize = 32;

/// Where the size of one program header entry is.
const HEADER_SIZE_AT: usize = 54;

/// Where the count of them is.
const HEADER_COUNT_AT: usize = 56;

/// The program header type of an interpreter: what a dynamically linked
/// executable names its loader with, and a static one has none of.
const PROGRAM_INTERPRETER: u32 = 3;

/// Where the section header table's offset is.
const SECTION_HEADERS_AT: usize = 40;

/// Where the size of one section header entry is.
const SECTION_SIZE_AT: usize = 58;

/// Where the count of them is.
const SECTION_COUNT_AT: usize = 60;

/// The section type of a symbol table: what stripping takes out, and the one
/// thing that says whether it happened.
const SECTION_SYMBOLS: u32 = 2;

/// Where a header's type is, in both tables.
const KIND_AT: usize = 4;

/// What an artifact's ELF headers say about it.
struct Shape {
    /// The machine it declares.
    machine: u16,
    /// Whether any program header names an interpreter, which a statically
    /// linked executable has none of.
    interpreted: bool,
    /// Whether any section is a symbol table, which a stripped one has none
    /// of.
    symbols: bool,
}

/// A little-endian half at `at`, if the file reaches that far.
fn half(bytes: &[u8], at: usize) -> Option<u16> {
    bytes
        .get(at..at.saturating_add(2))
        .and_then(|held| <[u8; 2]>::try_from(held).ok())
        .map(u16::from_le_bytes)
}

/// A little-endian word at `at`, if the file reaches that far.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.saturating_add(4))
        .and_then(|held| <[u8; 4]>::try_from(held).ok())
        .map(u32::from_le_bytes)
}

/// A little-endian offset at `at`, if the file reaches that far.
fn offset(bytes: &[u8], at: usize) -> Option<usize> {
    bytes
        .get(at..at.saturating_add(8))
        .and_then(|held| <[u8; 8]>::try_from(held).ok())
        .map(u64::from_le_bytes)
        .and_then(|held| usize::try_from(held).ok())
}

/// Whether any entry of a header table declares `kind`.
///
/// A table this cannot walk — no entries, or entries of no length, or a
/// header past the end of the file — is an error rather than an answer of
/// `false`: reporting a truncated file as statically linked and stripped is
/// exactly the wrong way round.
///
/// # Errors
///
/// When the table cannot be walked.
fn declares(
    bytes: &[u8],
    at: usize,
    stride: usize,
    count: usize,
    kind: u32,
) -> Result<bool, Failed> {
    if stride == 0 || count == 0 {
        return Err(format!("a header table of {count} entries of {stride} bytes").into());
    }
    let mut found = false;
    for index in 0..count {
        let entry = at
            .checked_add(index.saturating_mul(stride))
            .ok_or("a header table that runs past what can be addressed")?;
        let declared = word(bytes, entry.saturating_add(KIND_AT))
            .ok_or("a header table entry past the end of the file")?;
        found = found || declared == kind;
    }
    Ok(found)
}

/// What an ELF file's own headers say: the machine, whether it names a loader,
/// and whether it still has a symbol table.
///
/// The headers, not the strings: musl's loader path appears in a static
/// binary's data, so searching for it would call every artifact dynamic.
///
/// # Errors
///
/// When the file cannot be read, is not an ELF file, or has a header table
/// that cannot be walked.
fn elf_shape(path: &Path) -> Result<Shape, Failed> {
    let bytes = std::fs::read(path)?;
    if bytes.get(0..ELF_MAGIC.len()) != Some(ELF_MAGIC) {
        return Err(format!("{} is not an ELF file", path.display()).into());
    }
    let machine = half(&bytes, MACHINE_AT).ok_or("the ELF header ends before its machine")?;
    let programs = offset(&bytes, PROGRAM_HEADERS_AT)
        .ok_or("the ELF header ends before its program headers")?;
    let interpreted = declares(
        &bytes,
        programs,
        usize::from(half(&bytes, HEADER_SIZE_AT).unwrap_or_default()),
        usize::from(half(&bytes, HEADER_COUNT_AT).unwrap_or_default()),
        PROGRAM_INTERPRETER,
    )?;
    let sections =
        offset(&bytes, SECTION_HEADERS_AT).ok_or("the ELF header ends before its sections")?;
    let symbols = declares(
        &bytes,
        sections,
        usize::from(half(&bytes, SECTION_SIZE_AT).unwrap_or_default()),
        usize::from(half(&bytes, SECTION_COUNT_AT).unwrap_or_default()),
        SECTION_SYMBOLS,
    )?;
    Ok(Shape {
        machine,
        interpreted,
        symbols,
    })
}

/// Builds one target and holds it to what every artifact must be.
///
/// # Errors
///
/// When the build fails or the artifact cannot be read.
///
/// # Panics
///
/// When the artifact is not a stripped static executable for the machine it
/// names, or is over the size ceiling.
fn artifact_of(target: &str, machine: u16) -> Result<Artifact, Failed> {
    let root = workspace_root();
    let artifact = build(&root, target)?;
    // `build` refuses over the ceiling, so what is worth asserting here is
    // that the number it recorded is the file's own — a producer that
    // measured something else would be under the ceiling and wrong.
    assert_eq!(
        std::fs::metadata(&artifact.path)?.len(),
        artifact.bytes,
        "{target}: the recorded size is the artifact's own"
    );
    assert!(
        artifact.bytes < ARTIFACT_SIZE_CEILING,
        "{target} is {} bytes, over the {ARTIFACT_SIZE_CEILING} ceiling",
        artifact.bytes
    );
    let shape = elf_shape(&artifact.path)?;
    assert_eq!(
        shape.machine, machine,
        "{target} declares the machine it is for"
    );
    assert!(!shape.interpreted, "{target} names no loader: it is static");
    assert!(
        !shape.symbols,
        "{target} has no symbol table: it is stripped"
    );
    assert_eq!(
        digest_of(&artifact.path)?,
        artifact.digest,
        "and its digest is its own"
    );
    Ok(artifact)
}

/// # Panics
///
/// When either musl artifact is not a stripped static executable for the
/// machine it names, or is over the size ceiling.
#[ignore = "builds release artifacts; run deliberately"]
#[test]
fn distribution_builds_both_musl_targets() {
    let case = || -> Result<(), Failed> {
        let _native = artifact_of(NATIVE, MACHINE_X86_64)?;
        let _cross = artifact_of(CROSS, MACHINE_AARCH64)?;
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Throws away what cargo built for a triple, so the next build is a build and
/// not a lookup.
///
/// # Errors
///
/// When `cargo clean` cannot be run or does not succeed.
fn rebuild_from_nothing(root: &Path, target: &str) -> Result<(), Failed> {
    let mut command =
        std::process::Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command
        .current_dir(root)
        .arg("clean")
        .arg("--package")
        .arg(BINARY)
        .arg("--target")
        .arg(target)
        .arg("--release");
    let _cleaned = iznik_harness::process::run(
        command,
        Deadline(CLEAN_DEADLINE),
        iznik_harness::process::Output::Capture,
    )?;
    Ok(())
}

/// # Panics
///
/// When two builds of one commit do not produce the same bytes, or the
/// manifest does not record what the artifact is.
#[ignore = "builds release artifacts; run deliberately"]
#[test]
fn distribution_is_reproducible_and_recorded() {
    let case = || -> Result<(), Failed> {
        let root = workspace_root();
        let once = build(&root, NATIVE)?;
        let first = std::fs::read(&once.path)?;
        let sums = std::fs::read_to_string(once.path.with_file_name(CHECKSUMS))?;
        // Without this the second build is a fingerprint hit: cargo relinks
        // nothing, `build` copies the same file to the same place, and the
        // comparison below holds however non-deterministic the compiler is.
        rebuild_from_nothing(&root, NATIVE)?;
        let again = build(&root, NATIVE)?;
        let second = std::fs::read(&again.path)?;
        assert_eq!(first, second, "two builds of one commit are one binary");
        assert_eq!(
            sums,
            std::fs::read_to_string(again.path.with_file_name(CHECKSUMS))?,
            "and one checksum file"
        );

        let manifest = std::fs::read_to_string(again.path.with_file_name(MANIFEST))?;
        for wanted in [
            "crate_version = ",
            "protocol_version = ",
            &format!("target = \"{NATIVE}\""),
            &format!("sha256 = \"{}\"", again.digest),
        ] {
            assert!(
                manifest.contains(wanted),
                "the manifest records {wanted:?}: {manifest}"
            );
        }
        assert!(
            !manifest.contains("protocol_version = 0"),
            "and a protocol version it actually read: {manifest}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the artifact cannot say its version inside the host container, which
/// is the only place its static linking is really tested.
#[ignore = "builds a release artifact and starts containers; run deliberately"]
#[test]
fn distribution_runs_inside_the_host_container() {
    let case = || -> Result<(), Failed> {
        let root = workspace_root();
        let artifact = build(&root, NATIVE)?;
        // A staging tree of this artifact's own: everything the fixture needs
        // to run, with the binary under test in the place the container looks.
        let ordinary = staging::stage(Deadline(STAGING_DEADLINE))?;
        let staged = xtask::distribution::target_directory(&root)
            .join("distribution-staging")
            .join(NATIVE);
        let _cleared = std::fs::remove_dir_all(&staged);
        copy_tree(&ordinary, &staged)?;
        for place in [
            staged.join("bin").join(BINARY),
            staged.join("distribution").join(NATIVE).join(BINARY),
        ] {
            let _copied = std::fs::copy(&artifact.path, &place)?;
        }

        let fixture = Fixture::start(FixtureOptions::new(1, staged))?;
        let said = fixture.exec(
            "host0",
            "/iznik/bin/iznik-server --version",
            CONTAINER_DEADLINE,
        )?;
        let printed = String::from_utf8_lossy(&said.stdout).into_owned();
        assert!(
            printed.starts_with("iznik-server ") && printed.contains(" protocol "),
            "it says its version inside the container: {printed:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// Copies a directory tree, so a staged layout can be assembled around one
/// artifact without disturbing the one every other scenario shares.
///
/// # Errors
///
/// When a file or a directory cannot be read or written.
fn copy_tree(from: &Path, to: &Path) -> Result<(), Failed> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            let _copied = std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}
