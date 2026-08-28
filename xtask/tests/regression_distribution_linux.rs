//! The Linux artifacts, held to what a bootstrap needs of them: statically
//! linked so a host's libc version does not matter, stripped and small enough
//! to upload, reproducible so a digest is worth having, and — for the one this
//! machine can run — able to say its version inside the host container.
//!
//! Every case here is ignored by default: each builds a release artifact,
//! which takes minutes and is not something the ordinary gate should do. What
//! the ELF reading itself says is proven in `artifact_shape`, which builds
//! nothing and runs in that gate.

use std::path::Path;

use iznik_harness::fixture::{Fixture, FixtureOptions};
use iznik_harness::process::Deadline;
use iznik_harness::staging;
use xtask::distribution::shape::elf_shape;
use xtask::distribution::{
    ARTIFACT_SIZE_CEILING, Artifact, BINARY, CHECKSUMS, MANIFEST, build, digest_of,
    protocol_version, workspace_root,
};

/// The triple this machine can also run.
const NATIVE: &str = "x86_64-unknown-linux-musl";

/// The triple it can only build.
const CROSS: &str = "aarch64-unknown-linux-musl";

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
    let shape = elf_shape(&std::fs::read(&artifact.path)?)?;
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
        let declared = protocol_version(&root)?;
        // With the newline, so a manifest that wrote 10 where 1 was declared
        // is not a prefix that passes.
        assert!(
            manifest.contains(&format!("protocol_version = {declared}\n")),
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
