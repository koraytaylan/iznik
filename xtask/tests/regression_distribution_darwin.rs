//! The Darwin artifacts, and what can be said about them from a machine that
//! is not a Mac.
//!
//! Cross-compiling to Darwin needs an SDK a Linux machine does not have, so
//! most of what is claimed here is deferred rather than proven: the claims
//! carry `platform = "darwin"`, and the workflow builds them where the
//! toolchain is native. What *can* be proven anywhere is the diagnostic — that
//! a missing toolchain is named rather than reported as a link error from deep
//! inside cargo — and that is the case that runs here.

use std::path::Path;

use xtask::distribution::{
    ARTIFACT_SIZE_CEILING, DISTRIBUTION_DIRECTORY, DistributionError, MANIFEST, build, darwin,
    digest_of, workspace_root,
};

/// The triples this is about.
const TARGETS: &[&str] = darwin::TARGETS;

/// What a Mach-O file begins with, little-endian and 64-bit.
const MACH_MAGIC: u32 = 0xfeed_facf;

/// The CPU type of an Apple silicon Mac.
const CPU_ARM64: u32 = 0x0100_000c;

/// The CPU type of an Intel Mac.
const CPU_X86_64: u32 = 0x0100_0007;

/// How long a 64-bit Mach-O header is, and so where its load commands start.
const COMMANDS_AT: usize = 32;

/// Where the count of load commands is in that header.
const COMMAND_COUNT_AT: usize = 16;

/// The load command that names a library the file needs at run time.
const LOAD_DYLIB: u32 = 0x0000_000c;

/// It and the two of the same shape beside it: a weak dependency and a
/// re-export.
const LOAD_LIBRARY: &[u32] = &[LOAD_DYLIB, 0x8000_0018, 0x8000_001f];

/// A load command that names no library. A segment is most of what a real
/// Mach-O file's commands are, and stepping over them by the length they
/// declare is the part of the walk that can go wrong unnoticed.
const LOAD_SEGMENT: u32 = 0x0000_0019;

/// How long one of those is with no sections in it.
const SEGMENT_LENGTH: usize = 72;

/// What a load command's name is padded to, which is how a linker lays one
/// out and so what the walk has to step over.
const NAME_ALIGNMENT: usize = 4;

/// Where, within one of those commands, the offset of its name is; and where
/// the command's own length is, which is how the walk advances.
const NAME_OFFSET_AT: usize = 8;

/// Where a load command's length is, in every load command.
const COMMAND_SIZE_AT: usize = 4;

/// The file type of an executable, which is what an artifact is.
const MACH_EXECUTABLE: u32 = 2;

/// The system libraries a stock macOS install has, which are the only ones an
/// artifact may name.
///
/// The list is what the criterion allows, not a guess at what this binary
/// happens to link: every one of these ships with the operating system, so a
/// host that has macOS has them. An artifact that named something else would
/// be one a person had to install a dependency for, which is the whole thing a
/// distributed binary must not be.
const ALLOWED_LIBRARIES: &[&str] = &[
    "/usr/lib/libSystem.B.dylib",
    "/usr/lib/libiconv.2.dylib",
    "/usr/lib/libresolv.9.dylib",
    "/usr/lib/libc++.1.dylib",
    "/usr/lib/libobjc.A.dylib",
    "/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation",
    "/System/Library/Frameworks/Security.framework/Versions/A/Security",
];

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A little-endian word at `at`, if the file reaches that far.
fn word(bytes: &[u8], at: usize) -> Option<u32> {
    bytes
        .get(at..at.saturating_add(4))
        .and_then(|held| <[u8; 4]>::try_from(held).ok())
        .map(u32::from_le_bytes)
}

/// The name at `at`, if there is one and it is terminated.
///
/// A run of bytes to the end of the file is not a name: a file that stopped
/// mid-string is truncated, and reporting the rest of it as a library would
/// be an answer where there should be a refusal.
fn name(bytes: &[u8], at: usize) -> Option<String> {
    let rest = bytes.get(at..)?;
    let end = rest.iter().position(|byte| *byte == 0)?;
    rest.get(..end)
        .map(|held| String::from_utf8_lossy(held).into_owned())
}

/// The CPU type a Mach-O file declares and every library it names, which is
/// its whole dynamic-link surface.
///
/// The load commands, not the strings: a path that happens to appear in the
/// binary's data is not something it links.
///
/// # Errors
///
/// When the file cannot be read or is not a 64-bit Mach-O file at all.
fn mach_shape(path: &Path) -> Result<(u32, Vec<String>), Failed> {
    let bytes = std::fs::read(path)?;
    shape_of(&bytes).map_err(|source| format!("{}: {source}", path.display()).into())
}

/// The same, of bytes already in hand — which is what makes the walk provable
/// on a machine that cannot build a Mach-O file.
///
/// # Errors
///
/// When the bytes are not a 64-bit Mach-O file, or its load commands run past
/// the end of them.
fn shape_of(bytes: &[u8]) -> Result<(u32, Vec<String>), Failed> {
    if word(bytes, 0) != Some(MACH_MAGIC) {
        return Err("not a 64-bit Mach-O file".into());
    }
    let cpu = word(bytes, 4).ok_or("the Mach-O header ends before its CPU type")?;
    let count = word(bytes, COMMAND_COUNT_AT).ok_or("and before its command count")?;
    let mut linked = Vec::new();
    let mut at = COMMANDS_AT;
    for _each in 0..count {
        let kind = word(bytes, at).ok_or("a load command begins past the end of the file")?;
        let size = word(bytes, at.saturating_add(COMMAND_SIZE_AT))
            .and_then(|held| usize::try_from(held).ok())
            .filter(|held| *held > 0)
            .ok_or("a load command declares no length, and the walk cannot go on")?;
        if LOAD_LIBRARY.contains(&kind) {
            let offset = word(bytes, at.saturating_add(NAME_OFFSET_AT))
                .and_then(|held| usize::try_from(held).ok())
                .and_then(|held| at.checked_add(held))
                .ok_or("a library command names nothing")?;
            linked.push(name(bytes, offset).ok_or("a library name begins past the end")?);
        }
        at = at.saturating_add(size);
    }
    Ok((cpu, linked))
}

/// One load command that names a library, laid out as a linker lays it out:
/// the fixed part, then the name, padded to a multiple of four.
fn dylib_command(library: &str) -> Vec<u8> {
    let fixed = NAME_OFFSET_AT.saturating_add(16);
    let mut named = library.as_bytes().to_vec();
    named.push(0);
    while !named.len().is_multiple_of(NAME_ALIGNMENT) {
        named.push(0);
    }
    let size = fixed.saturating_add(named.len());
    let mut command = Vec::new();
    command.extend_from_slice(&LOAD_DYLIB.to_le_bytes());
    command.extend_from_slice(&u32::try_from(size).unwrap_or_default().to_le_bytes());
    command.extend_from_slice(&u32::try_from(fixed).unwrap_or_default().to_le_bytes());
    command.extend_from_slice(&0_u32.to_le_bytes());
    command.extend_from_slice(&0_u32.to_le_bytes());
    command.extend_from_slice(&0_u32.to_le_bytes());
    command.extend_from_slice(&named);
    command
}

/// One load command that names nothing: a segment, all zeros after its kind
/// and its length. It is here so the walk has to step over something by the
/// length it declares rather than by the length of what it has just read.
fn segment_command() -> Vec<u8> {
    let mut command = Vec::new();
    command.extend_from_slice(&LOAD_SEGMENT.to_le_bytes());
    command.extend_from_slice(
        &u32::try_from(SEGMENT_LENGTH)
            .unwrap_or_default()
            .to_le_bytes(),
    );
    command.resize(SEGMENT_LENGTH, 0);
    command
}

/// A 64-bit Mach-O file whose load commands are, in order, what `libraries`
/// says: a name for one the file needs, and nothing for a segment.
fn synthetic(cpu: u32, libraries: &[Option<&str>]) -> Vec<u8> {
    let mut commands = Vec::new();
    for library in libraries {
        match *library {
            Some(named) => commands.extend_from_slice(&dylib_command(named)),
            None => commands.extend_from_slice(&segment_command()),
        }
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MACH_MAGIC.to_le_bytes());
    bytes.extend_from_slice(&cpu.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&MACH_EXECUTABLE.to_le_bytes());
    bytes.extend_from_slice(
        &u32::try_from(libraries.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(
        &u32::try_from(commands.len())
            .unwrap_or_default()
            .to_le_bytes(),
    );
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&0_u32.to_le_bytes());
    bytes.extend_from_slice(&commands);
    bytes
}

/// The CPU type the triple names.
fn cpu_of(target: &str) -> Option<u32> {
    match target {
        "aarch64-apple-darwin" => Some(CPU_ARM64),
        "x86_64-apple-darwin" => Some(CPU_X86_64),
        _other => None,
    }
}

/// # Panics
///
/// When a Darwin build without an SDK does not say which component is
/// missing, or leaves an artifact behind.
#[ignore = "builds release artifacts where a toolchain allows; run deliberately"]
#[test]
fn distribution_names_the_missing_darwin_toolchain() {
    let case = || -> Result<(), Failed> {
        if std::env::var_os("SDKROOT").is_some() {
            // A machine that has one proves the other cases instead.
            return Ok(());
        }
        let root = workspace_root();
        for target in TARGETS {
            let refused = build(&root, target);
            let Err(DistributionError::Build {
                target: named,
                detail,
            }) = refused
            else {
                return Err(format!("{target} was not refused: {refused:?}").into());
            };
            assert_eq!(&named, target, "the refusal names the target");
            assert!(
                detail.contains("SDKROOT") && detail.contains("darwin-artifacts.yml"),
                "and says what is missing and where these are built: {detail}"
            );
            let left = xtask::distribution::target_directory(&root)
                .join(DISTRIBUTION_DIRECTORY)
                .join(target)
                .join(MANIFEST);
            assert!(
                !left.exists(),
                "and leaves no partial artifact: {}",
                left.display()
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a Darwin artifact is not a Mach-O executable for the machine it names,
/// is over the size ceiling, or links something a stock macOS install lacks.
#[ignore = "needs a Darwin toolchain; the workflow runs it on a macOS runner"]
#[test]
fn distribution_builds_both_darwin_targets() {
    let case = || -> Result<(), Failed> {
        if std::env::var_os("SDKROOT").is_none() {
            // Deferred, not skipped: the claim carries `platform = "darwin"`
            // and reports as such, and this says the same thing in the runner.
            return Ok(());
        }
        let root = workspace_root();
        for target in TARGETS {
            let cpu = cpu_of(target).ok_or_else(|| format!("{target} has no known CPU type"))?;
            let artifact = build(&root, target)?;
            assert!(
                artifact.bytes < ARTIFACT_SIZE_CEILING,
                "{target} is {} bytes, over the {ARTIFACT_SIZE_CEILING} ceiling",
                artifact.bytes
            );
            let (declared, linked) = mach_shape(&artifact.path)?;
            assert_eq!(declared, cpu, "{target} declares the machine it is for");
            let unexpected: Vec<&String> = linked
                .iter()
                .filter(|library| !ALLOWED_LIBRARIES.contains(&library.as_str()))
                .collect();
            assert!(
                unexpected.is_empty(),
                "{target} links what a stock macOS install may not have: {unexpected:?}"
            );
            assert!(
                !linked.is_empty(),
                "{target} links nothing at all, which no Mach-O executable does: \
                 the load commands were not read"
            );
            assert_eq!(
                digest_of(&artifact.path)?,
                artifact.digest,
                "and its digest is its own"
            );
        }
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the load-command walk does not find exactly the libraries a Mach-O
/// file names, or accepts something that is not one.
///
/// The case above needs a Mac to build what it reads, so this proves the
/// reading itself here: a walk that silently found nothing would let that one
/// pass on the runner with its allowlist never consulted.
#[test]
fn mach_shape_reads_what_a_file_names() {
    let first = "/usr/lib/libSystem.B.dylib";
    let second = "/usr/lib/libiconv.2.dylib";
    // Segments around and between them, so the two names can be found only by
    // stepping over commands of another kind and another length.
    let file = synthetic(
        CPU_ARM64,
        &[None, Some(first), None, None, Some(second), None],
    );
    let (cpu, linked) = shape_of(&file)
        .unwrap_or_else(|error| panic!("a synthetic Mach-O file was refused: {error}"));
    assert_eq!(cpu, CPU_ARM64, "the CPU type is read from the header");
    assert_eq!(linked, [first, second], "and the libraries, in order");
    assert!(
        linked
            .iter()
            .all(|library| ALLOWED_LIBRARIES.contains(&library.as_str())),
        "which the allowlist above is written against"
    );
    let (_cpu, none) = shape_of(&synthetic(CPU_X86_64, &[None, None]))
        .unwrap_or_else(|error| panic!("a file of segments was refused: {error}"));
    assert!(
        none.is_empty(),
        "a file whose commands name no library yields none: {none:?}"
    );
    assert!(
        shape_of(b"\x7fELF\x02\x01\x01").is_err(),
        "and something that is not a Mach-O file at all is refused"
    );
    let mut truncated = file;
    truncated.truncate(truncated.len().saturating_sub(SEGMENT_LENGTH));
    assert!(
        shape_of(&truncated).is_err(),
        "as is a file whose commands run past its end"
    );
}
