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

/// The `e_machine` an ELF file declares, and whether any of its program
/// headers names an interpreter — which a statically linked executable has
/// none of.
///
/// The headers, not the strings: musl's loader path appears in a static
/// binary's data, so searching for it would call every artifact dynamic.
///
/// # Errors
///
/// When the file cannot be read or is not an ELF file at all.
fn elf_shape(path: &Path) -> Result<(u16, bool), Failed> {
    let bytes = std::fs::read(path)?;
    if bytes.get(0..ELF_MAGIC.len()) != Some(ELF_MAGIC) {
        return Err(format!("{} is not an ELF file", path.display()).into());
    }
    let half = |at: usize| -> Option<u16> {
        bytes
            .get(at..at.saturating_add(2))
            .and_then(|held| <[u8; 2]>::try_from(held).ok())
            .map(u16::from_le_bytes)
    };
    let word = |at: usize| -> Option<u32> {
        bytes
            .get(at..at.saturating_add(4))
            .and_then(|held| <[u8; 4]>::try_from(held).ok())
            .map(u32::from_le_bytes)
    };
    let machine = half(MACHINE_AT).ok_or("the ELF header ends before its machine")?;
    let at = bytes
        .get(PROGRAM_HEADERS_AT..PROGRAM_HEADERS_AT.saturating_add(8))
        .and_then(|held| <[u8; 8]>::try_from(held).ok())
        .map(u64::from_le_bytes)
        .and_then(|offset| usize::try_from(offset).ok())
        .ok_or("the ELF header ends before its program headers")?;
    let stride = usize::from(half(HEADER_SIZE_AT).unwrap_or_default());
    let count = usize::from(half(HEADER_COUNT_AT).unwrap_or_default());
    let interpreted = (0..count).any(|index| {
        at.checked_add(index.saturating_mul(stride))
            .and_then(word)
            .is_some_and(|kind| kind == PROGRAM_INTERPRETER)
    });
    Ok((machine, interpreted))
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
    assert!(
        artifact.bytes < ARTIFACT_SIZE_CEILING,
        "{target} is {} bytes, over the {ARTIFACT_SIZE_CEILING} ceiling",
        artifact.bytes
    );
    let (declared, interpreted) = elf_shape(&artifact.path)?;
    assert_eq!(declared, machine, "{target} declares the machine it is for");
    assert!(!interpreted, "{target} names no loader: it is static");
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
