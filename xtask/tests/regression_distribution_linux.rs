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

/// Where a program header's type is.
const PROGRAM_KIND_AT: usize = 0;

/// Where a section header's type is. Not the same place: an ELF64 program
/// header carries its flags where a section header carries its type, so one
/// offset for both would read a segment's permissions and find a loader in
/// nothing.
const SECTION_KIND_AT: usize = 4;

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
    kind_at: usize,
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
        let declared = word(bytes, entry.saturating_add(kind_at))
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
    shape_of(&bytes).map_err(|source| format!("{}: {source}", path.display()).into())
}

/// The same, of bytes already in hand — which is what makes the walk provable
/// without building an artifact for every shape it must tell apart.
///
/// # Errors
///
/// When the bytes are not an ELF file, or a header table cannot be walked.
fn shape_of(bytes: &[u8]) -> Result<Shape, Failed> {
    if bytes.get(0..ELF_MAGIC.len()) != Some(ELF_MAGIC) {
        return Err("not an ELF file".into());
    }
    let machine = half(bytes, MACHINE_AT).ok_or("the ELF header ends before its machine")?;
    let programs = offset(bytes, PROGRAM_HEADERS_AT)
        .ok_or("the ELF header ends before its program headers")?;
    let interpreted = declares(
        bytes,
        programs,
        usize::from(half(bytes, HEADER_SIZE_AT).unwrap_or_default()),
        usize::from(half(bytes, HEADER_COUNT_AT).unwrap_or_default()),
        PROGRAM_KIND_AT,
        PROGRAM_INTERPRETER,
    )?;
    let sections =
        offset(bytes, SECTION_HEADERS_AT).ok_or("the ELF header ends before its sections")?;
    let symbols = declares(
        bytes,
        sections,
        usize::from(half(bytes, SECTION_SIZE_AT).unwrap_or_default()),
        usize::from(half(bytes, SECTION_COUNT_AT).unwrap_or_default()),
        SECTION_KIND_AT,
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

/// How long an ELF64 header is, and so where the tables after it can start.
const ELF_HEADER_LENGTH: usize = 64;

/// How long one program header is.
const PROGRAM_HEADER_LENGTH: usize = 56;

/// How long one section header is.
const SECTION_HEADER_LENGTH: usize = 64;

/// The program header type of a loadable segment: what a file has instead of
/// an interpreter when it names no loader.
const PROGRAM_LOAD: u32 = 1;

/// The section type of ordinary contents: what a file has instead of a symbol
/// table when it has been stripped.
const SECTION_PROGRAM_BITS: u32 = 1;

/// An ELF64 executable whose program headers are `programs` and whose section
/// headers are `sections`, and nothing else.
///
/// The walk above reads two tables whose entries carry their type in different
/// places, and a walk that read one offset for both would find no loader in
/// anything. This is what says it does not.
fn synthetic(machine: u16, programs: &[u32], sections: &[u32]) -> Vec<u8> {
    let at = ELF_HEADER_LENGTH;
    let sections_at = at.saturating_add(programs.len().saturating_mul(PROGRAM_HEADER_LENGTH));
    let mut bytes = Vec::new();
    bytes.extend_from_slice(ELF_MAGIC);
    // Sixty-four-bit, little-endian, version one, and the rest of the
    // identification zero.
    bytes.extend_from_slice(&[2, 1, 1]);
    bytes.resize(16, 0);
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&machine.to_le_bytes());
    bytes.extend_from_slice(&1_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u64.to_le_bytes());
    bytes.extend_from_slice(&u64::try_from(at).unwrap_or_default().to_le_bytes());
    bytes.extend_from_slice(&u64::try_from(sections_at).unwrap_or_default().to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(
        &u16::try_from(ELF_HEADER_LENGTH)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u16::try_from(PROGRAM_HEADER_LENGTH)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u16::try_from(programs.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u16::try_from(SECTION_HEADER_LENGTH)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u16::try_from(sections.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    for kind in programs {
        // The type, then flags of 3 where a section header would carry its
        // type: a walk that read the wrong offset would find a symbol table
        // in every segment, and this is what catches it.
        let mut header = Vec::new();
        header.extend_from_slice(&kind.to_le_bytes());
        header.extend_from_slice(&SECTION_SYMBOLS.to_le_bytes());
        header.resize(PROGRAM_HEADER_LENGTH, 0);
        bytes.extend_from_slice(&header);
    }
    for kind in sections {
        // The name, then the type: the other way round from a program header.
        let mut header = Vec::new();
        header.extend_from_slice(&PROGRAM_INTERPRETER.to_le_bytes());
        header.extend_from_slice(&kind.to_le_bytes());
        header.resize(SECTION_HEADER_LENGTH, 0);
        bytes.extend_from_slice(&header);
    }
    bytes
}

/// # Panics
///
/// When the walk does not find an interpreter that is there, finds one that is
/// not, does not find a symbol table that is there, finds one that is not, or
/// accepts a file whose tables it cannot walk.
///
/// It reads the two tables at different offsets, and a single offset for both
/// makes every artifact look static and stripped — which is what the artifact
/// cases assert and which no build of this workspace would ever contradict.
#[test]
fn elf_shape_reads_both_header_tables() {
    let case = |programs: &[u32], sections: &[u32]| -> Result<Shape, Failed> {
        shape_of(&synthetic(MACHINE_X86_64, programs, sections))
    };
    let dynamic = case(
        &[PROGRAM_LOAD, PROGRAM_INTERPRETER],
        &[SECTION_PROGRAM_BITS],
    )
    .unwrap_or_else(|error| panic!("a synthetic ELF file was refused: {error}"));
    assert_eq!(dynamic.machine, MACHINE_X86_64, "the machine is read");
    assert!(dynamic.interpreted, "and a program header naming a loader");
    assert!(!dynamic.symbols, "and no symbol table where there is none");
    let stripped = case(&[PROGRAM_LOAD], &[SECTION_PROGRAM_BITS, SECTION_SYMBOLS])
        .unwrap_or_else(|error| panic!("a synthetic ELF file was refused: {error}"));
    assert!(!stripped.interpreted, "a file naming no loader is static");
    assert!(stripped.symbols, "and a symbol table where there is one");
    assert!(
        case(&[], &[SECTION_PROGRAM_BITS]).is_err(),
        "a table of no entries is refused rather than answered"
    );
    assert!(
        shape_of(b"not an ELF file at all").is_err(),
        "and so is something that is not an ELF file"
    );
}

/// Where the protocol version is declared, and the line it is declared on.
const PROTOCOL_SOURCE: &str = "crates/iznik-protocol/src/message.rs";

/// What that line begins with.
const PROTOCOL_DECLARATION: &str = "pub const PROTOCOL_VERSION: u16 = ";

/// The protocol version the source declares, read here rather than taken from
/// the manifest so that the manifest is held to something other than itself.
///
/// # Errors
///
/// When the source cannot be read or does not declare it.
fn declared_protocol_version(root: &Path) -> Result<u16, Failed> {
    let source = std::fs::read_to_string(root.join(PROTOCOL_SOURCE))?;
    source
        .lines()
        .find_map(|line| line.trim().strip_prefix(PROTOCOL_DECLARATION))
        .and_then(|rest| rest.trim_end_matches(';').trim().parse().ok())
        .ok_or_else(|| format!("{PROTOCOL_SOURCE} declares no protocol version").into())
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
        let declared = declared_protocol_version(&root)?;
        assert!(
            manifest.contains(&format!("protocol_version = {declared}")),
            "and the protocol version the source declares, {declared}: {manifest}"
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
