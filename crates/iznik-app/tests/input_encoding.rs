//! Fixed input-mode matrix, first pinned at the native encoder boundary.

#[path = "fixtures/input_modes.rs"]
mod input_modes;

/// Every legacy mode combination agrees with the committed byte fixture.
///
/// # Panics
/// Fails when the pinned encoder rejects a fixture or changes its input bytes.
#[test]
fn input_mode_fixtures_pin_the_native_encoder() {
    use libghostty_vt::key::{Action, Encoder, Event, Mods};
    use libghostty_vt::terminal::{Options, Terminal};

    assert_eq!(input_modes::CASES.len(), 72);
    for case in input_modes::CASES {
        let mut terminal = Terminal::new(Options {
            cols: 80,
            rows: 24,
            max_scrollback: 0,
        })
        .expect("terminal");
        terminal.vt_write(case.modes);
        let mut encoder = Encoder::new().expect("encoder");
        encoder.set_options_from_terminal(&terminal);
        let mut event = Event::new().expect("event");
        event.set_action(Action::Press).set_key(case.key);
        if !case.text.is_empty() {
            event.set_utf8(Some(case.text));
        }
        if let Some(character) = case.unshifted {
            event.set_unshifted_codepoint(character);
        }
        if case.control_shift {
            event.set_mods(Mods::CTRL | Mods::SHIFT);
        }
        let mut actual = Vec::new();
        encoder.encode_to_vec(&event, &mut actual).expect("encode");
        assert_eq!(actual, case.expected, "{}", case.name);
    }
}

mod support;

use iznik_app::input::{KeyInput, MouseInput, TerminalInput};
use iznik_app::vt::{VtCommand, VtError, VtOptions, VtOutput, VtThread};
use iznik_protocol::identity::Sequence;
use libghostty_vt::{key, mouse};

/// Encode one owned request through the real owner thread, without a render frame.
///
/// # Errors
/// Returns thread, emulator or unexpected reply failures.
///
/// # Panics
/// Fails if the VT owner does not reply before the fixture deadline.
fn encoded(thread: &VtThread, input: TerminalInput) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    thread.send(VtCommand::Input {
        key: support::key(),
        input,
    })?;
    match support::receive(thread).result? {
        Some(VtOutput::Input(bytes)) => Ok(bytes),
        _ => Err("input unexpectedly produced a render snapshot".into()),
    }
}

/// The application service preserves every committed mode combination byte-for-byte.
///
/// # Panics
/// Fails if mode state is stale, metadata is lost or input waits for rendering.
#[test]
fn input_mode_matrix_runs_on_the_owning_thread() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    for case in input_modes::CASES {
        support::open(&thread, Sequence(0), 80, 24).expect("open");
        thread
            .send(VtCommand::Feed {
                receipt: None,
                key: support::key(),
                sequence: Sequence(0),
                bytes: case.modes.to_vec(),
            })
            .expect("mode change");
        let input = TerminalInput::Key(KeyInput {
            key: case.key,
            action: key::Action::Press,
            modifiers: if case.control_shift {
                key::Mods::CTRL | key::Mods::SHIFT
            } else {
                key::Mods::empty()
            },
            consumed: key::Mods::empty(),
            text: case.text.to_owned(),
            unshifted: case.unshifted,
        });
        // Queue input before the application has consumed the mode-change snapshot.
        thread
            .send(VtCommand::Input {
                key: support::key(),
                input,
            })
            .expect("input");
        let snapshot = support::snapshot(&thread).expect("mode snapshot");
        assert_eq!(
            usize::try_from(snapshot.consumed_bytes).expect("credit fits"),
            case.modes.len()
        );
        let reply = support::receive(&thread)
            .result
            .expect("encoded")
            .expect("reply");
        let VtOutput::Input(actual) = reply else {
            panic!("input produced a render frame");
        };
        assert_eq!(actual, case.expected, "{}", case.name);
        assert!(
            thread.poll().is_none(),
            "input produces no extra render frame"
        );
    }
}

/// Bracket framing follows live mode and pasted terminators cannot escape the frame.
///
/// # Panics
/// Fails if control bytes survive sanitization or framing/newlines ignore live mode.
#[test]
fn input_paste_sanitizes_payload_before_mode_dependent_framing() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    support::open(&thread, Sequence(0), 80, 24).expect("open");
    let text = "first\nsecond\u{1b}[201~\u{0}\u{e9}";
    let plain = encoded(&thread, TerminalInput::Paste(text.to_owned())).expect("plain paste");
    assert_eq!(plain, "first\rsecond [201~ \u{e9}".as_bytes());
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: b"\x1b[?2004h".to_vec(),
        })
        .expect("bracketed mode");
    let current = support::snapshot(&thread).expect("mode snapshot");
    let bracketed =
        encoded(&thread, TerminalInput::Paste(text.to_owned())).expect("bracketed paste");
    assert_eq!(
        bracketed,
        "\u{1b}[200~first\nsecond [201~ \u{e9}\u{1b}[201~".as_bytes()
    );
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: current.sequence,
            bytes: b"\x1b[?2004l".to_vec(),
        })
        .expect("plain mode again");
    support::snapshot(&thread).expect("mode snapshot");
    assert_eq!(
        encoded(&thread, TerminalInput::Paste(text.to_owned())).expect("plain again"),
        plain
    );
}

/// Pointer geometry shared by the mouse protocol fixture cases.
fn pointer(action: mouse::Action) -> MouseInput {
    /// Two text cells into an eighty-column surface, using the default grid width.
    const GEOMETRY: mouse::EncoderSize = mouse::EncoderSize {
        screen_width: 640,
        screen_height: 432,
        cell_width: 8,
        cell_height: 18,
        padding_top: 0,
        padding_bottom: 0,
        padding_left: 0,
        padding_right: 0,
    };
    /// A point in terminal column two and row two, counted from one on the wire.
    const POSITION: mouse::Position = mouse::Position { x: 8.0, y: 18.0 };
    MouseInput {
        action,
        button: Some(mouse::Button::Left),
        modifiers: key::Mods::empty(),
        position: POSITION,
        geometry: GEOMETRY,
        pressed: action != mouse::Action::Release,
    }
}

/// Mode output and the exact press, release and drag input encodings.
type MouseCase = (&'static [u8], &'static [u8], &'static [u8], &'static [u8]);

/// Mouse events obey tracking and encoding modes instead of always becoming input.
///
/// # Panics
/// Fails if disabled motion, press/release or SGR coordinates are encoded incorrectly.
#[test]
fn input_mouse_tracks_only_requested_events_in_the_live_protocol() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    let cases: &[MouseCase] = &[
        (b"", b"", b"", b""),
        (b"\x1b[?9h", b"\x1b[M \x22\x22", b"", b""),
        (b"\x1b[?1000h", b"\x1b[M \x22\x22", b"\x1b[M#\x22\x22", b""),
        (
            b"\x1b[?1002h",
            b"\x1b[M \x22\x22",
            b"\x1b[M#\x22\x22",
            b"\x1b[M@#\x22",
        ),
        (
            b"\x1b[?1003h",
            b"\x1b[M \x22\x22",
            b"\x1b[M#\x22\x22",
            b"\x1b[M@#\x22",
        ),
        (b"\x1b[?1006h", b"", b"", b""),
        (b"\x1b[?1000;1006h", b"\x1b[<0;2;2M", b"\x1b[<0;2;2m", b""),
        (
            b"\x1b[?1002;1006h",
            b"\x1b[<0;2;2M",
            b"\x1b[<0;2;2m",
            b"\x1b[<32;3;2M",
        ),
        (
            b"\x1b[?1003;1006h",
            b"\x1b[<0;2;2M",
            b"\x1b[<0;2;2m",
            b"\x1b[<32;3;2M",
        ),
    ];
    for (modes, press, release, motion) in cases {
        support::open(&thread, Sequence(0), 80, 24).expect("open");
        thread
            .send(VtCommand::Feed {
                receipt: None,
                key: support::key(),
                sequence: Sequence(0),
                bytes: modes.to_vec(),
            })
            .expect("modes");
        support::snapshot(&thread).expect("mode snapshot");
        assert_eq!(
            encoded(&thread, TerminalInput::Mouse(pointer(mouse::Action::Press))).expect("press"),
            *press,
            "press {modes:?}"
        );
        assert_eq!(
            encoded(
                &thread,
                TerminalInput::Mouse(pointer(mouse::Action::Release))
            )
            .expect("release"),
            *release,
            "release {modes:?}"
        );
        let mut moved = pointer(mouse::Action::Motion);
        moved.position.x = 16.0;
        assert_eq!(
            encoded(&thread, TerminalInput::Mouse(moved)).expect("motion"),
            *motion,
            "motion {modes:?}"
        );
    }
}

/// Input cannot use mode state after a stream discontinuity.
///
/// # Panics
/// Fails if a poisoned terminal encodes new input before an authoritative screen.
#[test]
fn input_requires_an_authoritative_terminal_after_a_gap() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    support::open(&thread, Sequence(0), 80, 24).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(1),
            bytes: b"gap".to_vec(),
        })
        .expect("gap");
    assert!(matches!(
        support::receive(&thread).result,
        Err(VtError::NeedsScreen)
    ));
    thread
        .send(VtCommand::Input {
            key: support::key(),
            input: TerminalInput::Paste("text".to_owned()),
        })
        .expect("input");
    assert!(matches!(
        support::receive(&thread).result,
        Err(VtError::NeedsScreen)
    ));
}

/// Copy the requested displayed range through the owning-thread serializer.
///
/// # Errors
/// Returns thread, selection or unexpected reply failures.
///
/// # Panics
/// Fails if the VT owner misses its reply deadline.
fn copied(
    thread: &VtThread,
    snapshot: &iznik_app::vt::TerminalSnapshot,
    selection: iznik_app::grid::GridSelection,
) -> Result<String, Box<dyn std::error::Error>> {
    thread.send(VtCommand::Input {
        key: snapshot.key.clone(),
        input: TerminalInput::Copy(iznik_app::input::CopyInput {
            selection,
            frame: iznik_app::input::InputFrame::from(snapshot),
        }),
    })?;
    match support::receive(thread).result? {
        Some(VtOutput::Clipboard(text)) => Ok(text),
        _ => Err("copy unexpectedly produced terminal input or a render frame".into()),
    }
}

/// Selection uses native graphemes and strips styling while accepting either direction.
///
/// # Panics
/// Fails if wide/combining cells, direction or empty selection change clipboard text.
#[test]
fn input_copy_serializes_selected_graphemes_without_styles() {
    use iznik_app::grid::{GridPosition, GridSelection};
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    support::open(&thread, Sequence(0), 12, 4).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: "ab\u{1b}[1;31m\u{6f22}e\u{301}\u{1b}[0m!"
                .as_bytes()
                .to_vec(),
        })
        .expect("text");
    let current = support::snapshot(&thread).expect("snapshot");
    let selection = GridSelection {
        anchor: GridPosition { row: 0, column: 2 },
        head: GridPosition { row: 0, column: 5 },
    };
    assert_eq!(
        copied(&thread, &current, selection).expect("copy"),
        "\u{6f22}e\u{301}"
    );
    assert_eq!(
        copied(
            &thread,
            &current,
            GridSelection {
                anchor: selection.head,
                head: selection.anchor
            }
        )
        .expect("backward"),
        "\u{6f22}e\u{301}"
    );
    assert_eq!(
        copied(
            &thread,
            &current,
            GridSelection {
                anchor: selection.anchor,
                head: selection.anchor
            }
        )
        .expect("empty"),
        ""
    );
    assert!(
        thread.poll().is_none(),
        "copy earns no credit and publishes no render frame"
    );
}

/// Native formatting joins soft wraps and follows the exact historical viewport.
///
/// # Panics
/// Fails if wrapping adds a line break or a stale frame copies different visible text.
#[test]
fn input_copy_unwraps_and_rejects_a_changed_display_frame() {
    use iznik_app::grid::{GridPosition, GridSelection};
    use libghostty_vt::terminal::ScrollViewport;
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    support::open(&thread, Sequence(0), 4, 2).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: b"abcdefgh".to_vec(),
        })
        .expect("wrapped text");
    let wrapped = support::snapshot(&thread).expect("snapshot");
    let selection = GridSelection {
        anchor: GridPosition { row: 0, column: 0 },
        head: GridPosition { row: 1, column: 4 },
    };
    assert_eq!(
        copied(&thread, &wrapped, selection).expect("wrapped copy"),
        "abcdefgh"
    );
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: wrapped.sequence,
            bytes: b"\r\nijkl\r\nmnop".to_vec(),
        })
        .expect("history");
    support::snapshot(&thread).expect("snapshot");
    assert!(
        copied(&thread, &wrapped, selection).is_err(),
        "output invalidates an old selection frame"
    );
    thread
        .send(VtCommand::Scroll {
            key: support::key(),
            scroll: ScrollViewport::Top,
        })
        .expect("history top");
    let history = support::snapshot(&thread).expect("historical frame");
    assert_eq!(
        copied(&thread, &history, selection).expect("history copy"),
        "abcdefgh"
    );
    thread
        .send(VtCommand::Scroll {
            key: support::key(),
            scroll: ScrollViewport::Bottom,
        })
        .expect("bottom");
    let bottom = support::snapshot(&thread).expect("bottom frame");
    assert!(
        copied(&thread, &history, selection).is_err(),
        "scrolling invalidates viewport coordinates"
    );
    thread
        .send(VtCommand::Resize {
            key: support::key(),
            columns: 8,
            rows: 2,
        })
        .expect("resize");
    support::snapshot(&thread).expect("resized frame");
    assert!(
        copied(&thread, &bottom, selection).is_err(),
        "resize invalidates old cell geometry"
    );
}

/// Wheel presses use native wheel button codes; malformed geometry stays outside FFI.
///
/// # Panics
/// Fails if wheel direction or modifiers are lost, or invalid coordinates are encoded.
#[test]
fn input_mouse_encodes_wheel_and_rejects_invalid_geometry() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    support::open(&thread, Sequence(0), 80, 24).expect("open");
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: b"\x1b[?1000;1006h".to_vec(),
        })
        .expect("modes");
    support::snapshot(&thread).expect("snapshot");
    for (button, expected) in [
        (mouse::Button::Four, b"\x1b[<64;2;2M"),
        (mouse::Button::Five, b"\x1b[<65;2;2M"),
    ] {
        let mut wheel = pointer(mouse::Action::Press);
        wheel.button = Some(button);
        wheel.pressed = false;
        assert_eq!(
            encoded(&thread, TerminalInput::Mouse(wheel)).expect("wheel"),
            expected
        );
    }
    let mut modified = pointer(mouse::Action::Press);
    modified.modifiers = key::Mods::SHIFT | key::Mods::CTRL;
    assert_eq!(
        encoded(&thread, TerminalInput::Mouse(modified)).expect("modified pointer"),
        b"\x1b[<20;2;2M"
    );
    let mut invalid = pointer(mouse::Action::Press);
    invalid.geometry.cell_width = 0;
    let _width_error =
        encoded(&thread, TerminalInput::Mouse(invalid)).expect_err("zero cell width");
    let mut position = pointer(mouse::Action::Press);
    position.position.x = f32::NAN;
    let _position_error =
        encoded(&thread, TerminalInput::Mouse(position)).expect_err("nonfinite position");
}

/// Unicode text and release/repeat metadata survive the request channel.
///
/// # Panics
/// Fails if native extended keyboard encoding loses event type or layout text.
#[test]
fn input_preserves_unicode_and_extended_key_event_types() {
    let thread = VtThread::start(VtOptions::default()).expect("thread");
    support::open(&thread, Sequence(0), 80, 24).expect("open");
    let text = KeyInput {
        key: key::Key::Unidentified,
        action: key::Action::Press,
        modifiers: key::Mods::empty(),
        consumed: key::Mods::empty(),
        text: "\u{6f22}\u{e9}".to_owned(),
        unshifted: None,
    };
    assert_eq!(
        encoded(&thread, TerminalInput::Key(text.clone())).expect("Unicode"),
        text.text.as_bytes()
    );
    let mut repeated = text.clone();
    repeated.action = key::Action::Repeat;
    assert_eq!(
        encoded(&thread, TerminalInput::Key(repeated)).expect("repeat"),
        text.text.as_bytes()
    );
    let mut released = text;
    released.action = key::Action::Release;
    assert!(
        encoded(&thread, TerminalInput::Key(released))
            .expect("legacy release")
            .is_empty()
    );
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: b"\x1b[>11u".to_vec(),
        })
        .expect("extended keyboard");
    support::snapshot(&thread).expect("mode snapshot");
    let mut backspace = KeyInput {
        key: key::Key::Backspace,
        action: key::Action::Press,
        modifiers: key::Mods::empty(),
        consumed: key::Mods::empty(),
        text: String::new(),
        unshifted: None,
    };
    assert_eq!(
        encoded(&thread, TerminalInput::Key(backspace.clone())).expect("extended press"),
        b"\x1b[127u"
    );
    backspace.action = key::Action::Release;
    assert_eq!(
        encoded(&thread, TerminalInput::Key(backspace)).expect("extended release"),
        b"\x1b[127;1:3u"
    );
}

/// Native wheel expansion is bounded by options, while local history keeps its full distance.
///
/// # Panics
/// Fails if a burst exceeds its report cap or a local fallback is silently truncated.
#[test]
fn input_pointer_limits_native_wheel_expansion_without_truncating_history() {
    use iznik_app::input::{InputFrame, PointerAction, PointerInput};

    let thread = VtThread::start(VtOptions {
        maximum_wheel_reports: 2,
        ..VtOptions::default()
    })
    .expect("thread");
    let frame = support::open(&thread, Sequence(0), 80, 24).expect("screen");
    let mut mouse = pointer(mouse::Action::Press);
    mouse.button = Some(mouse::Button::Four);
    mouse.pressed = false;
    let input = PointerInput {
        mouse,
        local: Some(PointerAction::Scroll(-3)),
        frame: InputFrame::from(&frame),
    };
    thread
        .send(VtCommand::Input {
            key: support::key(),
            input: TerminalInput::Pointer(input.clone()),
        })
        .expect("local wheel");
    let Some(VtOutput::LocalPointer(local)) =
        support::receive(&thread).result.expect("local reply")
    else {
        panic!("local fallback missing");
    };
    assert!(matches!(local.local, Some(PointerAction::Scroll(-3))));
    thread
        .send(VtCommand::Feed {
            receipt: None,
            key: support::key(),
            sequence: Sequence(0),
            bytes: b"\x1b[?1000h\x1b[?1006h".to_vec(),
        })
        .expect("enable tracking");
    support::snapshot(&thread).expect("mode snapshot");
    thread
        .send(VtCommand::Input {
            key: support::key(),
            input: TerminalInput::Pointer(input),
        })
        .expect("native wheel");
    assert!(matches!(
        support::receive(&thread).result,
        Err(VtError::Input(
            "wheel burst exceeds the configured report limit"
        ))
    ));
}

/// Clipboard frame identity includes its pane, even when sequence and dimensions agree.
///
/// # Panics
/// Fails if a frame naming another pane can select text from the command's pane.
#[test]
fn input_copy_rejects_a_frame_from_another_pane() {
    use iznik_app::grid::{GridPosition, GridSelection};
    use iznik_app::input::{CopyInput, InputFrame};
    use iznik_protocol::identity::PaneId;

    let thread = VtThread::start(VtOptions::default()).expect("thread");
    let snapshot = support::open(&thread, Sequence(0), 80, 24).expect("screen");
    let mut frame = InputFrame::from(&snapshot);
    frame.key.pane = PaneId(2);
    thread
        .send(VtCommand::Input {
            key: support::key(),
            input: TerminalInput::Copy(CopyInput {
                frame,
                selection: GridSelection {
                    anchor: GridPosition { row: 0, column: 0 },
                    head: GridPosition { row: 0, column: 1 },
                },
            }),
        })
        .expect("copy request");
    assert!(matches!(
        support::receive(&thread).result,
        Err(VtError::Input(
            "selection no longer matches the displayed frame"
        ))
    ));
}
