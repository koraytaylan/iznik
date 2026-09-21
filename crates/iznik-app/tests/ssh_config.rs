//! Reading the aliases a person's ssh configuration names, and appending the
//! one block Add Host asks to write.

use iznik_app::ssh_config::{
    SshConfigError, aliases, append_host, defines, known_hosts, parsed_aliases, parsed_known_hosts,
};
use std::path::PathBuf;

/// Fixture filesystem failures.
type Failed = Box<dyn std::error::Error>;

/// A scratch directory named after the case, removed when the test ends.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// A scratch directory for one case.
///
/// # Errors
///
/// Returns a filesystem failure.
fn scratch(name: &str) -> Result<Scratch, Failed> {
    let directory =
        std::env::temp_dir().join(format!("iznik-ssh-config-{name}-{}", std::process::id()));
    let _removed = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory)?;
    Ok(Scratch(directory))
}

#[test]
/// A configuration's concrete host patterns are read in order, and wildcards,
/// negations and matches are not hosts.
///
/// # Panics
///
/// Panics when the aliases read differ.
fn parsed_aliases_keeps_plain_hosts_in_order() {
    let text = "\
# a comment line
Host alpha
    HostName alpha.example
Host beta gamma
    User someone
host Delta
Host *
Host !refused
Host web-?
Host alpha
";
    assert_eq!(
        parsed_aliases(text),
        ["alpha", "beta", "gamma", "Delta"],
        "wildcards, negations and repeats are not hosts"
    );
}

#[test]
/// An absent file defines no aliases, and a file's own aliases are read back.
///
/// # Errors
///
/// Returns a filesystem failure.
///
/// # Panics
///
/// Panics when an absent file is not empty or a written file is not read.
fn aliases_reads_a_file_and_an_absent_one_is_empty() -> Result<(), Failed> {
    let scratch = scratch("read")?;
    let path = scratch.0.join("config");
    assert_eq!(aliases(&path)?, Vec::<String>::new());
    std::fs::write(&path, "Host devbox\n    HostName 10.0.0.1\n")?;
    assert_eq!(aliases(&path)?, ["devbox"]);
    assert!(defines("Host devbox\n", "devbox"));
    assert!(defines("Host DevBox\n", "devbox"), "matching is case-free");
    assert!(!defines("Host other\n", "devbox"));
    Ok(())
}

#[test]
/// The hosts a `known_hosts` text remembers are read in order, with
/// duplicates, hashed names, bracketed ports and comment lines left out.
///
/// # Panics
///
/// Panics when the names read differ.
fn parsed_known_hosts_keeps_the_names_a_person_typed() {
    let text = "\
# a comment
github.com ssh-ed25519 AAAA
|1|aGFzaGVkIG5hbWU=|c2FsdA= ssh-ed25519 AAAA
workstation,10.0.0.9 ssh-ed25519 AAAA
[host.example]:2222 ssh-ed25519 AAAA
github.com ssh-rsa AAAA
10.0.0.9 ecdsa-sha2-nistp256 AAAA
cafe.be ssh-ed25519 AAAA
";
    assert_eq!(
        parsed_known_hosts(text),
        ["github.com", "workstation", "cafe.be"],
        "names lead, addresses stay out, and a name that looks like hex is still a name"
    );
}

#[test]
/// The configuration's hosts lead, and every host ssh's record remembers
/// that the configuration does not is added after them.
///
/// # Errors
///
/// Returns a filesystem failure.
///
/// # Panics
///
/// Panics when the merged host list differs.
fn known_hosts_puts_the_record_after_the_configuration() -> Result<(), Failed> {
    let scratch = scratch("known")?;
    let path = scratch.0.join("config");
    let known = scratch.0.join("known_hosts");
    std::fs::write(&path, "Host devbox\n")?;
    std::fs::write(
        &known,
        "workstation ssh-ed25519 AAAA\nDEvbox ssh-ed25519 AAAA\n",
    )?;
    assert_eq!(known_hosts(&path, &known)?, ["devbox", "workstation"]);
    assert_eq!(
        known_hosts(&path, &scratch.0.join("absent"))?,
        ["devbox"],
        "an absent record adds nothing"
    );
    Ok(())
}

#[test]
/// Appending writes a `Host` block with its `HostName` and leaves every
/// earlier byte exactly where it was.
///
/// # Errors
///
/// Returns a filesystem failure.
///
/// # Panics
///
/// Panics when the block is not appended as written.
fn appends_a_block_after_what_was_there() -> Result<(), Failed> {
    let scratch = scratch("append")?;
    let path = scratch.0.join("config");
    let before = "Host alpha\n    HostName alpha.example\n";
    std::fs::write(&path, before)?;
    append_host(&path, "devbox", "10.0.0.1")?;
    let after = std::fs::read_to_string(&path)?;
    assert!(
        after.starts_with(before),
        "the earlier configuration is untouched: {after:?}"
    );
    assert!(
        after.ends_with("\nHost devbox\n    HostName 10.0.0.1\n"),
        "{after:?}"
    );
    assert_eq!(parsed_aliases(&after), ["alpha", "devbox"]);
    Ok(())
}

#[test]
/// Appending to a file that is not there creates it, and a directory that is
/// not there with it.
///
/// # Errors
///
/// Returns a filesystem failure.
///
/// # Panics
///
/// Panics when the created file is not the block alone.
fn appends_and_makes_the_file_and_its_directory() -> Result<(), Failed> {
    let scratch = scratch("create")?;
    let path = scratch.0.join(".ssh").join("config");
    append_host(&path, "devbox", "10.0.0.1")?;
    assert_eq!(
        std::fs::read_to_string(&path)?,
        "\nHost devbox\n    HostName 10.0.0.1\n"
    );
    Ok(())
}

#[test]
/// An alias the file already defines is refused, and the file is untouched.
///
/// # Errors
///
/// Returns a filesystem failure.
///
/// # Panics
///
/// Panics when a duplicate is not refused by name or the file changed.
fn refuses_an_alias_already_defined() -> Result<(), Failed> {
    let scratch = scratch("duplicate")?;
    let path = scratch.0.join("config");
    std::fs::write(&path, "Host devbox\n    HostName elsewhere\n")?;
    let refused = append_host(&path, "devbox", "10.0.0.1");
    assert!(
        matches!(&refused, Err(SshConfigError::AliasAlreadyDefined { alias }) if alias == "devbox"),
        "{refused:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&path)?,
        "Host devbox\n    HostName elsewhere\n"
    );
    Ok(())
}

#[test]
/// An alias that is no one host, and an address that cannot stand on its own
/// line, are refused naming what is wrong.
///
/// # Errors
///
/// Returns a filesystem failure.
///
/// # Panics
///
/// Panics when a refused alias or address is accepted, or the file changed.
fn refuses_an_alias_or_address_that_cannot_be_written() -> Result<(), Failed> {
    let scratch = scratch("refuse")?;
    let path = scratch.0.join("config");
    for alias in ["", "alpha beta", "web-*", "tag#"] {
        let refused = append_host(&path, alias, "10.0.0.1");
        assert!(
            matches!(refused, Err(SshConfigError::AliasNotPlain { .. })),
            "{alias:?} is refused: {refused:?}"
        );
    }
    for address in ["", "10.0.0.1 10.0.0.2", "host#name"] {
        let refused = append_host(&path, "devbox", address);
        assert!(
            matches!(refused, Err(SshConfigError::AddressNotPlain { .. })),
            "{address:?} is refused: {refused:?}"
        );
    }
    assert!(!path.exists(), "nothing was written");
    Ok(())
}
