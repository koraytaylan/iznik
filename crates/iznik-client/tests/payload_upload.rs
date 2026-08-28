//! What is loaded from a distribution directory, and what is sent to a host.
//!
//! Both are things this machine can look at: a directory of files with digests
//! computed from their bytes, and the text of a script. What happens when a
//! host runs that script — the refusal on a digest that does not match, the
//! link that drops in the middle, the terminfo compiled beside the binary — is
//! a scenario, because those are properties of a host and not of a string.

use std::path::PathBuf;

use iznik_client::bootstrap::upload::{
    ArtifactSet, BINARY_NAME, REMOTE_UPLOAD_SCRIPT, UPLOAD_CHUNK_LENGTH, UploadError, hexadecimal,
    remote_command,
};

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A temporary directory of this case's own, removed when the guard drops.
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A distribution directory named for `case`, holding one artifact per triple
/// with the bytes each name says.
///
/// # Errors
///
/// When it cannot be made or written.
fn distribution(case: &str, artifacts: &[(&str, &[u8])]) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-upload-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    for (triple, bytes) in artifacts {
        let held = path.join(triple);
        std::fs::create_dir_all(&held)?;
        std::fs::write(held.join(BINARY_NAME), bytes)?;
    }
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// # Panics
///
/// When a distribution directory is not loaded as one artifact per triple with
/// the digest of each file's own bytes.
#[test]
fn payload_upload_loads_one_artifact_for_every_triple() {
    let case = || -> Result<(), Failed> {
        let held = distribution(
            "loads",
            &[
                ("x86_64-unknown-linux-musl", b"one"),
                ("aarch64-unknown-linux-musl", b"another"),
            ],
        )?;
        let loaded = ArtifactSet::load(&held.path)?;
        assert_eq!(
            loaded.triples(),
            vec!["aarch64-unknown-linux-musl", "x86_64-unknown-linux-musl"],
            "both triples, in a settled order"
        );
        let one = loaded.for_triple("x86_64-unknown-linux-musl")?;
        assert_eq!(
            hexadecimal(&one.digest),
            // `printf one | sha256sum`
            "7692c3ad3540bb803c020b3aee66cd8887123234ea0c6e7143c0add73ff431ed",
            "and the digest of the file's own bytes, not of anything it was told"
        );
        assert!(
            one.path.ends_with(BINARY_NAME),
            "and where the file is: {}",
            one.path.display()
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a triple that is not there is not refused naming the ones that are.
#[test]
fn payload_upload_refuses_a_triple_it_does_not_have() {
    let case = || -> Result<(), Failed> {
        let held = distribution("refuses", &[("x86_64-unknown-linux-musl", b"one")])?;
        let loaded = ArtifactSet::load(&held.path)?;
        let refused = loaded.for_triple("aarch64-apple-darwin");
        let Err(UploadError::NoArtifact { triple, held: has }) = refused else {
            return Err(format!("a missing triple was not refused: {refused:?}").into());
        };
        assert_eq!(triple, "aarch64-apple-darwin", "it names what was wanted");
        assert_eq!(
            has,
            vec!["x86_64-unknown-linux-musl"],
            "and what this build actually has, so a person is not left guessing"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a directory with nothing in it is not loaded as nothing.
#[test]
fn payload_upload_loads_an_empty_directory_as_empty() {
    let case = || -> Result<(), Failed> {
        let held = distribution("empty", &[])?;
        let loaded = ArtifactSet::load(&held.path)?;
        assert!(loaded.is_empty(), "nothing there is nothing loaded");
        let refused = loaded.for_triple("x86_64-unknown-linux-musl");
        assert!(
            matches!(refused, Err(UploadError::NoArtifact { .. })),
            "and asking for one says so: {refused:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When the command sent to a host does not carry the digest this client
/// computed, or names the final path before the rename that puts it there.
#[test]
fn payload_upload_sends_a_script_that_names_the_digest_and_nothing_early() {
    let digest = "7692c3ad3540bb803c020b3aee66cd8887123234ea0c6e7143c0add73ff431ed";
    let command = remote_command(
        REMOTE_UPLOAD_SCRIPT,
        std::path::Path::new("/home/iznik/.local/share/iznik"),
        digest,
    );
    assert!(
        command.contains(digest),
        "the host is told the digest to check against: {command}"
    );
    assert!(
        command.contains("/home/iznik/.local/share/iznik"),
        "and the prefix to install under: {command}"
    );
    let renamed = REMOTE_UPLOAD_SCRIPT
        .find("mv ")
        .unwrap_or(REMOTE_UPLOAD_SCRIPT.len());
    let first_named = REMOTE_UPLOAD_SCRIPT
        .find(BINARY_NAME)
        .unwrap_or(REMOTE_UPLOAD_SCRIPT.len());
    assert!(
        first_named >= renamed,
        "the name it will finally have is written once, on the rename: a link that \
         drops leaves a partial and never something a later run would execute"
    );
    assert!(
        REMOTE_UPLOAD_SCRIPT.contains(".partial-"),
        "which is what the partial name is for"
    );
    assert!(
        REMOTE_UPLOAD_SCRIPT.contains("rm -f \"$into\"/.partial-*"),
        "and a stale one from a dropped link before it is cleared first"
    );
}

/// # Panics
///
/// When the chunk the artifact is read in is not the size the architecture
/// names, which is the size a host's shell is asked to swallow at once.
#[test]
fn payload_upload_reads_in_the_chunk_it_says() {
    assert_eq!(
        UPLOAD_CHUNK_LENGTH,
        1024 * 1024,
        "a mebibyte: large enough that the reads are not the cost, small enough \
         that a shell's buffer is not the limit"
    );
}
