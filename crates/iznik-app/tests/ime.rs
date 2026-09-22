//! Platform composition state, candidate geometry and native encoder routing.

mod support;

use std::cell::RefCell;
use std::rc::Rc;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    App, AppContext, Bounds, ClipboardItem, ElementInputHandler, Focusable, InputHandler, Pixels,
    Subscription, TestAppContext, Window, WindowHandle, px,
};
use iznik_app::grid::{GridInput, GridMetrics, TerminalGrid};
use iznik_app::input::TerminalInput;
use iznik_app::vt::{VtCommand, VtOptions, VtOutput, VtThread};
use iznik_protocol::identity::Sequence;

/// Failure propagated from fixture setup, GPUI or the VT owner.
type Failed = Box<dyn std::error::Error>;
/// Fixture width leaves enough room to inspect composition at a nonzero column.
const COLUMNS: u16 = 40;
/// Fixture height includes a nonzero cursor row.
const ROWS: u16 = 4;
/// UTF-16 endpoint after a letter and a supplementary-plane character.
const AFTER_EMOJI: usize = 3;
/// Complete UTF-16 length of the letter, emoji and trailing letter fixture.
const DRAFT_UNITS: usize = 4;
/// UTF-16 offset inside the emoji's surrogate pair.
const INSIDE_EMOJI: usize = 2;
/// Initial cursor is in the third column after the two printable characters.
const CURSOR_COLUMN: f32 = 2.0;
/// One logical pixel is the minimum width of an empty candidate anchor.
const CARET_WIDTH: f32 = 1.0;

/// Surface, native VT owner and observed UI requests share one fixture lifetime.
struct Fixture {
    /// Real headless GPUI window with a focused terminal grid.
    handle: WindowHandle<TerminalGrid>,
    /// Dedicated owner of the terminal and its native encoders.
    thread: VtThread,
    /// Owned requests emitted by the surface.
    requests: Rc<RefCell<Vec<GridInput>>>,
    /// Keep the framework event subscription alive until the fixture drops.
    _subscription: Subscription,
}

impl Fixture {
    /// Open a grid with an extended-keyboard terminal and a nonzero cursor origin.
    ///
    /// # Errors
    /// Propagates emulator, window and snapshot failures.
    fn new(context: &mut TestAppContext) -> Result<Self, Failed> {
        context.update(gpui_kit::init);
        let thread = VtThread::start(VtOptions::default())?;
        let bytes = b"\x1b[>11u\x1b[?2004h\r\nab";
        support::open(&thread, Sequence(0), COLUMNS, ROWS)?;
        thread.send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: bytes.to_vec(),
        })?;
        let frame = support::snapshot(&thread)?;
        let handle =
            context.add_window(|_, context| TerminalGrid::new(GridMetrics::default(), context));
        let requests = Rc::new(RefCell::new(Vec::new()));
        let subscription = handle.update(context, |grid, window, context| {
            grid.apply(frame, context)?;
            window.focus(&grid.focus_handle(context), context);
            let requests = Rc::clone(&requests);
            Ok::<_, Failed>(context.subscribe(
                &context.entity(),
                move |_, _, event: &GridInput, _| {
                    requests.borrow_mut().push(event.clone());
                },
            ))
        })??;
        let fixture = Self {
            handle,
            thread,
            requests,
            _subscription: subscription,
        };
        fixture.draw(context)?;
        Ok(fixture)
    }

    /// Complete a paint so the platform receives the focused input handler.
    ///
    /// # Errors
    /// Returns a closed-window failure.
    fn draw(&self, context: &mut TestAppContext) -> Result<(), Failed> {
        context.update_window(self.handle.into(), |_, window, application| {
            window.draw(application).clear(application);
        })?;
        Ok(())
    }

    /// Exercise GPUI's canonical platform adapter against the actual laid-out grid.
    ///
    /// # Errors
    /// Returns a closed-window failure.
    fn handler<ResultValue>(
        &self,
        context: &mut TestAppContext,
        operation: impl FnOnce(
            &mut ElementInputHandler<TerminalGrid>,
            Bounds<Pixels>,
            &mut Window,
            &mut App,
        ) -> ResultValue,
    ) -> Result<ResultValue, Failed> {
        context.update_window(self.handle.into(), |view, window, application| {
            let bounds = window.find("terminal-grid").bounds();
            let entity = view
                .downcast::<TerminalGrid>()
                .map_err(|_entity| "grid entity")?;
            let mut handler = ElementInputHandler::new(bounds, entity);
            Ok::<_, Failed>(operation(&mut handler, bounds, window, application))
        })?
    }

    /// Change terminal modes through the real owner and consume the resulting frame.
    ///
    /// # Errors
    /// Propagates window, thread and snapshot failures.
    fn modes(&self, context: &mut TestAppContext, bytes: &[u8]) -> Result<(), Failed> {
        let sequence = self.handle.update(context, |grid, _, _| {
            grid.snapshot()
                .map(|snapshot| snapshot.sequence)
                .ok_or("missing snapshot")
        })??;
        self.thread.send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence,
            bytes: bytes.to_vec(),
        })?;
        let snapshot = support::snapshot(&self.thread)?;
        self.handle
            .update(context, |grid, _, context| grid.apply(snapshot, context))??;
        self.draw(context)
    }

    /// Submit each queued UI request through the real owning thread.
    ///
    /// # Errors
    /// Propagates service failures or a missing reply.
    fn replies(&self) -> Result<Vec<VtOutput>, Failed> {
        let requests: Vec<_> = self.requests.borrow_mut().drain(..).collect();
        let mut replies = Vec::new();
        for request in requests {
            self.thread.send(VtCommand::Input {
                key: request.key,
                input: request.input,
            })?;
            replies.push(
                support::receive(&self.thread)
                    .result?
                    .ok_or("missing input reply")?,
            );
        }
        Ok(replies)
    }

    /// Drain emitted keyboard requests through the native VT input service.
    ///
    /// # Errors
    /// Propagates service failures or an unexpected reply type.
    fn encoded(&self) -> Result<Vec<u8>, Failed> {
        let mut encoded = Vec::new();
        for reply in self.replies()? {
            let VtOutput::Input(bytes) = reply else {
                return Err("expected encoded input".into());
            };
            encoded.extend(bytes);
        }
        Ok(encoded)
    }

    /// Route local pointer fallback exactly as the window will, retaining native bytes.
    ///
    /// # Errors
    /// Propagates service, window or selection failures.
    fn pointers(&self, context: &mut TestAppContext) -> Result<Vec<u8>, Failed> {
        let mut encoded = Vec::new();
        for reply in self.replies()? {
            match reply {
                VtOutput::Input(bytes) => encoded.extend(bytes),
                VtOutput::LocalPointer(input) => {
                    self.handle.update(context, |grid, _, context| {
                        grid.apply_pointer(&input, context)
                    })??;
                }
                _ => return Err("unexpected pointer reply".into()),
            }
        }
        Ok(encoded)
    }

    /// Ask the native serializer to copy the grid's actual local selection.
    ///
    /// # Errors
    /// Propagates service/window failures or missing clipboard text.
    fn copied(&self, context: &mut TestAppContext) -> Result<String, Failed> {
        self.handle
            .update(context, |grid, _, context| grid.copy_selection(context))?;
        let mut replies = self.replies()?.into_iter();
        match replies.next() {
            Some(VtOutput::Clipboard(text)) => Ok(text),
            None => Ok(String::new()),
            _ => Err("copy did not produce clipboard text".into()),
        }
    }

    /// Center of a displayed cell using the grid's actual surface origin.
    ///
    /// # Errors
    /// Propagates a closed-window failure.
    fn cell(
        &self,
        context: &mut TestAppContext,
        column: u16,
        row: u16,
    ) -> Result<gpui_kit::Point<Pixels>, Failed> {
        self.handler(context, |_, bounds, _, _| {
            let metrics = GridMetrics::default();
            gpui_kit::point(
                px(f32::from(bounds.origin.x)
                    + f32::from(metrics.cell_width) * (f32::from(column) + CELL_CENTER)),
                px(f32::from(bounds.origin.y)
                    + f32::from(metrics.line_height) * (f32::from(row) + CELL_CENTER)),
            )
        })
    }
}

/// Surface test failures remain visible outside the GPUI test macro.
///
/// # Panics
/// Fails with the underlying fixture error.
fn check(result: &Result<(), Failed>) {
    assert!(result.is_ok(), "{result:?}");
}

#[gpui_kit::test]
fn ime_edits_utf16_without_sending_preedit(context: &mut TestAppContext) {
    check(&edit_composition(context));
}

/// Exercise surrogate boundaries, partial replacements, commit and cancellation.
///
/// # Errors
/// Propagates fixture failures.
///
/// # Panics
/// Fails when editable ranges leak output, draft text is sent, or commit bytes differ.
fn edit_composition(context: &mut TestAppContext) -> Result<(), Failed> {
    let fixture = Fixture::new(context)?;
    fixture.handler(context, |handler, _, window, application| {
        handler.replace_and_mark_text_in_range(
            None,
            "a\u{1f600}b",
            Some(AFTER_EMOJI..AFTER_EMOJI),
            window,
            application,
        );
        let mut adjusted = None;
        assert_eq!(
            handler.text_for_range(
                INSIDE_EMOJI..AFTER_EMOJI,
                &mut adjusted,
                window,
                application
            ),
            Some("\u{1f600}".into()),
            "range begins at the whole surrogate pair"
        );
        assert_eq!(adjusted, Some(1..AFTER_EMOJI), "adjusted UTF-16 boundaries");
        assert_eq!(
            handler.text_input_editable_range(window, application),
            Some(0..DRAFT_UNITS),
            "only the unsent draft is editable"
        );
        handler.replace_and_mark_text_in_range(
            Some(1..AFTER_EMOJI),
            "\u{6f22}",
            Some(1..1),
            window,
            application,
        );
        assert_eq!(
            handler
                .selected_text_range(false, window, application)
                .map(|selection| selection.range),
            Some(INSIDE_EMOJI..INSIDE_EMOJI),
            "partial replacement offsets its selection"
        );
    })?;
    assert!(
        fixture.requests.borrow().is_empty(),
        "preedit must not reach the process"
    );
    fixture.handler(context, |handler, _, window, application| {
        handler.unmark_text(window, application);
        handler.unmark_text(window, application);
        assert_eq!(
            handler.marked_text_range(window, application),
            None,
            "commit clears the mark"
        );
        assert_eq!(
            handler.text_length_utf16(window, application),
            Some(0),
            "committed text is no longer editable"
        );
    })?;
    assert_eq!(
        fixture.encoded()?,
        "a\u{6f22}b".as_bytes(),
        "unmark commits once"
    );
    fixture.handler(context, |handler, _, window, application| {
        handler.replace_and_mark_text_in_range(None, "cancel", None, window, application);
        handler.replace_and_mark_text_in_range(None, "", None, window, application);
        handler.unmark_text(window, application);
    })?;
    assert!(
        fixture.encoded()?.is_empty(),
        "empty marked replacement cancels"
    );
    Ok(())
}

#[gpui_kit::test]
fn ime_candidate_geometry_uses_the_painted_cursor(context: &mut TestAppContext) {
    check(&candidate_geometry(context));
}

/// Candidate and point queries share shaped advances and the emulator cursor origin.
///
/// # Errors
/// Propagates fixture failures.
///
/// # Panics
/// Fails when candidate bounds or hit testing disagree with the rendered composition.
fn candidate_geometry(context: &mut TestAppContext) -> Result<(), Failed> {
    let fixture = Fixture::new(context)?;
    fixture.handler(context, |handler, _, window, application| {
        handler.replace_and_mark_text_in_range(
            None,
            "a\u{1f600}b",
            Some(1..1),
            window,
            application,
        );
    })?;
    fixture.draw(context)?;
    fixture.handler(context, |handler, surface, window, application| {
        let metrics = GridMetrics::default();
        let origin = handler
            .bounds_for_range(0..0, window, application)
            .ok_or("candidate bounds missing")?;
        assert_eq!(
            origin.origin.x,
            px(f32::from(surface.origin.x) + f32::from(metrics.cell_width) * CURSOR_COLUMN),
            "candidate column follows the cursor"
        );
        assert_eq!(
            origin.origin.y,
            px(f32::from(surface.origin.y) + f32::from(metrics.line_height)),
            "candidate row follows the cursor"
        );
        assert_eq!(
            origin.size.width,
            px(CARET_WIDTH),
            "empty range has a caret width"
        );
        assert_eq!(
            origin.size.height, metrics.line_height,
            "candidate uses row height"
        );
        let end = handler
            .bounds_for_range(AFTER_EMOJI..AFTER_EMOJI, window, application)
            .ok_or("end bounds missing")?;
        assert!(
            end.origin.x > origin.origin.x,
            "shaped draft advances the candidate"
        );
        assert_eq!(
            handler.character_index_for_point(end.origin, window, application),
            Some(AFTER_EMOJI),
            "point mapping returns UTF-16 rather than UTF-8 offsets"
        );
        let normalized = handler
            .bounds_for_range(INSIDE_EMOJI..AFTER_EMOJI, window, application)
            .ok_or("normalized bounds missing")?;
        assert!(
            normalized.size.width > px(CARET_WIDTH),
            "surrogate range covers the whole glyph"
        );
        Ok::<_, Failed>(())
    })??;
    Ok(())
}

#[gpui_kit::test]
fn ime_registration_dispatches_text_and_paste_through_native_modes(context: &mut TestAppContext) {
    check(&dispatch_input(context));
}

/// Framework fallback dispatch reaches the registered handler; paste retains its kind.
///
/// # Errors
/// Propagates fixture failures.
///
/// # Panics
/// Fails when platform text is duplicated, lost, or pasted without native framing.
fn dispatch_input(context: &mut TestAppContext) -> Result<(), Failed> {
    let fixture = Fixture::new(context)?;
    context.simulate_input(fixture.handle.into(), "\u{e9}");
    assert_eq!(
        fixture.encoded()?,
        "\u{e9}".as_bytes(),
        "registered platform text path"
    );
    fixture.handler(context, |handler, _, window, application| {
        handler.replace_and_mark_text_in_range(None, "draft", None, window, application);
        handler.replace_text_in_range(None, "\u{6f22}\u{5b57}", window, application);
        handler.paste(
            ClipboardItem::new_string("one\ntwo".into()),
            window,
            application,
        );
    })?;
    assert!(
        fixture
            .requests
            .borrow()
            .iter()
            .any(|request| matches!(request.input, TerminalInput::Paste(_))),
        "paste preserves its encoder route"
    );
    assert_eq!(
        fixture.encoded()?,
        "\u{6f22}\u{5b57}\x1b[200~one\ntwo\x1b[201~".as_bytes(),
        "committed Unicode and framed paste"
    );
    Ok(())
}

/// Keyboard fixtures pin framework names to native bytes in live terminal modes.
const KEY_CASES: &[(&[u8], &str, &[u8])] = &[
    (b"\x1b[<u\x1b[?1l\x1b[>4;0m", "up", b"\x1b[A"),
    (b"\x1b[?1h", "up", b"\x1bOA"),
    (b"\x1b[?1l", "ctrl-c", b"\x03"),
    (b"", "ctrl-shift-h", b"\x1b[104;6u"),
    (b"\x1b[>4;2m", "ctrl-shift-h", b"\x1b[27;6;72~"),
    (b"\x1b[>4;0m", "enter", b"\r"),
    (b"", "tab", b"\t"),
    (b"", "shift-tab", b"\x1b[Z"),
    (b"", "f5", b"\x1b[15~"),
    (b"", "ctrl-shift-pageup", b"\x1b[5;6~"),
];

#[gpui_kit::test]
fn keyboard_dispatch_uses_live_terminal_modes(context: &mut TestAppContext) {
    check(&keyboard_modes(context));
}

/// Dispatch each chord through GPUI before the native owner chooses its encoding.
///
/// # Errors
/// Propagates fixture and encoding failures.
///
/// # Panics
/// Fails when framework event mapping changes the expected terminal bytes.
fn keyboard_modes(context: &mut TestAppContext) -> Result<(), Failed> {
    let fixture = Fixture::new(context)?;
    for (modes, chord, expected) in KEY_CASES {
        fixture.modes(context, modes)?;
        context.simulate_keystrokes(fixture.handle.into(), chord);
        assert_eq!(fixture.encoded()?, *expected, "framework chord {chord}");
    }
    Ok(())
}

#[gpui_kit::test]
fn keyboard_dispatch_pairs_repeat_and_release(context: &mut TestAppContext) {
    check(&keyboard_events(context));
}

/// Preserve press/repeat/release while suppressing releases of local history chords.
///
/// # Errors
/// Propagates fixture and keystroke parsing failures.
///
/// # Panics
/// Fails when an extended keyboard event is lost or a local shortcut leaks bytes.
fn keyboard_events(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{KeyDownEvent, KeyUpEvent, Keystroke, VisualTestContext};
    let fixture = Fixture::new(context)?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    let stroke = Keystroke::parse("backspace")?;
    for is_held in [false, true] {
        visual.simulate_event(KeyDownEvent {
            keystroke: stroke.clone(),
            is_held,
            prefer_character_input: false,
        });
    }
    visual.simulate_event(KeyUpEvent { keystroke: stroke });
    assert_eq!(
        fixture.encoded()?,
        b"\x1b[127u\x1b[127;1:2u\x1b[127;1:3u",
        "all native event types"
    );
    let history = Keystroke::parse("shift-pageup")?;
    visual.simulate_event(KeyDownEvent {
        keystroke: history,
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("pageup")?,
    });
    assert!(
        fixture.encoded()?.is_empty(),
        "history stays local even if Shift is released first"
    );
    Ok(())
}

#[gpui_kit::test]
fn keyboard_dispatch_leaves_composition_to_the_platform(context: &mut TestAppContext) {
    check(&keyboard_composition(context));
}

/// Marked drafts and explicit character preference bypass raw terminal key dispatch.
///
/// # Errors
/// Propagates fixture and keystroke parsing failures.
///
/// # Panics
/// Fails when raw presses or releases escape a composition session.
fn keyboard_composition(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{KeyDownEvent, KeyUpEvent, Keystroke, VisualTestContext};
    let fixture = Fixture::new(context)?;
    fixture.handler(context, |handler, _, window, application| {
        handler.replace_and_mark_text_in_range(None, "draft", None, window, application);
    })?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    visual.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("up")?,
        is_held: false,
        prefer_character_input: false,
    });
    visual.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("up")?,
    });
    assert!(
        fixture.requests.borrow().is_empty(),
        "composition retains arrow keys"
    );
    fixture.handler(context, |handler, _, window, application| {
        handler.replace_and_mark_text_in_range(None, "", None, window, application);
    })?;
    visual.simulate_event(KeyDownEvent {
        keystroke: Keystroke::parse("up")?,
        is_held: false,
        prefer_character_input: true,
    });
    visual.simulate_event(KeyUpEvent {
        keystroke: Keystroke::parse("up")?,
    });
    assert!(
        fixture.requests.borrow().is_empty(),
        "character-preferred events bypass raw key encoding"
    );
    Ok(())
}

/// Half a cell places fixture pointers away from boundary rounding decisions.
const CELL_CENTER: f32 = 0.5;
/// Third cell ends the forward selection after its third grapheme.
const SELECTION_END: u16 = 2;
/// Fourth cell starts the reversed selection.
const REVERSED_START: u16 = 3;

#[gpui_kit::test]
fn pointer_dispatch_selects_and_copies_in_both_directions(context: &mut TestAppContext) {
    check(&pointer_selection(context));
}

/// Selection fallback runs only after the native owner declines mouse tracking.
///
/// # Errors
/// Propagates fixture, pointer and native copy failures.
///
/// # Panics
/// Fails when local drag direction changes the selected graphemes or emits process input.
fn pointer_selection(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{Modifiers, MouseButton, VisualTestContext};
    let fixture = Fixture::new(context)?;
    fixture.modes(context, b"\x1b[2J\x1b[Habcde")?;
    let start = fixture.cell(context, 0, 0)?;
    let end = fixture.cell(context, SELECTION_END, 0)?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    assert!(
        fixture.pointers(context)?.is_empty(),
        "untracked selection sends no bytes"
    );
    assert_eq!(
        fixture.copied(context)?,
        "abc",
        "forward half-open selection"
    );
    let reverse_start = fixture.cell(context, REVERSED_START, 0)?;
    let reverse_end = fixture.cell(context, 1, 0)?;
    visual.simulate_mouse_down(reverse_start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(reverse_end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(reverse_end, MouseButton::Left, Modifiers::default());
    fixture.pointers(context)?;
    assert_eq!(
        fixture.copied(context)?,
        "bcd",
        "reversed half-open selection"
    );
    visual.simulate_click(reverse_end, Modifiers::default());
    fixture.pointers(context)?;
    assert!(
        fixture.copied(context)?.is_empty(),
        "a click clears the selection"
    );
    Ok(())
}

#[gpui_kit::test]
fn pointer_dispatch_uses_live_tracking_and_never_reinterprets_filtered_events(
    context: &mut TestAppContext,
) {
    check(&pointer_tracking(context));
}

/// Tracking is decided on the owner even before the mode snapshot reaches the grid.
///
/// # Errors
/// Propagates fixture, pointer and native encoding failures.
///
/// # Panics
/// Fails when a tracked event becomes a local selection or uses stale mode state.
fn pointer_tracking(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{Modifiers, MouseButton, VisualTestContext};
    let fixture = Fixture::new(context)?;
    let sequence = fixture
        .handle
        .update(context, |grid, _, _| {
            grid.snapshot().map(|snapshot| snapshot.sequence)
        })?
        .ok_or("snapshot")?;
    fixture.thread.send(VtCommand::Feed {
        receipt: None,
        key: support::key(),
        sequence,
        bytes: b"\x1b[?1002h\x1b[?1006h".to_vec(),
    })?;
    let held_snapshot = support::snapshot(&fixture.thread)?;
    let start = fixture.cell(context, 0, 0)?;
    let end = fixture.cell(context, SELECTION_END, 0)?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    assert_eq!(
        fixture.pointers(context)?,
        b"\x1b[<0;1;1M\x1b[<32;3;1M\x1b[<0;3;1m",
        "live SGR drag encoding"
    );
    assert!(
        fixture.copied(context)?.is_empty(),
        "program tracking does not select cells"
    );
    fixture.handle.update(context, |grid, _, context| {
        grid.apply(held_snapshot, context)
    })??;
    fixture.modes(context, b"\x1b[?9h")?;
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    assert_eq!(
        fixture.pointers(context)?,
        b"\x1b[<0;1;1M",
        "X10 consumes its filtered drag and release"
    );
    assert!(
        fixture.copied(context)?.is_empty(),
        "filtered program events have no local fallback"
    );
    Ok(())
}

#[gpui_kit::test]
fn pointer_dispatch_retains_shift_override_and_rejects_stale_fallback(
    context: &mut TestAppContext,
) {
    check(&pointer_override(context));
}

/// Explicit Shift drag bypasses tracking, while delayed local replies cannot select new output.
///
/// # Errors
/// Propagates fixture, pointer and copy failures.
///
/// # Panics
/// Fails when Shift release changes drag ownership or stale fallback selects new cells.
fn pointer_override(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{Modifiers, MouseButton, VisualTestContext};
    let fixture = Fixture::new(context)?;
    fixture.modes(context, b"\x1b[2J\x1b[Habcde\x1b[?1002h\x1b[?1006h")?;
    let start = fixture.cell(context, 0, 0)?;
    let end = fixture.cell(context, SELECTION_END, 0)?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    visual.simulate_mouse_down(
        start,
        MouseButton::Left,
        Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    );
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    assert!(
        fixture.requests.borrow().is_empty(),
        "Shift override stays local through release"
    );
    assert_eq!(
        fixture.copied(context)?,
        "abc",
        "Shift selects under program tracking"
    );
    fixture.modes(context, b"\x1b[?1002l")?;
    visual.simulate_click(end, Modifiers::default());
    fixture.pointers(context)?;
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    fixture.modes(context, b"\rchanged")?;
    fixture.pointers(context)?;
    visual.simulate_mouse_move(end, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(end, MouseButton::Left, Modifiers::default());
    fixture.pointers(context)?;
    assert!(
        fixture.copied(context)?.is_empty(),
        "stale press cannot start a drag on the new frame"
    );
    let outside = fixture.cell(context, COLUMNS, 0)?;
    visual.simulate_mouse_down(start, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_move(outside, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(outside, MouseButton::Left, Modifiers::default());
    fixture.pointers(context)?;
    assert!(
        fixture.copied(context)?.starts_with("changed"),
        "outside drag reaches and clamps to the row edge"
    );
    Ok(())
}

/// A two-row wheel event must preserve both native reports and history distance.
const WHEEL_ROWS: i16 = 2;
/// Two half-row pixel events accumulate to one complete wheel report.
const HALF_WHEEL: f32 = 0.5;

#[gpui_kit::test]
fn pointer_wheel_routes_history_and_program_reports_from_live_modes(context: &mut TestAppContext) {
    check(&pointer_wheel(context));
}

/// Smooth wheel accumulation and Shift bypass share the same native-mode routing as buttons.
///
/// # Errors
/// Propagates fixture and native input failures.
///
/// # Panics
/// Fails if wheel counts or direction change, or tracked wheels also scroll local history.
fn pointer_wheel(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{Modifiers, ScrollDelta, ScrollWheelEvent, VisualTestContext, point};
    use iznik_app::grid::GridScroll;
    use libghostty_vt::terminal::ScrollViewport;

    let fixture = Fixture::new(context)?;
    let scrolls = Rc::new(RefCell::new(Vec::new()));
    let received = Rc::clone(&scrolls);
    let _subscription = fixture.handle.update(context, |_, _, context| {
        context.subscribe(&context.entity(), move |_, _, event: &GridScroll, _| {
            received.borrow_mut().push(event.scroll);
        })
    })?;
    let position = fixture.cell(context, 0, 0)?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    visual.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0.0, -f32::from(WHEEL_ROWS))),
        ..ScrollWheelEvent::default()
    });
    assert!(
        fixture.pointers(context)?.is_empty(),
        "untracked wheel is local"
    );
    assert_eq!(
        *scrolls.borrow(),
        vec![ScrollViewport::Delta(isize::from(WHEEL_ROWS))],
        "history keeps the full row count"
    );
    scrolls.borrow_mut().clear();
    fixture.modes(context, b"\x1b[?1000h\x1b[?1006h")?;
    visual.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0.0, f32::from(WHEEL_ROWS))),
        ..ScrollWheelEvent::default()
    });
    assert_eq!(
        fixture.pointers(context)?,
        b"\x1b[<64;1;1M\x1b[<64;1;1M",
        "tracked wheel repeats every native report"
    );
    assert!(
        scrolls.borrow().is_empty(),
        "program wheel does not move local history"
    );
    let half = px(f32::from(GridMetrics::default().line_height) * HALF_WHEEL);
    let event = ScrollWheelEvent {
        position,
        delta: ScrollDelta::Pixels(point(px(0.0), half)),
        ..ScrollWheelEvent::default()
    };
    visual.simulate_event(event.clone());
    assert!(
        fixture.requests.borrow().is_empty(),
        "partial wheel stays accumulated"
    );
    visual.simulate_event(event);
    assert_eq!(
        fixture.pointers(context)?,
        b"\x1b[<64;1;1M",
        "two halves produce one native wheel"
    );
    visual.simulate_event(ScrollWheelEvent {
        position,
        delta: ScrollDelta::Lines(point(0.0, -1.0)),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
        ..ScrollWheelEvent::default()
    });
    assert!(
        fixture.requests.borrow().is_empty(),
        "Shift wheel bypasses the process"
    );
    assert_eq!(
        *scrolls.borrow(),
        vec![ScrollViewport::Delta(1)],
        "Shift wheel moves local history under tracking"
    );
    Ok(())
}

/// A doubled backing scale represents a common high-density display.
const POINTER_SCALE: f32 = 2.0;

#[gpui_kit::test]
fn pointer_pixel_protocol_uses_the_window_backing_scale(context: &mut TestAppContext) {
    check(&pointer_scale(context));
}

/// Logical event coordinates and cell geometry are converted to physical pixels together.
///
/// # Errors
/// Propagates fixture and native pointer failures.
///
/// # Panics
/// Fails if scaled pixel reporting disagrees with the displayed cell center.
fn pointer_scale(context: &mut TestAppContext) -> Result<(), Failed> {
    use gpui_kit::{Modifiers, MouseButton, VisualTestContext};
    let fixture = Fixture::new(context)?;
    context.simulate_window_scale_factor_change(fixture.handle.into(), POINTER_SCALE);
    fixture.modes(context, b"\x1b[?1000h\x1b[?1006h\x1b[?1016h")?;
    let position = fixture.cell(context, 0, 0)?;
    let mut visual = VisualTestContext::from_window(fixture.handle.into(), context);
    visual.simulate_mouse_down(position, MouseButton::Left, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Left, Modifiers::default());
    assert_eq!(
        fixture.pointers(context)?,
        b"\x1b[<0;8;18M\x1b[<0;8;18m",
        "native pixel positions follow the backing scale"
    );
    Ok(())
}
