//! The person's own ssh configuration: the hosts it and ssh's own record
//! name, and the one block this application appends when asked.
//!
//! iznik never interprets a configuration — `ssh` does that, and the client's
//! transport hands it the alias untouched. What this module reads is only what
//! a person must see before choosing: the aliases their configuration names
//! and the names `known_hosts` remembers, so the welcome can offer them.
//! What it writes is the small block a person asks for when they add a host
//! neither file names yet.
//!
//! A host pattern that matches more than itself — one holding `*`, `?` or `!`
//! — is not a host a person can connect to, so it is never offered and never
//! written. A name in `known_hosts` that is hashed or carries its port cannot
//! be handed to `ssh` as an alias, so it is not offered either. Nothing here
//! follows an `Include`: an alias defined in an included file is not in the
//! text read, which is why a person who typed it anyway is asked for an
//! address rather than refused.

use core::fmt::{self, Display, Formatter};
use std::io::Write as _;
use std::path::{Path, PathBuf};

/// The directory the ssh configuration lives in, under the home directory.
const SSH_DIRECTORY: &str = ".ssh";

/// The file name of the ssh configuration.
const CONFIG_NAME: &str = "config";

/// The file name of ssh's own record of the hosts its people have reached,
/// which is the second place a host a person means can be named.
const KNOWN_HOSTS_NAME: &str = "known_hosts";

/// The marker that begins the hash of a host whose name `ssh` deliberately
/// does not write down, which is not one this can offer.
const HASH_MARKER: &str = "|1|";

/// The character that begins a bracketed host, which carries its port and is
/// no alias a person typed.
const BRACKETED_START: char = '[';

/// The first word that begins a host block, in any case, as it is read.
const HOST_WORD: &str = "host";

/// The same word as the block is written, in the spelling a person's file
/// uses.
const WRITTEN_HOST_WORD: &str = "Host";

/// How many characters make a host pattern match more than itself.
const PATTERN_CHARACTER_COUNT: usize = 3;

/// The characters that make a host pattern match more than itself.
const PATTERN_CHARACTERS: [char; PATTERN_CHARACTER_COUNT] = ['*', '?', '!'];

/// The character that begins a comment.
const COMMENT: char = '#';

/// The word that names a host's address inside its block.
const ADDRESS_WORD: &str = "HostName";

/// Why an ssh configuration could not be read or written.
#[derive(Debug)]
pub enum SshConfigError {
    /// `$HOME` is not set, so where the configuration lives is unknown.
    MissingHome,
    /// The configuration could not be read or written.
    Io {
        /// The path that could not be read or written.
        path: PathBuf,
        /// What the operating system said.
        source: std::io::Error,
    },
    /// The configuration already defines the alias.
    AliasAlreadyDefined {
        /// The alias that is already there.
        alias: String,
    },
    /// The alias could not stand as the first word of a host block.
    AliasNotPlain {
        /// What was offered.
        alias: String,
        /// Why it cannot be one host.
        detail: &'static str,
    },
    /// The address could not stand on an address line.
    AddressNotPlain {
        /// What was offered.
        address: String,
        /// Why it cannot be an address.
        detail: &'static str,
    },
}

impl Display for SshConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            SshConfigError::MissingHome => formatter.write_str(
                "the home directory is not known, so there is no ssh configuration to write",
            ),
            SshConfigError::Io { path, source } => {
                write!(formatter, "{}: {source}", path.display())
            }
            SshConfigError::AliasAlreadyDefined { alias } => write!(
                formatter,
                "{alias} is already defined by the ssh configuration"
            ),
            SshConfigError::AliasNotPlain { alias, detail } => {
                write!(formatter, "{alias} cannot name one host: {detail}")
            }
            SshConfigError::AddressNotPlain { address, detail } => {
                write!(formatter, "{address} is not an address: {detail}")
            }
        }
    }
}

impl core::error::Error for SshConfigError {}

/// The place a person's ssh configuration lives, from `$HOME`.
#[must_use]
pub fn default_path() -> Option<PathBuf> {
    home_path(CONFIG_NAME)
}

/// The place ssh's record of reached hosts lives, beside the configuration.
#[must_use]
pub fn default_known_hosts_path() -> Option<PathBuf> {
    home_path(KNOWN_HOSTS_NAME)
}

/// One of ssh's own files under the home directory.
fn home_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| PathBuf::from(home).join(SSH_DIRECTORY).join(name))
}

/// The hosts the configuration at `path` names first, then every host the
/// `known_hosts` at `known_hosts_path` remembers that it does not.
///
/// The configuration's own order leads because a person who wrote a block
/// means it; what `known_hosts` adds is the hosts they have already reached,
/// so a laptop whose configuration is nothing but `Host *` still offers the
/// machines its person has used.
///
/// # Errors
///
/// [`SshConfigError::Io`] when a file exists but cannot be read.
pub fn known_hosts(path: &Path, known_hosts_path: &Path) -> Result<Vec<String>, SshConfigError> {
    let mut hosts = aliases(path)?;
    for remembered in read_known_hosts(known_hosts_path)? {
        if !hosts
            .iter()
            .any(|known| known.eq_ignore_ascii_case(&remembered))
        {
            hosts.push(remembered);
        }
    }
    Ok(hosts)
}

/// Every host the `known_hosts` at `path` remembers; an absent file remembers
/// none.
///
/// # Errors
///
/// [`SshConfigError::Io`] when the file exists but cannot be read.
fn read_known_hosts(path: &Path) -> Result<Vec<String>, SshConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parsed_known_hosts(&text)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(SshConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Every plain alias a configuration's text defines, in the order it defines
/// them and with duplicates removed.
///
/// Pure, so what a configuration says is a table of inputs rather than a file
/// read. A line whose first word is not `Host`, in any case, is not a block; a
/// pattern holding `*`, `?` or `!` is not a host.
#[must_use]
pub fn parsed_aliases(text: &str) -> Vec<String> {
    let mut aliases: Vec<String> = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(first) = words.next() else {
            continue;
        };
        if first.starts_with(COMMENT) || !first.eq_ignore_ascii_case(HOST_WORD) {
            continue;
        }
        for pattern in words {
            if pattern.starts_with(COMMENT) {
                break;
            }
            if plain_pattern(pattern) && !aliases.iter().any(|known| known == pattern) {
                aliases.push(pattern.to_owned());
            }
        }
    }
    aliases
}

/// Whether a configuration's text already defines `alias`, whichever case
/// either is written in, because `ssh` matches host patterns without regard
/// to case.
#[must_use]
pub fn defines(text: &str, alias: &str) -> bool {
    parsed_aliases(text)
        .iter()
        .any(|known| known.eq_ignore_ascii_case(alias))
}

/// Every host name a `known_hosts` text remembers, in the order it remembers
/// them and with duplicates removed.
///
/// Only a name is read, not an address: a bare `10.0.0.9` is a machine a
/// person reached once, not a host they would recognize in a list, and `ssh`
/// takes a typed address without any configuration. A host whose name `ssh`
/// writes hashed, and a host written with its port in brackets, is not one
/// this can offer either: `ssh` never writes a hashed name down, and a
/// bracketed one carries a nonstandard port this must not drop.
#[must_use]
pub fn parsed_known_hosts(text: &str) -> Vec<String> {
    let mut hosts: Vec<String> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() || trimmed.starts_with(COMMENT) || trimmed.starts_with(HASH_MARKER) {
            continue;
        }
        let Some(first) = trimmed.split_whitespace().next() else {
            continue;
        };
        for name in first.split(',') {
            if name.is_empty()
                || name.starts_with(BRACKETED_START)
                || !plain_pattern(name)
                || address(name)
            {
                continue;
            }
            if !hosts.iter().any(|known| known == name) {
                hosts.push(name.to_owned());
            }
        }
    }
    hosts
}

/// Whether a name is a bare address rather than a host name: it holds a
/// colon, which no name does and every IPv6 literal does, or every dot-
/// separated part of it is digits, which is what an IPv4 literal is.
fn address(name: &str) -> bool {
    if name.contains(':') {
        return true;
    }
    name.split('.')
        .all(|part| !part.is_empty() && part.chars().all(|character| character.is_ascii_digit()))
}

/// Every plain alias the configuration at `path` defines; an absent file
/// defines none.
///
/// # Errors
///
/// [`SshConfigError::Io`] when the file exists but cannot be read.
pub fn aliases(path: &Path) -> Result<Vec<String>, SshConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(parsed_aliases(&text)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(source) => Err(SshConfigError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

/// Appends a block defining `alias` at `address` to the configuration at
/// `path`, creating the file and the directory it lives in when they are not
/// there.
///
/// The block is `Host <alias>` followed by an indented `HostName <address>`,
/// which is what `ssh` reads and what a person reading their own file expects
/// to find.
///
/// # Errors
///
/// [`SshConfigError::AliasNotPlain`] when the alias cannot stand as the first
/// word of a host block, [`SshConfigError::AddressNotPlain`] when the address
/// cannot stand on an address line, [`SshConfigError::AliasAlreadyDefined`]
/// when the file already defines the alias, and [`SshConfigError::Io`] when
/// the file cannot be read or written.
pub fn append_host(path: &Path, alias: &str, address: &str) -> Result<(), SshConfigError> {
    let alias = alias.trim();
    let address = address.trim();
    refuse_alias(alias)?;
    refuse_address(address)?;
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(source) => {
            return Err(SshConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if defines(&existing, alias) {
        return Err(SshConfigError::AliasAlreadyDefined {
            alias: alias.to_owned(),
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SshConfigError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let mut block = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        block.push('\n');
    }
    // A blank line keeps the block apart from whatever was there, in the file
    // as a person will read it.
    {
        use core::fmt::Write as _;
        let _written = write!(
            block,
            "\n{WRITTEN_HOST_WORD} {alias}\n    {ADDRESS_WORD} {address}\n"
        );
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .and_then(|mut file| file.write_all(block.as_bytes()))
        .map_err(|source| SshConfigError::Io {
            path: path.to_path_buf(),
            source,
        })
}

/// Whether a host pattern names one host rather than a set of them.
fn plain_pattern(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.contains(PATTERN_CHARACTERS)
}

impl crate::window::WindowShell {
    /// Define a host in the person's ssh configuration, then hold and connect
    /// it, exactly as [`crate::window::WindowShell::add_host`] would.
    ///
    /// The block is written first, because a host held before its
    /// configuration names it would have `ssh` connect to the alias as a
    /// hostname — which is the failure this exists to prevent.
    ///
    /// # Errors
    ///
    /// [`SshConfigError`]'s words when the configuration cannot be read or
    /// written, as an engine error, and the bridge error when the engine has
    /// ended.
    pub fn add_host_with_address(
        &mut self,
        alias: &str,
        address: &str,
    ) -> Result<(), crate::bridge::EngineError> {
        let path =
            self.ssh_config_path
                .clone()
                .ok_or(crate::bridge::EngineError::Configuration(
                    SshConfigError::MissingHome,
                ))?;
        append_host(&path, alias, address).map_err(crate::bridge::EngineError::Configuration)?;
        self.add_host(alias)
    }

    /// The hosts the person's ssh configuration names and the ones ssh's own
    /// record remembers, `known_hosts` names last; empty when neither can be
    /// read.
    ///
    /// The record is looked for beside the configuration, so a test that
    /// points the configuration at a scratch directory never reads the
    /// developer's own.
    #[must_use]
    pub fn ssh_alias(&self) -> Vec<String> {
        let Some(path) = self.ssh_config_path.as_deref() else {
            return Vec::new();
        };
        let remembered = path
            .parent()
            .map(|directory| directory.join(KNOWN_HOSTS_NAME));
        match remembered {
            Some(remembered) => known_hosts(path, &remembered).unwrap_or_default(),
            None => aliases(path).unwrap_or_default(),
        }
    }

    /// Whether `ssh` can already reach `alias` without this application
    /// writing anything: the name is one the configuration defines, one
    /// `known_hosts` remembers, or this application's own `unix:` socket form.
    #[must_use]
    pub fn ssh_defines(&self, alias: &str) -> bool {
        alias.starts_with(iznik_client::transport::LOCAL_PREFIX)
            || self
                .ssh_alias()
                .iter()
                .any(|known| known.eq_ignore_ascii_case(alias))
    }
}

/// Refuses an alias that cannot stand as the first word of a host block.
///
/// # Errors
///
/// [`SshConfigError::AliasNotPlain`] naming why the alias cannot be one host,
/// and nothing when it can.
fn refuse_alias(alias: &str) -> Result<(), SshConfigError> {
    let detail = if alias.is_empty() {
        Some("it is empty")
    } else if alias.chars().any(char::is_whitespace) {
        Some("it holds space")
    } else if alias.contains(PATTERN_CHARACTERS) {
        Some("it is a pattern rather than one host")
    } else if alias.contains(COMMENT) {
        Some("it holds the character that begins a comment")
    } else {
        None
    };
    match detail {
        Some(detail) => Err(SshConfigError::AliasNotPlain {
            alias: alias.to_owned(),
            detail,
        }),
        None => Ok(()),
    }
}

/// Refuses an address that cannot stand on an address line.
///
/// # Errors
///
/// [`SshConfigError::AddressNotPlain`] naming why the address cannot be one,
/// and nothing when it can.
fn refuse_address(address: &str) -> Result<(), SshConfigError> {
    let detail = if address.is_empty() {
        Some("it is empty")
    } else if address.chars().any(char::is_whitespace) {
        Some("it holds space")
    } else if address.contains(COMMENT) {
        Some("it holds the character that begins a comment")
    } else {
        None
    };
    match detail {
        Some(detail) => Err(SshConfigError::AddressNotPlain {
            address: address.to_owned(),
            detail,
        }),
        None => Ok(()),
    }
}
