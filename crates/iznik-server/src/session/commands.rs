//! Applying a session command to the registry: validation against the model
//! first, then the operation, answered exactly once.
//!
//! A rejected command changes nothing — not the generation, not a pane, not a
//! delta — because a half-applied command is the one thing a client cannot
//! reconcile: it would hold a model the server does not, with no delta to tell
//! it so and no way to know it should ask.
//!
//! Every command is answered, and answered once. The answer says what was made
//! so that a client which waited a round trip for a creation has the id it
//! waited for, and says why when nothing was made, by a code rather than a
//! sentence — the sentence is for a log.

use std::time::Duration;

use crate::session::registry::{Registry, RegistryError};
use iznik_protocol::command::{CommandOutcome, Created, RejectionCode, SessionCommand};

/// How long a client-supplied working directory may take to be looked at
/// before the pane starts in the home directory instead. A local directory
/// answers in microseconds; one on a stale network mount may never answer,
/// and the look must not hold up the command — or the registry lock the
/// command is applied under — for as long as the mount takes to give up.
pub const WORKING_DIRECTORY_DEADLINE: Duration = Duration::from_millis(500);

/// A creating command with its working directory looked at first, off the
/// runtime's workers and under [`WORKING_DIRECTORY_DEADLINE`]: kept when it is
/// a directory, and dropped — so the pane starts in the home directory — when
/// it is not one, cannot be reached, or does not answer in time. Every other
/// command is returned as it came.
///
/// Called before the registry lock is taken, so a directory that hangs holds
/// up nobody else, and the spawn that follows is given only a directory that
/// has just answered.
pub async fn settle(command: SessionCommand) -> SessionCommand {
    settle_within(command, WORKING_DIRECTORY_DEADLINE).await
}

/// [`settle`] under a deadline of the caller's choosing.
pub async fn settle_within(mut command: SessionCommand, deadline: Duration) -> SessionCommand {
    let slot = match &mut command {
        SessionCommand::CreateSession {
            working_directory, ..
        }
        | SessionCommand::CreateTab {
            working_directory, ..
        }
        | SessionCommand::CreatePane {
            working_directory, ..
        } => working_directory,
        _other => return command,
    };
    if let Some(asked) = slot.take() {
        // An answer that came after the deadline is as late as none, so the
        // rule is the same whichever of the two the timer noticed first.
        let started = tokio::time::Instant::now();
        let looked = tokio::time::timeout(deadline, tokio::fs::metadata(&asked))
            .await
            .ok()
            .filter(|_answered| started.elapsed() <= deadline);
        match looked {
            Some(Ok(metadata)) if metadata.is_dir() => *slot = Some(asked),
            Some(Ok(_other)) => {
                tracing::info!("a pane asked to start in something that is not a directory");
            }
            Some(Err(error)) => {
                tracing::info!(%error, "a pane asked to start in a directory that is not there");
            }
            None => {
                tracing::warn!("a pane's working directory did not answer in time");
            }
        }
    }
    command
}

/// The answer a refusal becomes: the code a client acts on, and the words a
/// log keeps.
fn rejected(error: &RegistryError) -> CommandOutcome {
    let code = match error {
        RegistryError::UnknownSession { .. } => RejectionCode::UnknownSession,
        RegistryError::UnknownTab { .. } => RejectionCode::UnknownTab,
        RegistryError::UnknownPane { .. } => RejectionCode::UnknownPane,
        RegistryError::EmptyName => RejectionCode::EmptyName,
        RegistryError::NotAPermutation { .. } | RegistryError::NotASessionPermutation => {
            RejectionCode::InvalidOrder
        }
        RegistryError::InvalidLayout { .. } => RejectionCode::InvalidLayout,
        // A pane that was started and then taken away again is, from the
        // client's side, a pane that was never made.
        RegistryError::Spawn(..) | RegistryError::Refused { .. } => RejectionCode::SpawnFailed,
    };
    CommandOutcome::Rejected {
        code,
        message: error.to_string(),
    }
}

/// The answer an operation that changed the model becomes.
fn applied(registry: &Registry, created: Created) -> CommandOutcome {
    CommandOutcome::Applied {
        generation: registry.generation(),
        created,
    }
}

/// Applies one command and answers it.
///
/// It is asynchronous because three of the eleven start a process: there is no
/// synchronous way to open a pseudoterminal and wait for its child to be
/// there, and pretending otherwise would put a blocking spawn on the runtime
/// that carries every other pane's bytes.
pub async fn apply(registry: &mut Registry, command: SessionCommand) -> CommandOutcome {
    match command {
        SessionCommand::CreateSession {
            name,
            columns,
            rows,
            working_directory,
        } => match registry
            .create_session(name, columns, rows, working_directory.map(Into::into))
            .await
        {
            Ok(session) => applied(registry, Created::Session(session)),
            Err(error) => rejected(&error),
        },
        SessionCommand::CreateTab {
            session,
            name,
            columns,
            rows,
            working_directory,
        } => match registry
            .create_tab(
                session,
                name,
                columns,
                rows,
                working_directory.map(Into::into),
            )
            .await
        {
            Ok(tab) => applied(registry, Created::Tab(tab)),
            Err(error) => rejected(&error),
        },
        SessionCommand::CreatePane {
            tab,
            placement,
            columns,
            rows,
            working_directory,
        } => match registry
            .create_pane(
                tab,
                placement,
                columns,
                rows,
                working_directory.map(Into::into),
            )
            .await
        {
            Ok(pane) => applied(registry, Created::Pane(pane)),
            Err(error) => rejected(&error),
        },
        other => apply_change(registry, other),
    }
}

/// Applies one command that changes what is already there, and answers it.
///
/// Nothing here starts a process, so nothing here waits.
fn apply_change(registry: &mut Registry, command: SessionCommand) -> CommandOutcome {
    let outcome = match command {
        SessionCommand::RenameSession { session, name } => registry.rename_session(session, name),
        SessionCommand::CloseSession { session } => registry.close_session(session),
        SessionCommand::RenameTab { tab, name } => registry.rename_tab(tab, name),
        SessionCommand::CloseTab { tab } => registry.close_tab(tab),
        SessionCommand::ReorderTabs { session, order } => registry.reorder_tabs(session, order),
        SessionCommand::ReorderSessions { order } => registry.reorder_sessions(order),
        SessionCommand::ClosePane { pane } => registry.close_pane(pane),
        SessionCommand::MovePane {
            pane,
            to_tab,
            placement,
        } => registry.move_pane(pane, to_tab, placement),
        SessionCommand::SetLayout { tab, layout } => registry.set_layout(tab, layout),
        // Ruled out by `apply`, the only caller: these three start a process
        // and are answered there.
        SessionCommand::CreateSession { .. }
        | SessionCommand::CreateTab { .. }
        | SessionCommand::CreatePane { .. } => Ok(()),
    };
    match outcome {
        Ok(()) => applied(registry, Created::Nothing),
        Err(error) => rejected(&error),
    }
}
