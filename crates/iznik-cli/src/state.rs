//! `iznik state <host>`: the host model as the server holds it, printed.
//!
//! What a host says it has — its sessions, their tabs, the panes in them and
//! the size each is — as one object, so that a script can ask a question of a
//! running system without a screen and a person can see what an application
//! would be drawing.

use std::ffi::OsString;
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use iznik_client::host::identity::HostId;
use iznik_protocol::model::{HostModel, Pane, Session, Tab};

use crate::output::{Value, line, object, refusal, text};
use crate::{CLIENT_LAYER, USAGE_EXIT_CODE, holding, one_host};

/// What this subcommand takes.
const USAGE: &str = "usage: iznik state <host>";

/// The subcommand's entry point.
///
/// The dispatcher hands over every argument after the program name, the
/// subcommand first, and the module parses its own flags.
#[must_use]
pub fn run(arguments: &[OsString]) -> ExitCode {
    if crate::asked_for_help(arguments) {
        return crate::help_with(USAGE);
    }
    let Some(alias) = one_host(arguments) else {
        let _said = refusal(&mut std::io::stderr(), CLIENT_LAYER, USAGE);
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    let uninterrupted = AtomicBool::new(false);
    let (manager, _events) = match holding(&alias, &uninterrupted) {
        Ok(held) => held,
        Err((layer, detail)) => {
            let _said = refusal(&mut std::io::stderr(), layer, &detail);
            return ExitCode::FAILURE;
        }
    };
    let held = manager.model();
    let Some(view) = held.host(&HostId(alias.clone())) else {
        let _said = refusal(
            &mut std::io::stderr(),
            CLIENT_LAYER,
            &format!("{alias} said nothing about itself"),
        );
        return ExitCode::FAILURE;
    };
    let _printed = line(&mut std::io::stdout(), &shaped(&alias, &view.settled));
    ExitCode::SUCCESS
}

/// A host's model as one object.
fn shaped(alias: &str, held: &HostModel) -> Value {
    object(vec![
        ("host", text(alias)),
        ("generation", Value::Whole(held.generation.0)),
        (
            "sessions",
            Value::List(held.sessions.iter().map(sessioned).collect()),
        ),
    ])
}

/// One session, with the tabs under it.
fn sessioned(held: &Session) -> Value {
    object(vec![
        ("id", Value::Whole(held.id.0)),
        ("name", text(&held.name)),
        ("tabs", Value::List(held.tabs.iter().map(tabbed).collect())),
    ])
}

/// One tab, with the panes in it.
fn tabbed(held: &Tab) -> Value {
    object(vec![
        ("id", Value::Whole(held.id.0)),
        ("name", text(&held.name)),
        ("panes", Value::List(held.panes.iter().map(paned).collect())),
    ])
}

/// One pane, and what is known about it.
fn paned(held: &Pane) -> Value {
    object(vec![
        ("id", Value::Whole(held.id.0)),
        ("title", text(&held.title)),
        (
            "working_directory",
            held.working_directory
                .as_ref()
                .map_or(Value::Null, |named| text(named)),
        ),
        ("columns", Value::Whole(u64::from(held.columns))),
        ("rows", Value::Whole(u64::from(held.rows))),
    ])
}
