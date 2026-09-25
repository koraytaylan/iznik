//! `pane: close` names a pane of the tab on screen, including when the window
//! has not yet recorded which pane has keyboard focus.

use std::rc::Rc;

use gpui_kit::TestAppContext;
use iznik_app::actions::ActionId;
use iznik_app::bridge::EngineEvent;
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_protocol::identity::{Generation, PaneId, SessionId, TabId};
use iznik_protocol::model::{
    HostModel, LayoutNode, Pane, Session, SplitDirection, Tab, Weighted, encode_host_model,
};

#[path = "support/engine.rs"]
mod engine;

/// Fixture setup and assertion failures.
type Failed = Box<dyn std::error::Error>;

/// Two panes side by side, so a close can name one of them.
fn model() -> HostModel {
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "shell".to_owned(),
                layout: LayoutNode::Split {
                    direction: SplitDirection::Horizontal,
                    children: vec![
                        Weighted {
                            node: LayoutNode::Leaf(PaneId(1)),
                            weight: 1,
                        },
                        Weighted {
                            node: LayoutNode::Leaf(PaneId(2)),
                            weight: 1,
                        },
                    ],
                },
                panes: vec![
                    Pane {
                        id: PaneId(1),
                        title: String::new(),
                        working_directory: None,
                        columns: 80,
                        rows: 24,
                    },
                    Pane {
                        id: PaneId(2),
                        title: String::new(),
                        working_directory: None,
                        columns: 80,
                        rows: 24,
                    },
                ],
            }],
        }],
    }
}

/// Keep fixture assertion outside the GPUI macro's generated documentation.
///
/// # Panics
/// Fails on a fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

#[gpui_kit::test]
fn closing_a_shown_pane_has_a_pane_to_act_on(context: &mut TestAppContext) {
    check(&shown(context));
}

/// A selected tab with panes makes `pane: close` name one. Recording focus
/// writes that pane on the window's model even when the scheduler does not
/// hold the host: the scheduler's copy is not the one the command is built from.
///
/// # Errors
/// Returns setup, encoding, or window failures.
///
/// # Panics
/// Panics when the close reports nothing to act on, or focus is not recorded.
fn shown(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let (bridge, directory) = engine::start("close-pane")?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, context| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                ..ShellOptions::default()
            },
            window,
            context,
        )
    });
    let host = HostId("fixture".to_owned());
    let model = model();
    handle.update(context, |shell, window, context| {
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Snapshot {
                host: host.clone(),
                generation: model.generation,
                payload: encode_host_model(&model)?,
            }),
            window,
            context,
        );
        Ok::<(), Failed>(())
    })??;
    handle.update(context, |shell, _, _| -> Result<(), Failed> {
        if shell.selected().is_none() {
            return Err("the snapshot left no tab on screen".into());
        }
        match shell.dispatch_action(ActionId::ClosePane) {
            Ok(false) => Err("pane: close reported that it has nothing to act on".into()),
            Ok(true) => Ok(()),
            Err(error) if error.to_string().contains("is not held") => Ok(()),
            Err(error) => Err(format!("close did not name a pane: {error}").into()),
        }
    })??;
    handle.update(context, |shell, _, _| -> Result<(), Failed> {
        let refused = shell.hosts_mut().focus(&host.0, Some(PaneId(2)));
        let Some(error) = refused.err() else {
            return Err("an unheld host accepted a focus".into());
        };
        if !error.to_string().contains("is not held") {
            return Err(format!("focus failed for another reason: {error}").into());
        }
        let recorded = shell
            .hosts()
            .state()
            .model()
            .host(&host)
            .and_then(|view| view.focus);
        if recorded == Some(PaneId(2)) {
            Ok(())
        } else {
            Err(format!("the window recorded {recorded:?} for the focused pane").into())
        }
    })??;
    drop(directory);
    Ok(())
}
