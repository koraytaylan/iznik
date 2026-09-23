//! Includes are followed, marker lines are not hosts, and an alias that
//! reads as an option is neither offered nor written.

use iznik_app::ssh_config::{SshConfigError, aliases, append_host, parsed_known_hosts};

/// A configuration's `Include`, by relative path and by wildcard, adds the
/// aliases the included files define.
///
/// # Panics
///
/// Panics when an included alias is missing or the order differs.
#[test]
fn includes_are_followed() {
    let directory = std::env::temp_dir().join(format!("iznik-ssh-include-{}", std::process::id()));
    let _stale = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("config.d")).expect("directory");
    std::fs::write(
        directory.join("config"),
        "Host main\nInclude config.d/*\nInclude extra\n",
    )
    .expect("config");
    std::fs::write(directory.join("config.d/one"), "Host first\n").expect("one");
    std::fs::write(directory.join("config.d/two"), "Host second\n").expect("two");
    std::fs::write(directory.join("extra"), "Host third\nInclude extra\n").expect("extra");
    let found = aliases(&directory.join("config")).expect("aliases");
    assert_eq!(found, ["main", "first", "second", "third"], "{found:?}");
    let _removed = std::fs::remove_dir_all(&directory);
}

/// `@cert-authority` and `@revoked` lines name no host a person reached.
///
/// # Panics
///
/// Panics when a marker line is offered.
#[test]
fn marker_lines_are_not_hosts() {
    let hosts = parsed_known_hosts(
        "@cert-authority *.example.com ssh-ed25519 AAAA\n@revoked badhost ssh-rsa AAAA\nbuild ssh-ed25519 AAAA\n",
    );
    assert_eq!(hosts, ["build"]);
}

/// An alias beginning with `-` would reach `ssh` as an option.
///
/// # Panics
///
/// Panics when such an alias is written.
#[test]
fn an_option_is_not_an_alias() {
    let path = std::env::temp_dir().join(format!("iznik-ssh-option-{}", std::process::id()));
    let refused = append_host(&path, "-oProxyCommand=touch", "example.com");
    assert!(
        matches!(refused, Err(SshConfigError::AliasNotPlain { .. })),
        "{refused:?}"
    );
    assert!(!path.exists(), "nothing was written");
}
