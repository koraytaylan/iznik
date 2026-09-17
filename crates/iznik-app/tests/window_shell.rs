//! Headless layout translation before the window's transport lifecycle proofs.

use std::collections::BTreeMap;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Bounds, Context, Entity, InteractiveElement, IntoElement, ParentElement, Pixels,
    Render, SharedString, Styled, TestAppContext, TestSupportExt, Window, WindowHandle, div, px,
    size,
};
use iznik_app::layout::render_layout;
use iznik_protocol::identity::PaneId;
use iznik_protocol::model::{LayoutNode, SplitDirection, Weighted};

/// Fixture setup and window failures.
type Failed = Box<dyn std::error::Error>;
/// The first leaf is the narrow left pane.
const LEFT: PaneId = PaneId(1);
/// The second leaf occupies the upper right.
const UPPER: PaneId = PaneId(2);
/// The third leaf occupies the lower right.
const LOWER: PaneId = PaneId(3);
/// The right branch takes three times the width of the left leaf.
const RIGHT_WEIGHT: u32 = 3;
/// Initial width chosen to divide exactly under the fixture's four weight units.
const WIDTH: f32 = 800.0;
/// Initial height chosen to divide exactly between the right leaves.
const HEIGHT: f32 = 400.0;
/// A larger viewport proves that weights survive a native window resize.
const LARGER_WIDTH: f32 = 1_200.0;
/// Layout rounding is permitted within one logical pixel.
const TOLERANCE: f32 = 1.0;
/// Several draw passes let the kit settle measurements after a resize.
const SETTLE_PASSES: usize = 4;

/// A stable leaf entity whose identity is independent of split wrappers.
struct Leaf {
    /// Identity observed in the laid-out element tree.
    pane: PaneId,
}

impl Render for Leaf {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .id(SharedString::from(format!("pane-{}", self.pane.0)))
            .test_support()
            .size_full()
    }
}

/// The window shell supplies a model tree and persistent pane entities to the renderer.
struct LayoutFixture {
    /// The model's current normalized layout.
    tree: LayoutNode,
    /// Revision changes when model weights change, not on ordinary redraws.
    revision: u64,
    /// Existing leaf entities reused under new split wrappers.
    panes: BTreeMap<PaneId, Entity<Leaf>>,
}

impl Render for LayoutFixture {
    fn render(
        &mut self,
        _window: &mut Window,
        _context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        div()
            .size_full()
            .child(render_layout(&self.tree, self.revision, |pane| {
                self.panes.get(&pane).map_or_else(
                    || div().into_any_element(),
                    |entity| entity.clone().into_any_element(),
                )
            }))
    }
}

/// Nested horizontal and vertical splits distinguish both axes and nonuniform weights.
fn tree(right: u32) -> LayoutNode {
    LayoutNode::Split {
        direction: SplitDirection::Horizontal,
        children: vec![
            Weighted {
                node: LayoutNode::Leaf(LEFT),
                weight: 1,
            },
            Weighted {
                weight: right,
                node: LayoutNode::Split {
                    direction: SplitDirection::Vertical,
                    children: vec![
                        Weighted {
                            node: LayoutNode::Leaf(UPPER),
                            weight: 1,
                        },
                        Weighted {
                            node: LayoutNode::Leaf(LOWER),
                            weight: 1,
                        },
                    ],
                },
            },
        ],
    }
}

/// Draw bounded settling frames, then inspect all leaf bounds.
///
/// # Errors
/// Returns a closed-window failure.
fn bounds(
    context: &mut TestAppContext,
    handle: WindowHandle<LayoutFixture>,
) -> Result<Vec<Bounds<Pixels>>, Failed> {
    for _pass in 0..SETTLE_PASSES {
        context.update_window(handle.into(), |_, window, application| {
            window.draw(application).clear(application);
        })?;
    }
    Ok(context.update_window(handle.into(), |_, window, _| {
        ["pane-1", "pane-2", "pane-3"]
            .into_iter()
            .map(|name| window.find(name).bounds())
            .collect()
    })?)
}

#[gpui_kit::test]
fn window_layout_preserves_weights_across_resize_and_reuses_leaf_entities(
    context: &mut TestAppContext,
) {
    check(&layout(context));
}

/// Measure the actual kit layout, then replace model weights without replacing leaves.
///
/// # Errors
/// Propagates GPUI window failures.
///
/// # Panics
/// Fails when actual geometry or retained entity identity disagrees with the model.
fn layout(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let handle = context.add_window(|_, context| LayoutFixture {
        tree: tree(RIGHT_WEIGHT),
        revision: 0,
        panes: [LEFT, UPPER, LOWER]
            .into_iter()
            .map(|pane| (pane, context.new(|_| Leaf { pane })))
            .collect(),
    });
    context.simulate_window_resize(handle.into(), size(px(WIDTH), px(HEIGHT)));
    assert_geometry(&bounds(context, handle)?, RIGHT_WEIGHT, WIDTH)?;
    let identities = handle.update(context, |fixture, _, _| {
        fixture
            .panes
            .values()
            .map(Entity::entity_id)
            .collect::<Vec<_>>()
    })?;
    context.simulate_window_resize(handle.into(), size(px(LARGER_WIDTH), px(HEIGHT)));
    assert_geometry(&bounds(context, handle)?, RIGHT_WEIGHT, LARGER_WIDTH)?;
    handle.update(context, |fixture, _, context| {
        fixture.tree = tree(1);
        fixture.revision = fixture.revision.saturating_add(1);
        context.notify();
    })?;
    assert_geometry(&bounds(context, handle)?, 1, LARGER_WIDTH)?;
    let after = handle.update(context, |fixture, _, _| {
        fixture
            .panes
            .values()
            .map(Entity::entity_id)
            .collect::<Vec<_>>()
    })?;
    assert_eq!(
        after, identities,
        "new weights retain the existing pane entities"
    );
    Ok(())
}

/// Assert both axes using the actual measured bounds, allowing pixel rounding only.
///
/// # Errors
/// Returns a missing-leaf failure.
///
/// # Panics
/// Fails if weighted geometry disagrees with the fixture.
fn assert_geometry(bounds: &[Bounds<Pixels>], right: u32, width: f32) -> Result<(), Failed> {
    let [left, upper, lower] = bounds else {
        return Err("three pane bounds are required".into());
    };
    let expected = f32::from(left.size.width).mul_add(f32::from(Pixels::from(right)), 0.0);
    assert!(
        f32::from(upper.size.width).mul_add(1.0, -expected).abs() <= TOLERANCE,
        "weighted widths: {bounds:?}"
    );
    assert!(
        f32::from(left.size.width)
            .mul_add(1.0, f32::from(upper.size.width))
            .mul_add(1.0, -width)
            .abs()
            <= TOLERANCE,
        "panes fill the actual window width: {bounds:?}"
    );
    assert!(
        f32::from(upper.size.height)
            .mul_add(1.0, -f32::from(lower.size.height))
            .abs()
            <= TOLERANCE,
        "equal vertical split: {bounds:?}"
    );
    assert_eq!(
        left.size.height,
        px(HEIGHT),
        "left leaf fills the window height"
    );
    assert_eq!(
        upper.origin.x, lower.origin.x,
        "right leaves share an origin"
    );
    assert!(
        f32::from(left.origin.x)
            .mul_add(1.0, f32::from(left.size.width))
            .mul_add(1.0, -f32::from(upper.origin.x))
            .abs()
            <= TOLERANCE,
        "horizontal split has no gap"
    );
    assert!(
        f32::from(upper.origin.y)
            .mul_add(1.0, f32::from(upper.size.height))
            .mul_add(1.0, -f32::from(lower.origin.y))
            .abs()
            <= TOLERANCE,
        "vertical split has no gap"
    );
    Ok(())
}

/// Keep fixture assertion outside the GPUI macro's generated test documentation.
///
/// # Panics
/// Fails on a fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

#[path = "support/engine.rs"]
mod engine;

/// Enough cells to inspect retained text across two ordered feeds.
const TERMINAL_COLUMNS: u16 = 32;
/// Small height keeps this owner-routing proof independent of history layout.
const TERMINAL_ROWS: u16 = 2;
/// A missing native reply fails promptly rather than hanging the headless window.
const REPLY_DEADLINE: std::time::Duration = std::time::Duration::from_secs(2);
/// Yield briefly to the dedicated VT owner between nonblocking window updates.
const REPLY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(1);

/// A normalized authoritative model containing the same weighted tree as the geometry fixture.
fn model() -> iznik_protocol::model::HostModel {
    use iznik_protocol::identity::{Generation, SessionId, TabId};
    use iznik_protocol::model::{HostModel, Pane, Session, Tab};
    HostModel {
        generation: Generation(1),
        sessions: vec![Session {
            id: SessionId(1),
            name: "work".to_owned(),
            tabs: vec![Tab {
                id: TabId(1),
                name: "terminals".to_owned(),
                layout: tree(RIGHT_WEIGHT),
                panes: [LEFT, UPPER, LOWER]
                    .into_iter()
                    .map(|id| Pane {
                        id,
                        title: String::new(),
                        working_directory: None,
                        columns: TERMINAL_COLUMNS,
                        rows: TERMINAL_ROWS,
                    })
                    .collect(),
            }],
        }],
    }
}

/// Host-qualified pane identity used by synthetic model and owner events.
fn pane_key() -> iznik_app::vt::PaneKey {
    iznik_app::vt::PaneKey {
        host: iznik_client::host::identity::HostId("fixture".to_owned()),
        pane: LEFT,
    }
}

/// Submit a model/lifecycle event through the actual shell's single ingress path.
///
/// # Errors
/// Returns a closed-window failure.
fn absorb(
    context: &mut TestAppContext,
    handle: WindowHandle<iznik_app::window::WindowShell>,
    event: iznik_client::host::manager::ManagerEvent,
) -> Result<(), Failed> {
    handle.update(context, |shell, window, context| {
        shell.absorb(iznik_app::bridge::EngineEvent::Said(event), window, context);
    })?;
    Ok(())
}

/// Pump until the pane's native frame reaches the named sequence.
///
/// # Errors
/// Returns a closed window or a missing native reply under the fixture deadline.
fn wait_frame(
    context: &mut TestAppContext,
    handle: WindowHandle<iznik_app::window::WindowShell>,
    ready: impl Fn(&iznik_app::vt::TerminalSnapshot) -> bool,
) -> Result<iznik_app::vt::TerminalSnapshot, Failed> {
    let started = std::time::Instant::now();
    loop {
        let snapshot = handle.update(context, |shell, window, context| {
            shell.update(window, context);
            shell.surface(&pane_key()).and_then(|surface| {
                surface
                    .read(context)
                    .grid()
                    .read(context)
                    .snapshot()
                    .cloned()
            })
        })?;
        if let Some(snapshot) = snapshot
            && ready(&snapshot)
        {
            return Ok(snapshot);
        }
        if started.elapsed() >= REPLY_DEADLINE {
            return Err("shell frame deadline".into());
        }
        std::thread::sleep(REPLY_INTERVAL);
    }
}

#[gpui_kit::test]
fn window_model_updates_keep_surface_and_terminal_identity(context: &mut TestAppContext) {
    check(&model_routing(context));
}

/// Model deltas and a disconnected lifecycle event cannot replace the held terminal.
///
/// # Errors
/// Propagates fixture, model encoding, window and native frame failures.
///
/// # Panics
/// Fails if model changes replace pane identities or lose the native terminal content.
fn model_routing(context: &mut TestAppContext) -> Result<(), Failed> {
    use iznik_client::host::manager::ManagerEvent;
    use iznik_client::host::state::HostState;
    use iznik_protocol::delta::{Delta, encode_delta};
    use iznik_protocol::identity::{Generation, Sequence, SessionId, TabId};
    let (handle, _directory) = open_shell(context)?;
    let host = pane_key().host;
    let identity = handle.update(context, |shell, _, _| {
        shell.surface(&pane_key()).map(Entity::entity_id)
    })?;
    assert!(identity.is_some(), "snapshot creates the visible surface");

    absorb(
        context,
        handle,
        ManagerEvent::Delta {
            host: host.clone(),
            generation: Generation(2),
            payload: encode_delta(&Delta::LayoutChanged {
                tab: TabId(1),
                layout: tree(1),
            })?,
        },
    )?;
    absorb(
        context,
        handle,
        ManagerEvent::Moved {
            host: host.clone(),
            state: HostState::Disconnected,
        },
    )?;
    absorb(
        context,
        handle,
        ManagerEvent::Detached {
            host: host.clone(),
            pane: LEFT,
        },
    )?;
    let retained = handle.update(context, |shell, _, _| {
        shell.surface(&pane_key()).map(Entity::entity_id)
    })?;
    assert_eq!(
        retained, identity,
        "layout and transport state retain the surface entity"
    );
    let bytes = b" after";
    absorb(
        context,
        handle,
        ManagerEvent::Bytes {
            host: host.clone(),
            pane: LEFT,
            sequence: Sequence(0),
            bytes: bytes.to_vec(),
            receipt: None,
        },
    )?;
    let sequence = Sequence(u64::try_from(bytes.len())?);
    let snapshot = wait_frame(context, handle, |snapshot| snapshot.sequence == sequence)?;
    let text: String = snapshot
        .rows
        .first()
        .ok_or("first row")?
        .iter()
        .map(|cell| cell.text.as_str())
        .collect();
    assert!(
        text.starts_with("before after"),
        "the same native terminal receives following bytes: {text:?}"
    );
    absorb(
        context,
        handle,
        ManagerEvent::Delta {
            host,
            generation: Generation(3),
            payload: encode_delta(&Delta::SessionRemoved {
                session: SessionId(1),
            })?,
        },
    )?;
    handle.update(context, |shell, _, _| {
        assert!(
            shell.surface(&pane_key()).is_none(),
            "actual model removal releases the surface"
        );
        assert!(
            shell.selected().is_none(),
            "removing the last session clears selection"
        );
    })?;
    Ok(())
}

/// Open a real shell with an unattached engine, a model and one initialized native pane.
///
/// # Errors
/// Propagates fixture, encoding, window and native reply failures.
fn open_shell(
    context: &mut TestAppContext,
) -> Result<
    (
        WindowHandle<iznik_app::window::WindowShell>,
        engine::Directory,
    ),
    Failed,
> {
    use iznik_app::vt::{VtOptions, VtThread};
    use iznik_app::window::{ShellOptions, WindowShell};
    use iznik_client::host::manager::ManagerEvent;
    use iznik_protocol::identity::{Generation, Sequence};
    use iznik_protocol::model::encode_host_model;
    context.update(gpui_kit::init);
    let (bridge, directory) = engine::start("window-model")?;
    let thread = std::rc::Rc::new(VtThread::start(VtOptions::default())?);
    let handle = context.add_window(|window, context| {
        WindowShell::new(
            bridge,
            thread,
            ShellOptions {
                update_interval: None,
                maximum_events_per_update: 1,
                ..ShellOptions::default()
            },
            window,
            context,
        )
    });
    let host = pane_key().host;
    absorb(
        context,
        handle,
        ManagerEvent::Snapshot {
            host: host.clone(),
            generation: Generation(1),
            payload: encode_host_model(&model())?,
        },
    )?;
    absorb(
        context,
        handle,
        ManagerEvent::Screen {
            host: host.clone(),
            pane: LEFT,
            sequence: Sequence(0),
            columns: TERMINAL_COLUMNS,
            rows: TERMINAL_ROWS,
            bytes: b"before".to_vec(),
        },
    )?;
    wait_frame(context, handle, |snapshot| snapshot.sequence == Sequence(0))?;
    Ok((handle, directory))
}

/// Smaller model dimensions prove that deltas resize the emulator, not only its wrapper.
const RESIZED_COLUMNS: u16 = 16;

#[gpui_kit::test]
fn window_applies_model_dimensions_and_renders_host_state(context: &mut TestAppContext) {
    check(&dimensions_and_banner(context));
}

/// Exercise native resize routing and actual kit banners in the production shell renderer.
///
/// # Errors
/// Propagates fixture, encoding, window and owner failures.
///
/// # Panics
/// Fails if the emulator keeps stale dimensions or the classified host state is not visible.
fn dimensions_and_banner(context: &mut TestAppContext) -> Result<(), Failed> {
    use iznik_client::host::manager::ManagerEvent;
    use iznik_client::host::state::HostState;
    use iznik_protocol::delta::{Delta, encode_delta};
    use iznik_protocol::identity::Generation;
    let (handle, _directory) = open_shell(context)?;
    absorb(
        context,
        handle,
        ManagerEvent::Delta {
            host: pane_key().host,
            generation: Generation(2),
            payload: encode_delta(&Delta::PaneResized {
                pane: LEFT,
                columns: RESIZED_COLUMNS,
                rows: TERMINAL_ROWS,
            })?,
        },
    )?;
    let snapshot = wait_frame(context, handle, |snapshot| {
        snapshot.columns == RESIZED_COLUMNS
    })?;
    assert_eq!(
        snapshot.rows.len(),
        usize::from(TERMINAL_ROWS),
        "authoritative height reaches the owner"
    );
    absorb(
        context,
        handle,
        ManagerEvent::Moved {
            host: pane_key().host,
            state: HostState::Connecting,
        },
    )?;
    context.simulate_window_resize(handle.into(), size(px(WIDTH), px(HEIGHT)));
    for _pass in 0..SETTLE_PASSES {
        context.update_window(handle.into(), |_, window, application| {
            window.draw(application).clear(application);
        })?;
    }
    context.update_window(handle.into(), |_, window, _| {
        assert!(
            window.find("window-shell").visible(),
            "production shell rendered"
        );
        assert!(
            window.find("pane-area").visible(),
            "pane area remains visible during connection changes"
        );
        assert_eq!(
            window.find("host-banner-fixture").label(),
            Some("fixture: connecting"),
            "banner uses the engine's classified state"
        );
    })?;
    Ok(())
}

/// Font size applied by `apply_theme`, distinct from the theme default.
const APPLIED_FONT_SIZE: f32 = 22.0;

#[gpui_kit::test]
fn window_starts_with_its_own_settings_font_not_the_chrome_default(context: &mut TestAppContext) {
    check(&starts_with_its_own_font(context));
}

/// A freshly constructed shell's own settings font reaches the kit's chrome
/// immediately, so the settings window opened right after startup shows the
/// same size it is actually drawn with, instead of the kit's unrelated
/// built-in default until the first edit applies a theme.
///
/// # Errors
/// Propagates fixture and window failures.
///
/// # Panics
/// Fails if the kit's chrome keeps its own built-in font past construction.
fn starts_with_its_own_font(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::component::Theme;
    use iznik_app::theme::AppTheme;
    let (_handle, _directory) = open_shell(context)?;
    let expected = AppTheme::default();
    context.update(|app| {
        let chrome_theme = Theme::global(app);
        assert_eq!(
            chrome_theme.font_family,
            SharedString::from(expected.font_family),
            "the shell's own default font family reaches the kit's chrome at construction"
        );
        assert_eq!(
            chrome_theme.font_size,
            px(expected.font_size),
            "the shell's own default font size reaches the kit's chrome at construction"
        );
    });
    Ok(())
}

#[gpui_kit::test]
fn window_propagates_theme_metrics_to_every_pane(context: &mut TestAppContext) {
    check(&metrics_reach_pane(context));
}

/// Applying a changed theme updates a retained pane's font and size immediately,
/// the same way it already updates the terminal's colors, and also restyles
/// the kit's own chrome text, not only terminal content.
///
/// # Errors
/// Propagates fixture, window and owner failures.
///
/// # Panics
/// Fails if the pane's grid keeps its old font or size after the theme
/// changes, or if the kit's own UI font and size do not follow it too.
fn metrics_reach_pane(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::component::Theme;
    use iznik_app::grid::GridMetrics;
    use iznik_app::theme::AppTheme;
    let (handle, _directory) = open_shell(context)?;
    let theme = AppTheme {
        font_family: "Custom Mono".to_owned(),
        font_size: APPLIED_FONT_SIZE,
        ..AppTheme::default()
    };
    handle.update(context, |shell, _, context| {
        shell.apply_theme(&theme, context);
    })?;
    context.update(|app| {
        let chrome_theme = Theme::global(app);
        assert_eq!(
            chrome_theme.font_family,
            SharedString::from("Custom Mono"),
            "the applied theme's font family also reaches the kit's own UI text"
        );
        assert_eq!(
            chrome_theme.font_size,
            px(APPLIED_FONT_SIZE),
            "the applied theme's font size also reaches the kit's own UI text"
        );
        assert_eq!(
            chrome_theme.mono_font_family,
            SharedString::from("Custom Mono"),
            "the applied theme's font family reaches the kit's monospace UI text too"
        );
        assert_eq!(
            chrome_theme.mono_font_size,
            px(APPLIED_FONT_SIZE),
            "the applied theme's font size reaches the kit's monospace UI text too"
        );
    });
    let metrics = handle
        .update(context, |shell, _, context| {
            shell
                .surface(&pane_key())
                .map(|surface| surface.read(context).grid().read(context).metrics().clone())
        })?
        .ok_or("pane surface exists")?;
    assert_eq!(
        metrics.font,
        SharedString::from("Custom Mono"),
        "the applied theme's font family reaches the pane's grid"
    );
    assert_eq!(
        metrics.font_size,
        px(APPLIED_FONT_SIZE),
        "the applied theme's font size reaches the pane's grid"
    );
    assert_ne!(
        metrics.cell_width,
        GridMetrics::default().cell_width,
        "the changed font size re-measures the cell width instead of clipping to the old one"
    );
    assert_ne!(
        metrics.line_height,
        GridMetrics::default().line_height,
        "the changed font size re-measures the line height instead of clipping to the old one"
    );
    Ok(())
}
