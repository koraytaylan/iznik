//! Getting a server onto a host and a channel to it: probe, decide, upload,
//! launch, hand shake, snapshot — and the two verbs that undo it.
//!
//! Three things happen here and nothing else does. [`bootstrap`] connects,
//! installing only when the host has nothing this client may run. [`upgrade`]
//! replaces a server that is there, and refuses while it is holding panes
//! because the daemon *is* the sessions and replacing it would end them.
//! [`uninstall`] takes it all off again: a tool that puts binaries on other
//! people's machines owes them a clean way to be rid of it.

pub mod launch;
pub mod probe;
pub mod terminfo;
pub mod upload;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::bootstrap::launch::{
    BootstrapError, BootstrapOptions, Bootstrapped, Decision, Stage, UpgradeError, bundled, decide,
    expiry, launch, left, live_panes, refused, server_path, triple_of,
};
use crate::bootstrap::probe::{HostProbe, probe};
use crate::bootstrap::upload::{Installed, PREFIX_VARIABLE, TERMINFO_DIRECTORY, quoted, upload};
use crate::transport::Transport;

/// The remote script that ends a daemon before its server is replaced.
///
/// A host with no server, or one whose daemon is not running, is not a
/// failure: this is asked so that nothing is running afterwards, and on such a
/// host that is already true.
pub const REMOTE_STOP_SCRIPT: &str = r#"
server="$IZNIK_PREFIX/bin/iznik-server"
if [ -x "$server" ]; then "$server" --stop >/dev/null 2>&1 || true; fi
printf 'stopped %s\n' "$IZNIK_PREFIX"
"#;

/// The remote script that takes iznik off a host.
///
/// It names what it installed rather than sweeping the prefix away, because a
/// probed prefix may be a directory iznik was given rather than one it made —
/// `XDG_RUNTIME_DIR` is one of the candidates — and nothing of somebody else's
/// is this program's to delete. The prefix itself goes when it is empty — and
/// never when it *is* the runtime directory, which is the one candidate iznik
/// is lent rather than makes, and which a person's session put there.
pub const REMOTE_UNINSTALL_SCRIPT: &str = r#"
server="$IZNIK_PREFIX/bin/iznik-server"
if [ -x "$server" ]; then "$server" --stop >/dev/null 2>&1 || true; fi
rm -f "$server" "$IZNIK_PREFIX/bin"/.partial-* "$IZNIK_PREFIX"/.terminfo-*
rm -rf "$IZNIK_PREFIX/terminfo"
if [ -n "${XDG_RUNTIME_DIR:-}" ]
then runtime="$XDG_RUNTIME_DIR/iznik"
else runtime="${TMPDIR:-/tmp}/iznik-$(id -u)"; fi
rm -rf "$runtime"
rmdir "$IZNIK_PREFIX/bin" 2>/dev/null || true
if [ "$IZNIK_PREFIX" != "${XDG_RUNTIME_DIR:-}" ]
then rmdir "$IZNIK_PREFIX" 2>/dev/null || true; fi
printf 'removed %s\n' "$IZNIK_PREFIX"
printf 'runtime %s\n' "$runtime"
"#;

/// What an [`uninstall`] took off a host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Removed {
    /// The prefix the server was under.
    pub prefix: PathBuf,
    /// The runtime directory the daemon kept its socket in.
    pub runtime: PathBuf,
}

/// One remote script, with the prefix in the environment the script reads it
/// from.
fn with_prefix(script: &str, prefix: &Path) -> String {
    format!(
        "{PREFIX_VARIABLE}={} sh -c {}",
        quoted(&prefix.display().to_string()),
        quoted(script)
    )
}

/// The refusal an unsupported machine becomes, before anything is uploaded.
///
/// It names what the build does carry, because "no server for" alone cannot
/// tell a build packaged without servers from one missing a single machine.
fn no_artifact(host: &str, triple: &str, carried: &[&str]) -> BootstrapError {
    let detail = if carried.is_empty() {
        format!("this build carries no server for {triple}: it carries no servers at all")
    } else {
        format!(
            "this build carries no server for {triple}: it carries servers for {}",
            carried.join(", ")
        )
    };
    BootstrapError {
        host: host.to_owned(),
        stage: Stage::Probe,
        detail,
    }
}

/// Connects to a host, installing the server first when it has none.
///
/// A host that already has the version this build carries is reached without a
/// byte being uploaded, which is what makes every connection after the first
/// one finish inside [`launch::FAST_RECONNECT_BUDGET`]. A host with another
/// version is connected to as it is: the offer to replace it rides back in
/// [`Bootstrapped::decision`] for somebody to accept.
///
/// # Errors
///
/// A [`BootstrapError`] naming the stage it stopped at and what the remote
/// said. An unsupported machine is refused at [`Stage::Probe`], which is
/// before anything has been put on it.
pub async fn bootstrap(
    transport: &Transport,
    artifacts: &upload::ArtifactSet,
    options: &BootstrapOptions,
    deadline: Duration,
) -> Result<Bootstrapped, BootstrapError> {
    bootstrap_watched(transport, artifacts, options, deadline, &|_stage| {}).await
}

/// The same, telling `reached` which stage it has got to.
///
/// A bootstrap of a host that has nothing takes as long as the link does, and
/// something is watching: the manager renders "probing", "uploading",
/// "connecting" from these, and a person waiting deserves to know which of
/// them they are waiting through.
///
/// # Errors
///
/// As [`bootstrap`].
pub async fn bootstrap_watched(
    transport: &Transport,
    artifacts: &upload::ArtifactSet,
    options: &BootstrapOptions,
    deadline: Duration,
    reached: &(dyn Fn(Stage) + Send + Sync),
) -> Result<Bootstrapped, BootstrapError> {
    let host = transport.alias();
    reached(Stage::Probe);
    let expires = expiry(deadline);
    let found = probe(transport, left(expires, options.probe_deadline))
        .await
        .map_err(|source| refused(&host, Stage::Probe, &source))?;
    let decision = decide(&found, artifacts, &bundled());
    if let Decision::Unsupported { triple } = &decision {
        return Err(no_artifact(&host, triple, &artifacts.triples()));
    }
    // Where the server is, is where the host said it put it. The two agree,
    // and asking is cheaper than assuming they always will.
    let installed = if decision == Decision::Install {
        reached(Stage::Upload);
        install(
            transport,
            &found,
            artifacts,
            left(expires, options.upload_deadline),
        )
        .await?
    } else {
        // Nothing was installed, so what the host has is what the probe found.
        Installed {
            server: server_path(&found),
            terminfo: found
                .terminfo_installed
                .then(|| found.prefix.join(TERMINFO_DIRECTORY)),
            terminfo_refused: (!found.terminfo_installed)
                .then(|| "the host has no terminfo of iznik's under its prefix".to_owned()),
        }
    };
    let Installed {
        server,
        terminfo,
        terminfo_refused,
    } = installed;
    reached(Stage::Launch);
    let (channel, snapshot) = launch(transport, Some(&server), options, expires).await?;
    Ok(Bootstrapped {
        decision,
        server,
        terminfo,
        terminfo_refused,
        snapshot,
        channel,
    })
}

/// Puts this build's server on a probed host.
///
/// # Errors
///
/// A [`BootstrapError`] at [`Stage::Upload`], and what it left otherwise.
async fn install(
    transport: &Transport,
    found: &HostProbe,
    artifacts: &upload::ArtifactSet,
    deadline: Duration,
) -> Result<Installed, BootstrapError> {
    let host = transport.alias();
    let triple = triple_of(found);
    let artifact = artifacts
        .for_triple(&triple)
        .map_err(|source| refused(&host, Stage::Upload, &source))?;
    upload(transport, artifact, found, deadline)
        .await
        .map_err(|source| refused(&host, Stage::Upload, &source))
}

/// Ends the daemon on a host, if one is running.
///
/// # Errors
///
/// A [`BootstrapError`] at [`Stage::Launch`] when the host could not be asked.
async fn stop(
    transport: &Transport,
    prefix: &Path,
    deadline: Duration,
) -> Result<(), BootstrapError> {
    let host = transport.alias();
    let asked = with_prefix(REMOTE_STOP_SCRIPT, prefix);
    let _said = probe::RunsRemotely::run(transport, &asked, deadline)
        .await
        .map_err(|source| refused(&host, Stage::Launch, &source))?;
    Ok(())
}

/// Whether the daemon that is there may be replaced, by asking it what it
/// holds — unless it has already been said that it may be.
///
/// # Errors
///
/// [`UpgradeError::LivePanes`] when it holds panes, and
/// [`UpgradeError::Bootstrap`] when it could not be asked at all. Neither when
/// `force` is set, which is what makes an unreachable daemon replaceable.
async fn may_replace(
    transport: &Transport,
    found: &HostProbe,
    options: &BootstrapOptions,
    force: bool,
    expires: Instant,
) -> Result<(), UpgradeError> {
    if force {
        // Somebody has already said they mean it, so the count would change
        // nothing — and asking for it must not be what stops them. The case a
        // forced upgrade exists for is a daemon this client can no longer
        // speak to at all: one whose protocol version is not this one's,
        // whose binary will not start, whose handshake never comes. Every one
        // of those fails the very channel that would have counted the panes.
        return Ok(());
    }
    let host = transport.alias();
    let (channel, held) = launch(transport, Some(&server_path(found)), options, expires).await?;
    let count = live_panes(&held);
    channel.close();
    if count > 0 {
        return Err(UpgradeError::LivePanes { host, count });
    }
    Ok(())
}

/// Replaces the server on a host with the one this build carries.
///
/// Nothing happens to a host that already has it. A host holding panes is
/// refused unless `force` says otherwise, because those panes are what the
/// daemon is.
///
/// # Errors
///
/// [`UpgradeError::LivePanes`] with the count, or [`UpgradeError::Bootstrap`]
/// naming the stage that failed.
pub async fn upgrade(
    transport: &Transport,
    artifacts: &upload::ArtifactSet,
    options: &BootstrapOptions,
    force: bool,
    deadline: Duration,
) -> Result<(), UpgradeError> {
    let host = transport.alias();
    let expires = expiry(deadline);
    let found = probe(transport, left(expires, options.probe_deadline))
        .await
        .map_err(|source| refused(&host, Stage::Probe, &source))?;
    match decide(&found, artifacts, &bundled()) {
        Decision::UpToDate => return Ok(()),
        Decision::Unsupported { triple } => {
            return Err(no_artifact(&host, &triple, &artifacts.triples()).into());
        }
        // Nothing is there to end, and nothing is holding panes.
        Decision::Install => {}
        Decision::UpgradeAvailable { .. } => {
            may_replace(transport, &found, options, force, expires).await?;
            stop(
                transport,
                &found.prefix,
                left(expires, options.command_deadline),
            )
            .await?;
        }
    }
    let installed = install(
        transport,
        &found,
        artifacts,
        left(expires, options.upload_deadline),
    )
    .await?;
    let (channel, _held) = launch(transport, Some(&installed.server), options, expires).await?;
    // The daemon *is* the sessions, so what matters is which one is answering
    // — not which binary is on disk. `--stop` is allowed to fail, because a
    // host with nothing running is a host with nothing to stop; but if the old
    // daemon survived it, the new binary's relay has just attached to it and
    // the upgrade did not happen.
    let answering = channel.greeting().server_version.clone();
    channel.close();
    let carried = bundled().crate_version;
    if answering != carried {
        return Err(UpgradeError::Bootstrap(BootstrapError {
            host,
            stage: Stage::Launch,
            detail: format!(
                "the server was replaced but {answering} is still answering, not {carried}: \
                 the daemon that was there did not stop"
            ),
        }));
    }
    Ok(())
}

/// Takes iznik off a host: the daemon stopped, the server, the terminfo and
/// the runtime directory gone, and the prefix with them when iznik made it.
///
/// # Errors
///
/// A [`BootstrapError`] at [`Stage::Probe`] when the host cannot be asked
/// where it put things, and at [`Stage::Launch`] when the removal itself
/// failed.
pub async fn uninstall(
    transport: &Transport,
    options: &BootstrapOptions,
    deadline: Duration,
) -> Result<Removed, BootstrapError> {
    let host = transport.alias();
    let expires = expiry(deadline);
    let found = probe(transport, left(expires, options.probe_deadline))
        .await
        .map_err(|source| refused(&host, Stage::Probe, &source))?;
    let asked = with_prefix(REMOTE_UNINSTALL_SCRIPT, &found.prefix);
    let said = probe::RunsRemotely::run(transport, &asked, left(expires, options.command_deadline))
        .await
        .map_err(|source| refused(&host, Stage::Launch, &source))?;
    removed(&host, &said)
}

/// What the host said it took off.
///
/// # Errors
///
/// A [`BootstrapError`] at [`Stage::Launch`] when it did not say.
fn removed(host: &str, said: &str) -> Result<Removed, BootstrapError> {
    let named = |what: &str| {
        said.lines()
            .find_map(|line| line.trim().strip_prefix(what))
            .map(PathBuf::from)
    };
    let (Some(prefix), Some(runtime)) = (named("removed "), named("runtime ")) else {
        return Err(refused(
            host,
            Stage::Launch,
            &format!("the host did not say what it removed: {said}"),
        ));
    };
    Ok(Removed { prefix, runtime })
}
