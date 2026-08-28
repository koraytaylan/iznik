//! The Darwin artifacts, and what can be said about them from a machine that
//! is not a Mac.
//!
//! Cross-compiling to Darwin needs an SDK a Linux machine does not have, so
//! most of what is claimed here is deferred rather than proven: the claims
//! carry `platform = "darwin"`, and the workflow builds them where the
//! toolchain is native. What *can* be proven anywhere is the diagnostic — that
//! a missing toolchain is named rather than reported as a link error from deep
//! inside cargo — and that is the case that runs here. What the load-command
//! walk itself says is proven in `artifact_shape`, which needs no Mac.

use xtask::distribution::shape::mach_shape;
use xtask::distribution::{
    ARTIFACT_SIZE_CEILING, DISTRIBUTION_DIRECTORY, DistributionError, MANIFEST, build, darwin,
    digest_of, workspace_root,
};

/// The triples this is about.
const TARGETS: &[&str] = darwin::TARGETS;

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The CPU type of an Apple silicon Mac.
const CPU_ARM64: u32 = 0x0100_000c;

/// The CPU type of an Intel Mac.
const CPU_X86_64: u32 = 0x0100_0007;

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
            let shape = mach_shape(&std::fs::read(&artifact.path)?)?;
            assert_eq!(shape.cpu, cpu, "{target} declares the machine it is for");
            let unexpected: Vec<&String> = shape
                .linked
                .iter()
                .filter(|library| !ALLOWED_LIBRARIES.contains(&library.as_str()))
                .collect();
            assert!(
                unexpected.is_empty(),
                "{target} links what a stock macOS install may not have: {unexpected:?}"
            );
            assert!(
                !shape.linked.is_empty(),
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
