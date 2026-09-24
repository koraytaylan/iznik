//! The SSH agent a pane is told about: one stable path in the runtime
//! directory, re-pointed by every relay at the agent of the SSH connection it
//! arrived on.
//!
//! A pane outlives the connection that made it, and the daemon's environment
//! is that of the first connection. Handing a pane that connection's
//! `SSH_AUTH_SOCK` would leave every pane with a dead agent after a
//! reconnect; handing it a link the newest relay keeps current means `ssh`
//! and `git` in an old pane reach the agent of whoever is attached now.

use std::io;
use std::path::Path;

/// The link's name inside the runtime directory.
pub const AGENT_NAME: &str = "agent.sock";

/// The variable an SSH client reads its agent's socket from.
pub const AGENT_VARIABLE: &str = "SSH_AUTH_SOCK";

/// Points `link` at `agent`, replacing whatever it pointed at in one step: a
/// pane that looks at the link mid-change sees the old agent or the new one,
/// never no link at all. The link lives in the owner-only runtime directory,
/// so nobody else can put one there or follow it.
///
/// # Errors
///
/// When the link cannot be made or renamed into place; the partial link is
/// removed first.
pub async fn point(link: &Path, agent: &Path) -> io::Result<()> {
    let mut staged = link.as_os_str().to_owned();
    staged.push(format!(".{}", std::process::id()));
    let staged = std::path::PathBuf::from(staged);
    let _stale = tokio::fs::remove_file(&staged).await;
    stage(agent, &staged).await?;
    if let Err(error) = tokio::fs::rename(&staged, link).await {
        let _removed = tokio::fs::remove_file(&staged).await;
        return Err(error);
    }
    Ok(())
}

/// Makes `staged` a symbolic link to `agent`.
///
/// # Errors
///
/// When the link cannot be made.
#[cfg(unix)]
async fn stage(agent: &Path, staged: &Path) -> io::Result<()> {
    tokio::fs::symlink(agent, staged).await
}

/// There is no SSH agent socket to link to on Windows.
///
/// # Errors
///
/// Always: the operation is unsupported there.
#[cfg(windows)]
fn stage(_agent: &Path, _staged: &Path) -> std::future::Ready<io::Result<()>> {
    std::future::ready(Err(io::Error::from(io::ErrorKind::Unsupported)))
}

/// Points `link` at the agent this process was given, if it was given one:
/// what every relay does as it starts. A connection without agent forwarding
/// leaves the link alone, since another connection's agent may still be there
/// to be used.
pub async fn adopt(link: &Path) {
    let Some(agent) = std::env::var_os(AGENT_VARIABLE).filter(|held| !held.is_empty()) else {
        return;
    };
    if let Err(error) = point(link, Path::new(&agent)).await {
        tracing::debug!(%error, "the agent link could not be re-pointed");
    }
}
