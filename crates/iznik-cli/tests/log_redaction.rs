//! What `iznik doctor` keeps of a line of the daemon's log: when, how serious,
//! where, the server's own words and the names of the fields — and never a
//! field's value.

use iznik_cli::doctor::redacted;

/// # Panics
///
/// When a field's value survives, a field's name is lost, or a line with no
/// fields is changed.
#[test]
fn log_redaction_keeps_the_names_and_never_the_values() {
    let secret = "hunter2-token";
    let line = format!(
        "2026-09-23T12:00:00.000000Z  INFO iznik_server::daemon: a client's connection ended \
         error=the peer {secret} went away socket=/run/user/1000/{secret}.sock count=3"
    );
    let kept = redacted(&line);
    assert!(!kept.contains(secret), "no value survives: {kept}");
    assert!(
        kept.starts_with(
            "2026-09-23T12:00:00.000000Z  INFO iznik_server::daemon: a client's connection ended"
        ),
        "the time, the level, the target and the words are kept: {kept}"
    );
    for name in ["error=", "socket=", "count="] {
        assert!(kept.contains(name), "and every field's name: {kept}");
    }
    let quoted = format!("INFO target: said host=\"work {secret}\"");
    assert!(
        !redacted(&quoted).contains(secret),
        "a quoted value with a space in it goes too"
    );
    let plain = "2026-09-23T12:00:00.000000Z  INFO iznik_server::daemon: the daemon is listening";
    assert_eq!(
        redacted(plain),
        plain,
        "a line with no fields is kept whole"
    );
}
