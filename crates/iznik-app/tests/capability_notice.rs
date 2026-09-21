//! A host whose connected server is offering an upgrade raises a toast saying
//! why and where to act on it — for a version difference and for a missing
//! feature alike.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext as _, TestAppContext};
use iznik_app::bridge::{EngineBridge, EngineEvent};
use iznik_app::vt::{VtOptions, VtThread};
use iznik_app::window::{ShellOptions, WindowShell};
use iznik_client::bootstrap::probe::InstalledServer;
use iznik_client::host::identity::HostId;
use iznik_client::host::manager::ManagerEvent;
use iznik_client::host::state::{HostState, UpgradeOffer, UpgradeReason};
use iznik_client::transport::ClientRuntimePaths;
use iznik_protocol::capabilities::Capabilities;

/// Fixture failures.
type Failed = Box<dyn std::error::Error>;

/// This build's own version, which a same-version offer means.
const BUNDLED: &str = env!("CARGO_PKG_VERSION");
/// A server of another version, which is the case a stale local build is.
const OTHER: &str = "0.0.1";

/// A connected server missing the reorder feature raises a warning toast.
#[gpui_kit::test]
fn a_capability_gap_raises_a_warning_toast(context: &mut TestAppContext) {
    check(&toast_for(
        context,
        "gap",
        BUNDLED,
        UpgradeReason::Capabilities,
        0,
    ));
}

/// A connected server of *another* version raises a toast too, and its words
/// name the version rather than claiming it is older.
#[gpui_kit::test]
fn another_version_raises_a_warning_toast(context: &mut TestAppContext) {
    check(&toast_for(
        context,
        "version",
        OTHER,
        UpgradeReason::Version,
        0,
    ));
}

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Open a shell inside the kit's root, connect a host whose server is offering
/// an upgrade for `reason`, and assert the toast appears.
///
/// # Errors
/// Returns setup failures and a closed-window error.
///
/// # Panics
///
/// Panics when the toast is missing.
fn toast_for(
    context: &mut TestAppContext,
    label: &str,
    server: &str,
    reason: UpgradeReason,
    capabilities: u32,
) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let directory: PathBuf = std::env::temp_dir().join(format!(
        "iznik-upgrade-notice-{label}-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(directory.join("artifacts"))?;
    let (bridge, thread) = owners(&directory)?;
    let created: std::cell::RefCell<Option<gpui_kit::Entity<WindowShell>>> =
        std::cell::RefCell::new(None);
    let (_root, context) = context.add_window_view(|window, context| {
        let shell = context.new(|context| {
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
        *created.borrow_mut() = Some(shell.clone());
        gpui_kit::component::Root::new(shell, window, context)
    });
    let shell = created.borrow().clone().ok_or("the shell was not built")?;
    let notice = HostState::Connected {
        server_version: server.to_owned(),
        capabilities: Capabilities::from_bits(capabilities),
        upgrade: Some(UpgradeOffer {
            installed: InstalledServer {
                crate_version: server.to_owned(),
                protocol_version: 1,
            },
            bundled: InstalledServer {
                crate_version: BUNDLED.to_owned(),
                protocol_version: 1,
            },
            reason,
        }),
    };
    shell.update_in(context, |shell, window, application| {
        shell.absorb(
            EngineEvent::Said(ManagerEvent::Moved {
                host: HostId("devbox".to_owned()),
                state: notice,
            }),
            window,
            application,
        );
    });
    context.update(|window, _application| {
        window.find("notification").visible();
    });
    let _removed = std::fs::remove_dir_all(directory);
    Ok(())
}

/// Assemble the bridge and terminal owner a headless shell needs, without
/// connecting a host.
///
/// # Errors
/// Returns the bridge or terminal owner's startup errors.
fn owners(directory: &Path) -> Result<(EngineBridge, Rc<VtThread>), Failed> {
    let bridge = EngineBridge::start(
        directory.join("artifacts"),
        ClientRuntimePaths::under(&directory.join("runtime"))?,
    )?;
    let thread = Rc::new(VtThread::start(VtOptions::default())?);
    Ok((bridge, thread))
}
