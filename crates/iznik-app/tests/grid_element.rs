//! Headless rendering and viewport proofs over the existing fidelity corpus.

use budget::support;

#[path = "../benches/grid_budget.rs"]
mod budget;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{AppContext, TestAppContext, WindowHandle, px};
use iznik_app::grid::{GridMetrics, GridPosition, GridSelection, TerminalGrid, draw_list};
use iznik_app::vt::{VtCommand, VtOptions, VtThread};
use iznik_protocol::identity::Sequence;
use libghostty_vt::terminal::ScrollViewport;
use support::{key, open, snapshot};

/// Failure propagated out of a fixture helper into its test assertion.
type Failed = Box<dyn std::error::Error>;

/// Convert fixture failures into a named assertion outside the GPUI macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

/// Width of the committed fidelity corpus viewport.
const CORPUS_COLUMNS: u16 = 80;
/// Height of the committed fidelity corpus viewport.
const CORPUS_ROWS: u16 = 24;
/// Small width sufficient to isolate one row change.
const DAMAGE_COLUMNS: u16 = 12;
/// Four rows demonstrate that three siblings remain cached.
const DAMAGE_ROWS: u16 = 4;
/// A changed row is painted once initially and once after the update.
const UPDATED_PAINTS: u64 = 2;

/// Draw once without retaining a borrow of the root entity.
///
/// # Errors
/// Returns a closed-window error.
fn draw(context: &mut TestAppContext, handle: WindowHandle<TerminalGrid>) -> Result<(), Failed> {
    context.update_window(handle.into(), |_view, window, application| {
        window.draw(application).clear(application);
    })?;
    Ok(())
}

#[gpui_kit::test]
fn grid_renders_the_corpus_from_owned_snapshots(context: &mut TestAppContext) {
    check(&render_corpus(context));
}

/// Render every corpus construct and inspect the observed surface and draw list.
///
/// # Errors
/// Returns terminal, thread, grid or window failures.
///
/// # Panics
/// Fails when layout, cell mapping or painting disagrees with its snapshot.
fn render_corpus(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    for construct in iznik_testkit::corpus::constructs() {
        let mut current = open(&thread, Sequence(0), CORPUS_COLUMNS, CORPUS_ROWS)?;
        handle.update(context, |grid, _window, context| {
            grid.apply(current.clone(), context)
        })??;
        for bytes in construct.chunks {
            thread.send(VtCommand::Feed {
                receipt: None,
                key: key(),
                sequence: current.sequence,
                bytes,
            })?;
            current = snapshot(&thread)?;
        }
        let drawings = draw_list(&current, None)?;
        assert_eq!(
            drawings.len(),
            current.rows.len(),
            "{} rows",
            construct.name
        );
        for (drawing, cells) in drawings.iter().zip(&current.rows) {
            let text: String = drawing.runs.iter().map(|run| run.text.as_str()).collect();
            let expected: String = cells
                .iter()
                .filter(|cell| cell.width != libghostty_vt::screen::CellWide::SpacerTail)
                .map(|cell| {
                    if cell.text.is_empty() {
                        " "
                    } else {
                        &cell.text
                    }
                })
                .collect();
            assert_eq!(text, expected, "{} text mapping", construct.name);
        }
        let columns = current.columns;
        handle.update(context, |grid, _window, context| {
            grid.apply(current, context)
        })??;
        draw(context, handle)?;
        handle.update(context, |grid, window, application| {
            assert!(
                grid.paint_errors(application).is_empty(),
                "{} paint errors",
                construct.name
            );
            assert_eq!(
                window.find("terminal-grid").bounds().size.width,
                px(f32::from(columns) * f32::from(GridMetrics::default().cell_width)),
                "grid cell width"
            );
        })?;
    }
    Ok(())
}

#[gpui_kit::test]
fn grid_reuses_idle_rows_and_repaints_only_changed_rows(context: &mut TestAppContext) {
    check(&damage(context));
}

/// Count actual custom-element paints across unchanged and one-row updates.
///
/// # Errors
/// Returns terminal, thread, grid or window failures.
///
/// # Panics
/// Fails if an idle row is painted again or a changed row is reused.
fn damage(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let initial = open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    handle.update(context, |grid, _window, context| {
        grid.apply(initial.clone(), context)
    })??;
    draw(context, handle)?;
    let before = handle.update(context, |grid, _window, application| {
        grid.paint_counts(application)
    })?;
    assert!(
        before.iter().all(|count| *count == 1),
        "each row painted once: {before:?}"
    );
    let changed = handle.update(context, |grid, _window, context| {
        grid.apply(initial, context)
    })??;
    assert_eq!(changed, 0, "unchanged drawing");
    draw(context, handle)?;
    assert_eq!(
        handle.update(context, |grid, _window, application| grid
            .paint_counts(application))?,
        before,
        "cached row scenes"
    );
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"\x1b7\x1b[2;1Hchanged\x1b8".to_vec(),
    })?;
    let next = snapshot(&thread)?;
    let repainted =
        handle.update(context, |grid, _window, context| grid.apply(next, context))??;
    assert_eq!(repainted, 1, "one row drawing differs");
    draw(context, handle)?;
    let after = handle.update(context, |grid, _window, application| {
        grid.paint_counts(application)
    })?;
    assert_eq!(
        after,
        vec![1, UPDATED_PAINTS, 1, 1],
        "only the changed row painted again"
    );
    Ok(())
}

/// Scrollback remains emulator-owned and follows output only from the bottom.
///
/// # Panics
/// Fails if historical content moves unexpectedly or scrolling earns stream credit.
#[test]
fn grid_viewport_keeps_history_position_until_returning_to_bottom() {
    let thread = VtThread::start(VtOptions {
        scrollback_bytes: 1_000_000,
        ..VtOptions::default()
    })
    .expect("thread");
    open(&thread, Sequence(0), 20, 3).expect("open");
    let bytes = b"first\r\nsecond\r\nthird\r\nfourth\r\nfifth".to_vec();
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(0),
            bytes,
        })
        .expect("feed");
    let bottom = snapshot(&thread).expect("bottom");
    assert!(bottom.viewport.at_bottom());
    thread
        .send(VtCommand::Scroll {
            key: key(),
            scroll: ScrollViewport::Top,
        })
        .expect("top");
    let top = snapshot(&thread).expect("top snapshot");
    assert_eq!(top.viewport.offset, 0);
    assert!(!top.viewport.at_bottom());
    assert_eq!(top.sequence, bottom.sequence);
    assert_eq!(top.consumed_bytes, 0);
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: bottom.sequence,
            bytes: b"\r\nsixth\r\nseventh".to_vec(),
        })
        .expect("more output");
    let held = snapshot(&thread).expect("held");
    assert_eq!(held.rows, top.rows);
    assert_eq!(held.viewport.offset, top.viewport.offset);
    thread
        .send(VtCommand::Scroll {
            key: key(),
            scroll: ScrollViewport::Bottom,
        })
        .expect("bottom again");
    let current = snapshot(&thread).expect("current");
    assert!(current.viewport.at_bottom());
    assert_ne!(current.rows, top.rows);
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: current.sequence,
            bytes: b"\r\neighth".to_vec(),
        })
        .expect("follow");
    assert!(snapshot(&thread).expect("followed").viewport.at_bottom());
}

/// Runs retain terminal columns, colors and styles; overlays use the same cells.
///
/// # Panics
/// Fails if wide continuations duplicate text or selection/cursor geometry drifts.
#[test]
fn grid_maps_styles_wide_cells_cursor_and_selection() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    open(&thread, Sequence(0), 12, 3).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: key(),
            sequence: Sequence(0),
            bytes: "ab\u{1b}[1;3;38;2;10;20;30m\u{6f22}e\u{301}\u{1b}[0m!"
                .as_bytes()
                .to_vec(),
        })
        .expect("feed");
    let current = snapshot(&thread).expect("snapshot");
    let selection = GridSelection {
        anchor: GridPosition { row: 0, column: 5 },
        head: GridPosition { row: 0, column: 1 },
    };
    let drawings = draw_list(&current, Some(selection)).expect("draw list");
    let row = drawings.first().expect("row");
    assert_eq!(row.selection, Some(1..5));
    let wide = row
        .runs
        .iter()
        .find(|run| run.text == "\u{6f22}")
        .expect("wide run");
    assert_eq!((wide.column, wide.columns), (2, 2));
    assert!(wide.style.bold && wide.style.italic);
    assert_eq!(
        wide.foreground,
        libghostty_vt::style::RgbColor {
            r: 10,
            g: 20,
            b: 30
        }
    );
    let combined = row
        .runs
        .iter()
        .find(|run| run.text == "e\u{301}")
        .expect("combining run");
    assert_eq!((combined.column, combined.columns), (4, 1));
    assert_eq!(row.cursor.as_ref().expect("cursor").column, 6);
}

#[gpui_kit::test]
fn grid_dispatches_wheel_and_keyboard_history_requests(context: &mut TestAppContext) {
    check(&navigation(context));
}

/// Exercise platform input dispatch and preserve sub-row wheel movement.
///
/// # Errors
/// Returns terminal or grid failures.
///
/// # Panics
/// Fails if events target another pane, lose wheel motion or change the snapshot.
fn navigation(context: &mut TestAppContext) -> Result<(), Failed> {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui_kit::{Focusable, ScrollDelta, ScrollWheelEvent, point};
    use iznik_app::grid::{GridInput, GridScroll};

    context.update(gpui_kit::init);
    let (view, context) = context
        .add_window_view(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let initial = open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    view.update(context, |grid, context| grid.apply(initial, context))?;
    let requests = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&requests);
    let _subscription = context.update(|window, application| {
        window.focus(
            &view.read(application).focus_handle(application),
            application,
        );
        application.subscribe(&view, move |_view, event: &GridScroll, _application| {
            received
                .borrow_mut()
                .push((event.key.clone(), event.scroll));
        })
    });
    context.update(|window, application| window.draw(application).clear(application));
    context.simulate_keystrokes("pageup pagedown home end");
    assert!(
        requests.borrow().is_empty(),
        "unmodified keys belong to the terminal"
    );
    context.simulate_keystrokes("shift-pageup shift-pagedown shift-home shift-end");
    let extent = isize::try_from(DAMAGE_ROWS)?;
    assert_eq!(
        *requests.borrow(),
        vec![
            (key(), ScrollViewport::Delta(extent.saturating_neg())),
            (key(), ScrollViewport::Delta(extent)),
            (key(), ScrollViewport::Top),
            (key(), ScrollViewport::Bottom),
        ],
        "shift navigation must emit the matching history operation"
    );
    requests.borrow_mut().clear();
    let inputs = Rc::new(RefCell::new(Vec::new()));
    let received_input = Rc::clone(&inputs);
    let _input_subscription = context.update(|_, application| {
        application.subscribe(&view, move |_, event: &GridInput, _| {
            received_input.borrow_mut().push(event.clone());
        })
    });
    let position =
        context.update(|window, _application| window.find("terminal-grid").bounds().center());
    let half = half_line();
    let event = ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.0), half)),
        ..ScrollWheelEvent::default()
    };
    context.simulate_event(event.clone());
    assert!(
        requests.borrow().is_empty(),
        "partial row remains accumulated"
    );
    context.simulate_event(event);
    context.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0.0, -1.0)),
        ..ScrollWheelEvent::default()
    });
    for input in inputs.borrow_mut().drain(..) {
        thread.send(VtCommand::Input {
            key: input.key,
            input: input.input,
        })?;
        let Some(iznik_app::vt::VtOutput::LocalPointer(fallback)) =
            support::receive(&thread).result?
        else {
            return Err("wheel must return to local history".into());
        };
        view.update(context, |grid, context| {
            grid.apply_pointer(&fallback, context)
        })?;
    }
    assert_eq!(
        *requests.borrow(),
        vec![
            (key(), ScrollViewport::Delta(-1)),
            (key(), ScrollViewport::Delta(1))
        ],
        "pixel and line wheel deltas must preserve their direction"
    );
    view.read_with(context, |grid, _application| {
        assert_eq!(
            grid.snapshot().map(|snapshot| snapshot.sequence),
            Some(Sequence(0)),
            "input requests cannot advance the displayed stream sequence"
        );
    });
    Ok(())
}

/// Half a row tests accumulation independently of the configured row height.
fn half_line() -> gpui_kit::Pixels {
    /// Two partial events must add up to one complete row.
    const PARTS: f32 = 2.0;
    px(f32::from(GridMetrics::default().line_height) / PARTS)
}

#[gpui_kit::test]
fn grid_paints_decoration_colors_and_cell_geometry(context: &mut TestAppContext) {
    check(&decorations(context));
}

/// Inspect submitted GPUI quads, including explicit palette decoration colors.
///
/// # Errors
/// Returns terminal, grid or window failures.
///
/// # Panics
/// Fails if decoration styles collapse together or paint outside their cells.
fn decorations(context: &mut TestAppContext) -> Result<(), Failed> {
    /// Distinct decoration color, shared by direct RGB and palette references.
    const DECORATION_COLOR: u32 = 0x0001_0203;
    /// Expected quads for single, double, dotted, dashed and overline cells.
    const EXPECTED: [(f32, f32, f32, f32); 7] = [
        (0.0, 17.0, 8.0, 1.0),
        (8.0, 17.0, 8.0, 1.0),
        (8.0, 15.0, 8.0, 1.0),
        (16.0, 17.0, 1.0, 1.0),
        (20.0, 17.0, 1.0, 1.0),
        (24.0, 17.0, 6.0, 1.0),
        (32.0, 0.0, 8.0, 1.0),
    ];
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    thread.send(VtCommand::Feed {
                receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"\x1b]4;1;rgb:01/02/03\x1b\\\x1b[58;2;1;2;3m\x1b[4:1mA\x1b[4:2mB\x1b[58;5;1m\x1b[4:4mC\x1b[4:5mD\x1b[24;53;38;2;1;2;3mE\x1b[0m".to_vec(),
    })?;
    let current = snapshot(&thread)?;
    handle.update(context, |grid, _window, context| {
        grid.apply(current, context)
    })??;
    draw(context, handle)?;
    handle.update(context, |grid, window, application| {
        let observed = painted_bounds(window, gpui_kit::rgb(DECORATION_COLOR).into());
        assert_eq!(
            observed.len(),
            EXPECTED.len(),
            "each decoration has its own geometry: {observed:?}"
        );
        for expected in EXPECTED {
            assert!(
                observed.contains(&expected),
                "missing painted decoration {expected:?}: {observed:?}"
            );
        }
        assert!(
            grid.paint_errors(application).is_empty(),
            "GPUI text shaping must succeed"
        );
    })?;
    Ok(())
}

/// Convert submitted scene geometry back to coordinates within the terminal.
fn painted_bounds(window: &gpui_kit::Window, color: gpui_kit::Hsla) -> Vec<(f32, f32, f32, f32)> {
    let origin = window.find("terminal-grid").bounds().origin;
    let scale = window.scale_factor();
    window
        .painted_quads()
        .into_iter()
        .filter(|rectangle| rectangle.background.as_solid() == Some(color))
        .map(|rectangle| {
            (
                rectangle.bounds.origin.x.0 / scale - f32::from(origin.x),
                rectangle.bounds.origin.y.0 / scale - f32::from(origin.y),
                rectangle.bounds.size.width.0 / scale,
                rectangle.bounds.size.height.0 / scale,
            )
        })
        .collect()
}

#[gpui_kit::test]
fn grid_paints_selection_cursor_and_inverse_background(context: &mut TestAppContext) {
    check(&overlays(context));
}

/// Assert actual selection, cursor and inverse-background placement in the scene.
///
/// # Errors
/// Returns terminal, grid or window failures.
///
/// # Panics
/// Fails if overlays are shifted or inverse cells use the wrong background.
fn overlays(context: &mut TestAppContext) -> Result<(), Failed> {
    /// Distinct direct foreground becomes the inverse cell background.
    const INVERSE_COLOR: u32 = 0x0001_0203;
    /// Explicit OSC cursor color isolates the cursor quad from text and fills.
    const CURSOR_COLOR: u32 = 0x00a1_b2c3;
    /// The committed selection tint is translucent blue.
    const SELECTION_COLOR: u32 = 0x004c_7fbf;
    /// Selection opacity preserves legibility of underlying cell backgrounds.
    const SELECTION_ALPHA: f32 = 0.4;
    /// Selection spans the middle two columns of the initial row.
    const SELECTION: GridSelection = GridSelection {
        anchor: GridPosition { row: 0, column: 1 },
        head: GridPosition { row: 0, column: 3 },
    };
    /// Expected rectangle for two selected cells at the default metrics.
    const SELECTED: (f32, f32, f32, f32) = (8.0, 0.0, 16.0, 18.0);
    /// Bar cursor after the one printed cell occupies one logical pixel.
    const CURSOR: (f32, f32, f32, f32) = (8.0, 0.0, 1.0, 18.0);
    /// Inverse rendition occupies exactly the printed cell.
    const INVERSE: (f32, f32, f32, f32) = (0.0, 0.0, 8.0, 18.0);
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"\x1b]12;rgb:a1/b2/c3\x1b\\\x1b[6 q\x1b[7;38;2;1;2;3mA\x1b[0m".to_vec(),
    })?;
    let current = snapshot(&thread)?;
    handle.update(context, |grid, _window, context| {
        grid.apply(current, context)?;
        grid.select(Some(SELECTION), context)
    })??;
    draw(context, handle)?;
    handle.update(context, |_grid, window, _application| {
        let mut selected: gpui_kit::Hsla = gpui_kit::rgb(SELECTION_COLOR).into();
        selected.a = SELECTION_ALPHA;
        assert_eq!(
            painted_bounds(window, selected),
            vec![SELECTED],
            "selection rectangle"
        );
        assert_eq!(
            painted_bounds(window, gpui_kit::rgb(CURSOR_COLOR).into()),
            vec![CURSOR],
            "cursor rectangle"
        );
        assert_eq!(
            painted_bounds(window, gpui_kit::rgb(INVERSE_COLOR).into()),
            vec![INVERSE],
            "inverse background rectangle"
        );
    })?;
    Ok(())
}

#[gpui_kit::test]
fn grid_credit_counts_consumption_once_and_retries_failed_submission(context: &mut TestAppContext) {
    check(&credit(context));
}

/// Compare accepted byte grants with actual VT batches and local-only redraws.
///
/// # Errors
/// Returns terminal, grid or window errors.
///
/// # Panics
/// Fails if snapshots or selection mint duplicate credit or submission errors lose it.
fn credit(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let screen = open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    handle.update(context, |grid, _window, context| {
        grid.apply(screen, context)
    })??;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"abcd".to_vec(),
    })?;
    let first = snapshot(&thread)?;
    let first_bytes = first.consumed_bytes;
    let mut granted = Vec::new();
    handle.update(context, |grid, _window, context| -> Result<(), Failed> {
        grid.flush_credit(|pane, bytes| {
            granted.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })?;
        assert!(granted.is_empty(), "unconsumed output cannot return credit");
        grid.apply(first.clone(), context)?;
        let error = grid.flush_credit(|_pane, _bytes| Err::<(), _>("submission unavailable"));
        assert!(error.is_err(), "the submission failure must be returned");
        grid.apply(first.clone(), context)?;
        grid.select(
            Some(GridSelection {
                anchor: GridPosition { row: 0, column: 0 },
                head: GridPosition { row: 0, column: 1 },
            }),
            context,
        )?;
        grid.flush_credit(|pane, bytes| {
            granted.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })?;
        grid.flush_credit(|pane, bytes| {
            granted.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })?;
        assert_eq!(
            granted,
            vec![(key(), first_bytes)],
            "retry grants each accepted byte exactly once"
        );
        Ok(())
    })??;
    thread.send(VtCommand::Snapshot(key()))?;
    let idle = snapshot(&thread)?;
    handle.update(context, |grid, _window, context| -> Result<(), Failed> {
        grid.apply(idle, context)?;
        grid.flush_credit(|pane, bytes| {
            granted.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })?;
        assert_eq!(
            granted,
            vec![(key(), first_bytes)],
            "idle snapshots carry no credit"
        );
        Ok(())
    })??;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: first.sequence,
        bytes: b"more".to_vec(),
    })?;
    let next = snapshot(&thread)?;
    let next_bytes = next.consumed_bytes;
    handle.update(context, |grid, _window, context| -> Result<(), Failed> {
        grid.apply(next, context)?;
        assert_eq!(
            grid.apply(first, context),
            Err(iznik_app::grid::GridError::Credit),
            "stale frames cannot repeat grants"
        );
        grid.flush_credit(|pane, bytes| {
            granted.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })?;
        assert_eq!(
            granted,
            vec![(key(), first_bytes), (key(), next_bytes)],
            "new output earns only its own bytes"
        );
        Ok(())
    })??;
    Ok(())
}

#[gpui_kit::test]
fn grid_credit_is_independent_for_each_consuming_pane(context: &mut TestAppContext) {
    check(&independent_credit(context));
}

/// Hold one pane's snapshot while the other consumes and grants its own bytes.
///
/// # Errors
/// Returns terminal, grid or window errors.
///
/// # Panics
/// Fails if one pane grants bytes belonging to an unconsumed sibling.
fn independent_credit(context: &mut TestAppContext) -> Result<(), Failed> {
    use iznik_app::vt::TerminalTheme;
    use iznik_protocol::identity::PaneId;
    /// A different pane on the same host has an independent credit window.
    const SECOND_PANE: PaneId = PaneId(2);
    context.update(gpui_kit::init);
    let first =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let second =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let initial = open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    first.update(context, |grid, _window, context| {
        grid.apply(initial, context)
    })??;
    let mut other = key();
    other.pane = SECOND_PANE;
    thread.send(VtCommand::Screen {
        key: other.clone(),
        sequence: Sequence(0),
        columns: DAMAGE_COLUMNS,
        rows: DAMAGE_ROWS,
        bytes: Vec::new(),
        theme: Box::new(TerminalTheme::default()),
    })?;
    let other_initial = snapshot(&thread)?;
    second.update(context, |grid, _window, context| {
        grid.apply(other_initial, context)
    })??;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"held".to_vec(),
    })?;
    let held = snapshot(&thread)?;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: other.clone(),
        sequence: Sequence(0),
        bytes: b"flowing".to_vec(),
    })?;
    let flowing = snapshot(&thread)?;
    let expected = flowing.consumed_bytes;
    let mut grants = Vec::new();
    first.update(context, |grid, _window, _context| {
        grid.flush_credit(|pane, bytes| {
            grants.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })
    })??;
    second.update(context, |grid, _window, context| -> Result<(), Failed> {
        grid.apply(flowing, context)?;
        grid.flush_credit(|pane, bytes| {
            grants.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })
    })??;
    assert_eq!(
        grants,
        vec![(other, expected)],
        "only the consumed pane refills its credit window"
    );
    let held_bytes = held.consumed_bytes;
    first.update(context, |grid, _window, context| -> Result<(), Failed> {
        grid.apply(held, context)?;
        grid.flush_credit(|pane, bytes| {
            grants.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })
    })??;
    assert_eq!(
        grants.last(),
        Some(&(key(), held_bytes)),
        "the held pane grants its own bytes on consumption"
    );
    Ok(())
}

#[gpui_kit::test]
fn grid_credit_rebases_on_a_screen_without_crediting_screen_bytes(context: &mut TestAppContext) {
    check(&credit_reset(context));
}

/// A fresh screen rebases sequence comparisons while preserving already owed credit.
///
/// # Errors
/// Returns terminal, grid or window failures.
///
/// # Panics
/// Fails if screen bytes earn credit or resynchronization loses a pending grant.
fn credit_reset(context: &mut TestAppContext) -> Result<(), Failed> {
    context.update(gpui_kit::init);
    let handle =
        context.add_window(|_window, context| TerminalGrid::new(GridMetrics::default(), context));
    let thread = VtThread::start(VtOptions::default())?;
    let initial = open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    handle.update(context, |grid, _window, context| {
        grid.apply(initial, context)
    })??;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"old".to_vec(),
    })?;
    let first = snapshot(&thread)?;
    let first_bytes = first.consumed_bytes;
    handle.update(context, |grid, _window, context| grid.apply(first, context))??;
    let reset = open(&thread, Sequence(0), DAMAGE_COLUMNS, DAMAGE_ROWS)?;
    assert!(
        reset.reset,
        "only an authoritative screen resets the sequence baseline"
    );
    assert_eq!(
        reset.consumed_bytes, 0,
        "screen replay is not stream credit"
    );
    handle.update(context, |grid, _window, context| grid.apply(reset, context))??;
    thread.send(VtCommand::Feed {
        receipt: None,
        key: key(),
        sequence: Sequence(0),
        bytes: b"new".to_vec(),
    })?;
    let next = snapshot(&thread)?;
    let total = first_bytes
        .checked_add(next.consumed_bytes)
        .ok_or("fixture credit overflow")?;
    assert!(!next.reset, "ordinary output never resets the baseline");
    let mut grants = Vec::new();
    handle.update(context, |grid, _window, context| -> Result<(), Failed> {
        grid.apply(next, context)?;
        grid.flush_credit(|pane, bytes| {
            grants.push((pane.clone(), bytes));
            Ok::<(), Failed>(())
        })?;
        assert_eq!(
            grants,
            vec![(key(), total)],
            "resynchronization preserves only consumed-byte grants"
        );
        Ok(())
    })??;
    Ok(())
}
