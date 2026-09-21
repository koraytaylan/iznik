//! The Add Host flow over a person's ssh configuration: an alias the
//! configuration defines is offered and held, and a name it does not gets
//! asked for an address — which is written as a block before the host is
//! held.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::TestAppContext;
use iznik_app::actions::ActionId;
use iznik_app::bridge::EngineBridge;
use iznik_app::host_ui::EngineState;
use iznik_app::palette::{self, Palette};
use iznik_app::prompt::{Answer, Expected, Step, answer, begin_with, choices};
use iznik_app::ssh_config::parsed_aliases;
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::transport::ClientRuntimePaths;
use iznik_testkit::stack::{Stack, StackOptions};

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;
/// Poll interval for the live stack.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// How long the local stack's host may take to say anything.
const LIVE_DEADLINE: Duration = Duration::from_secs(10);

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

#[test]
/// Add Host offers the aliases the ssh configuration defines, and choosing
/// one adds it at once.
///
/// # Panics
///
/// Panics when the prompt does not list the configuration's aliases or
/// choosing one does not add it.
fn add_host_offers_the_configured_aliases() {
    let aliases = vec!["devbox".to_owned(), "builder".to_owned()];
    let state = EngineState::new();
    let Some(Step::Ask(prompt)) = begin_with(ActionId::AddHost, &state, None, &aliases) else {
        panic!("Add Host asks");
    };
    let offered: Vec<String> = choices(&prompt, "")
        .into_iter()
        .map(|choice| choice.label)
        .collect();
    assert_eq!(offered, aliases);
    assert_eq!(
        answer(&prompt, "devbox", 0),
        Some(Answer::AddHost("devbox".into()))
    );
}

#[test]
/// A name the configuration does not define is offered as one row that says
/// what choosing it does, and its answer carries the alias on to the address
/// question.
///
/// # Panics
///
/// Panics when the typed row is absent or its answer is not the alias.
fn a_name_the_configuration_does_not_hold_offers_to_write_it() {
    let aliases = vec!["devbox".to_owned()];
    let state = EngineState::new();
    let Some(Step::Ask(prompt)) = begin_with(ActionId::AddHost, &state, None, &aliases) else {
        panic!("Add Host asks");
    };
    let labels: Vec<String> = choices(&prompt, "new")
        .into_iter()
        .map(|choice| choice.label)
        .collect();
    assert_eq!(
        labels,
        ["Add \u{201C}new\u{201D} to your ssh configuration\u{2026}"]
    );
    assert_eq!(
        answer(&prompt, "new", 0),
        Some(Answer::AddHost("new".into()))
    );
    assert!(matches!(prompt.expected, Expected::Alias { .. }));
}

#[test]
/// A `unix:` answer is its own row and needs no configuration written.
///
/// # Panics
///
/// Panics when the socket row does not say the socket or names the
/// configuration.
fn a_socket_answer_is_offered_as_itself() {
    let state = EngineState::new();
    let Some(Step::Ask(prompt)) = begin_with(ActionId::AddHost, &state, None, &[]) else {
        panic!("Add Host asks");
    };
    let labels: Vec<String> = choices(&prompt, "unix:/tmp/iznik.sock")
        .into_iter()
        .map(|choice| choice.label)
        .collect();
    assert_eq!(labels, ["unix:/tmp/iznik.sock"]);
    assert_eq!(
        answer(&prompt, "unix:/tmp/iznik.sock", 0),
        Some(Answer::AddHost("unix:/tmp/iznik.sock".into()))
    );
}

#[test]
/// The address question's answer is the alias-and-address pair the stanza is
/// written from.
///
/// # Panics
///
/// Panics when the address answer differs.
fn the_address_question_answers_with_the_alias_and_address() {
    let prompt = iznik_app::prompt::host_address_prompt("new".to_owned());
    assert_eq!(
        answer(&prompt, " 10.0.0.2 ", 0),
        Some(Answer::AddHostWithAddress {
            alias: "new".to_owned(),
            address: "10.0.0.2".to_owned(),
        })
    );
    assert_eq!(answer(&prompt, "  ", 0), None);
}

#[gpui_kit::test]
/// A shell pointed at a scratch ssh configuration discovers its aliases,
/// writes a stanza on request, and adds a local host through the palette.
fn the_add_host_flow_reads_and_writes_the_configuration(context: &mut TestAppContext) {
    check(&flow(context));
}

/// A scratch ssh configuration holding one alias.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

/// Allocate the scratch directory and its configuration.
///
/// # Errors
///
/// Returns filesystem failures.
fn scratch() -> Result<Scratch, Failed> {
    let directory = std::env::temp_dir().join(format!("iznik-add-host-{}", std::process::id()));
    let _removed = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(directory.join("artifacts"))?;
    std::fs::write(
        directory.join("config"),
        "Host configured\n    HostName 10.0.0.1\n",
    )?;
    Ok(Scratch(directory))
}

/// Drive discovery, the block write and the palette's local add.
///
/// # Errors
///
/// Returns setup, engine, or deadline failures.
///
/// # Panics
///
/// Panics when discovery, the write, or the add differs.
fn flow(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
    let scratch = scratch()?;
    let config = scratch.0.join("config");
    let bridge = EngineBridge::start(
        scratch.0.join("artifacts"),
        ClientRuntimePaths::under(&scratch.0.join("runtime"))?,
    )?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, application| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ssh_config_path: Some(config.clone()),
                ..ShellOptions::default()
            },
            window,
            application,
        )
    });
    reads_the_aliases(context, handle, &config)?;
    asks_and_writes(context, handle, &config)?;
    a_socket_needs_nothing_written(context, handle, &stack)?;
    let _removed = std::fs::remove_dir_all(&scratch.0);
    drop(stack);
    Ok(())
}

/// The configuration's own aliases are read, and a direct write appends.
///
/// # Errors
///
/// Returns a window or filesystem failure.
///
/// # Panics
///
/// Panics when the aliases or the appended block differ.
fn reads_the_aliases(
    context: &mut TestAppContext,
    handle: gpui_kit::WindowHandle<WindowShell>,
    config: &Path,
) -> Result<(), Failed> {
    let (aliases, defines) = handle.update(context, |shell, _, _| {
        (shell.ssh_alias(), shell.ssh_defines("configured"))
    })?;
    assert_eq!(aliases, ["configured"], "the configuration is discovered");
    assert!(defines, "the configured alias is known to be defined");
    assert!(
        !handle.update(context, |shell, _, _| shell.ssh_defines("absent"))?,
        "an alias nothing defines is not one"
    );
    handle.update(context, |shell, _, _| {
        shell.add_host_with_address("created", "10.0.0.2")
    })??;
    let after = std::fs::read_to_string(config)?;
    assert_eq!(
        parsed_aliases(&after),
        ["configured", "created"],
        "the new block is in the configuration"
    );
    Ok(())
}

/// A name the configuration does not define asks its address, and answering
/// writes the block and holds the host.
///
/// # Errors
///
/// Returns a window, filesystem, or deadline failure.
///
/// # Panics
///
/// Panics when the question does not follow or nothing is written.
fn asks_and_writes(
    context: &mut TestAppContext,
    handle: gpui_kit::WindowHandle<WindowShell>,
    config: &Path,
) -> Result<(), Failed> {
    let asked = handle.update(context, |shell, window, application| {
        let mut palette_state = Palette::default();
        palette_state.open();
        palette::dispatch_action(
            shell,
            &mut palette_state,
            ActionId::AddHost,
            window,
            application,
        )?;
        palette_state.set_query("not-configured");
        let sent = palette::submit_prompt(shell, &mut palette_state);
        let expected = palette_state
            .prompt
            .as_ref()
            .map(|prompt| prompt.expected.clone());
        assert!(
            shell.hosts().state().hosts().next().is_none(),
            "nothing is held while the address is unknown"
        );
        sent.map(|_sent| expected)
    })??;
    assert!(
        matches!(&asked, Some(Expected::HostAddress { alias: asked_alias }) if asked_alias == "not-configured"),
        "the address question follows: {asked:?}"
    );
    let wrote = handle.update(context, |shell, _window, _application| {
        let mut palette_state = Palette::default();
        palette_state.open();
        palette_state.ask(iznik_app::prompt::host_address_prompt(
            "not-configured".to_owned(),
        ));
        palette_state.set_query("10.0.0.3");
        palette::submit_prompt(shell, &mut palette_state)
    })??;
    assert!(wrote, "the answer is sent");
    let after = std::fs::read_to_string(config)?;
    assert_eq!(
        parsed_aliases(&after),
        ["configured", "created", "not-configured"],
        "the address answer wrote the block"
    );
    wait_for(context, handle, |shell| {
        shell
            .hosts()
            .state()
            .host(&HostId("not-configured".to_owned()))
            .is_some()
    })
}

/// A local socket adds through the palette without touching the
/// configuration.
///
/// # Errors
///
/// Returns a window, engine, or deadline failure.
///
/// # Panics
///
/// Panics when the socket alias is not added.
fn a_socket_needs_nothing_written(
    context: &mut TestAppContext,
    handle: gpui_kit::WindowHandle<WindowShell>,
    stack: &Stack,
) -> Result<(), Failed> {
    let alias = HostId(format!("unix:{}", stack.socket().display()));
    let added = handle.update(context, |shell, window, application| {
        let mut palette_state = Palette::default();
        palette_state.open();
        palette::dispatch_action(
            shell,
            &mut palette_state,
            ActionId::AddHost,
            window,
            application,
        )?;
        palette_state.set_query(alias.0.clone());
        palette::submit_prompt(shell, &mut palette_state)
    })??;
    assert!(added, "a socket alias is added");
    wait_for(context, handle, |shell| {
        shell.hosts().state().host(&alias).is_some_and(|report| {
            matches!(
                report.connection,
                iznik_client::host::state::HostState::Connected { .. }
            )
        })
    })
}

/// Drive shell updates until a live-stack predicate becomes true.
///
/// # Errors
///
/// Returns a closed-window error or the live deadline error.
fn wait_for(
    context: &mut TestAppContext,
    handle: gpui_kit::WindowHandle<WindowShell>,
    predicate: impl Fn(&WindowShell) -> bool,
) -> Result<(), Failed> {
    let Some(deadline) = Instant::now().checked_add(LIVE_DEADLINE) else {
        return Err("live stack deadline could not be represented".into());
    };
    loop {
        let reached = handle.update(context, |shell, window, application| {
            shell.update(window, application);
            predicate(shell)
        })?;
        if reached {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("live stack deadline expired".into());
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}
