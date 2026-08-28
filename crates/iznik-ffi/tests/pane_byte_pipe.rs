//! A pane's bytes, handed to a surface.
//!
//! The one path at this boundary where bytes are not copied for the crossing,
//! and the one where flow control is the application's to keep: a surface that
//! stops consuming stops its own pane and nothing else. Every case here calls
//! the `extern "C"` functions against a daemon on this machine.

use core::ffi::{c_int, c_void};
use core::time::Duration;
use std::ffi::CString;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Instant;

use iznik::error::{Error, Layer, OK};
use iznik::pane::{
    PaneCallbacks, iznik_pane_attach, iznik_pane_credit, iznik_pane_detach, iznik_pane_input,
    iznik_pane_resize,
};
use iznik::{
    Client, Configuration, iznik_client_free, iznik_client_new, iznik_command, iznik_host_add,
};
use iznik_protocol::command::{SessionCommand, encode_session_command};
use iznik_testkit::stack::{Stack, StackOptions};
use iznik_testkit::vt::Vt;
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};

/// How long a case waits for something that should happen at once.
const PROMPT: Duration = Duration::from_secs(10);

/// How long it waits to be sure something is *not* going to happen.
const BRIEF: Duration = Duration::from_millis(400);

/// The width a pane is made at.
const COLUMNS: u16 = 80;

/// Its height.
const ROWS: u16 = 24;

/// The width a resize asks for.
const WIDER: u16 = 100;

/// The height it asks for.
const TALLER: u16 = 30;

/// The pane every case attaches to.
const PANE: u64 = 1;

/// How many bytes the host sends before it must be given more.
const WINDOW: usize = 256 * 1024;

/// How much a case asks a pane to print when it wants the window to run out.
const FLOOD: usize = 3 * WINDOW;

/// How many threads type at once in the atomicity case.
const TYPISTS: usize = 100;

/// What the shell is asked to run before they start typing.
///
/// A shell of its own would keep only some of a burst, and not the same some
/// twice: it re-arms its terminal before each line it reads, and the call it
/// uses throws away input that arrived but has not been read yet. That is the
/// shell's doing, not this boundary's, and a case that measured it would be
/// measuring how far the shell had got. `cat` re-arms nothing and reads
/// everything, so what a hundred callers typed is all there to be read — and
/// twice over, once as the terminal echoed it and once as `cat` wrote it
/// back.
const READER: &str = "cat\n";

/// How many times each of them repeats its own pattern.
///
/// Short, deliberately. What is being asked is whether one call is one
/// message; long lines would make it a case about how fast a shell reads.
const REPEATS: usize = 2;

/// How many characters name a caller.
const WIDTH: usize = 2;

/// What every caller's line begins with: a comment, which the shell reads and
/// discards. A line it would try to run costs a fork and a failed exec — most
/// of a second each, which would make this a case about how fast a shell
/// gives up rather than about what arrived.
const SILENT: char = '#';

/// A line typed before they start, whose coming back twice is what says the
/// reader is reading.
///
/// Letters, deliberately: every four-digit line whose halves match is some
/// caller's own, so a probe of that shape would be caller ninety-nine's, and
/// its arrival would prove theirs.
const PROBE: &str = "ready";

/// How many times a line comes back: once as the terminal echoed it, once as
/// the reader wrote it back.
const TWICE: usize = 2;

/// What the shell says before it is given something to run.
///
/// It is written by the shell while the terminal is echoing what arrives, so
/// it lands wherever it lands — in the middle of a caller's line as readily as
/// between two. Taking it out is what leaves the callers' own bytes to be
/// read: an echo interrupted by the shell's prompt is the terminal's doing,
/// and an echo interrupted by another caller's bytes would be this
/// boundary's.
const PROMPTED: &str = "$ ";

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// A client, carried to another thread.
///
/// Every function at this boundary is safe to call from any thread — they are
/// serialized among themselves — and that is what the case about a hundred
/// callers is for.
#[derive(Clone, Copy)]
struct Reachable(*mut Client);

// SAFETY: the boundary's own promise, stated in its header and its module:
// every function may be called from any thread, and the calls are serialized
// inside. Nothing in this case dereferences the pointer itself.
unsafe impl Send for Reachable {}

// SAFETY: as above.
unsafe impl Sync for Reachable {}

/// What the case that lets a pane go from inside a handler passes to it.
struct Leaving {
    /// The client to call back into.
    client: Reachable,
    /// The host, kept null-terminated for that call.
    alias: CString,
    /// What the handler has been told, and what came of its own call.
    told: Mutex<Left>,
}

/// What that handler saw.
#[derive(Debug, Default)]
struct Left {
    /// How many times output arrived.
    calls: usize,
    /// What letting the pane go answered, once it has.
    answered: Option<c_int>,
}

/// A handler that lets its own pane go, from inside the call.
///
/// The one call that must not wait for the call it is inside: waiting for a
/// handler to finish is exactly what makes an application able to free what it
/// gave, and a handler doing it to itself would wait for ever.
extern "C" fn leaves(context: *mut c_void, _bytes: *const u8, _length: usize) {
    if context.is_null() {
        return;
    }
    // SAFETY: the context is this case's own box, alive until the case ends.
    let leaving = unsafe { &*context.cast::<Leaving>() };
    let first = {
        let Ok(mut told) = leaving.told.lock() else {
            return;
        };
        told.calls = told.calls.saturating_add(1);
        told.calls == 1
    };
    if !first {
        return;
    }
    // SAFETY: the client is live for the whole of the case and the alias is
    // null-terminated; no error is asked for.
    let answered = unsafe {
        iznik_pane_detach(
            leaving.client.0,
            leaving.alias.as_ptr(),
            PANE,
            core::ptr::null_mut(),
        )
    };
    if let Ok(mut told) = leaving.told.lock() {
        told.answered = Some(answered);
    }
}

/// What a pane's handlers have been given.
#[derive(Debug, Default)]
struct Watched {
    /// Every byte of output, in the order it arrived.
    output: Vec<u8>,
    /// The screens, as the bytes that reproduce them and the size they were
    /// sent at.
    screens: Vec<(u64, u16, u16, Vec<u8>)>,
    /// Whether output arrived before the first screen, which the boundary
    /// forbids.
    output_before_screen: bool,
    /// How many times the pane was said to have detached.
    detachments: usize,
}

/// The pane's own output, kept where it was handed over.
extern "C" fn output(context: *mut c_void, bytes: *const u8, length: usize) {
    let Some(watched) = watching(context) else {
        return;
    };
    let Ok(mut held) = watched.lock() else {
        return;
    };
    if held.screens.is_empty() {
        held.output_before_screen = true;
    }
    if !bytes.is_null() {
        // SAFETY: iznik's own promise: `length` readable bytes, valid for this
        // call, which is where they are copied.
        held.output
            .extend_from_slice(unsafe { core::slice::from_raw_parts(bytes, length) });
    }
}

/// The pane's screen, which obliges whoever gets it to draw that and nothing
/// before it.
extern "C" fn screen(
    context: *mut c_void,
    sequence: u64,
    columns: u16,
    rows: u16,
    bytes: *const u8,
    length: usize,
) {
    let Some(watched) = watching(context) else {
        return;
    };
    let Ok(mut held) = watched.lock() else {
        return;
    };
    let drawn = if bytes.is_null() {
        Vec::new()
    } else {
        // SAFETY: iznik's own promise, as above.
        unsafe { core::slice::from_raw_parts(bytes, length) }.to_vec()
    };
    held.screens.push((sequence, columns, rows, drawn));
}

/// The host has stopped sending this pane.
extern "C" fn detached(context: *mut c_void) {
    let Some(watched) = watching(context) else {
        return;
    };
    if let Ok(mut held) = watched.lock() {
        held.detachments = held.detachments.saturating_add(1);
    }
}

/// The record a context points at.
fn watching<'held>(context: *mut c_void) -> Option<&'held Mutex<Watched>> {
    if context.is_null() {
        return None;
    }
    // SAFETY: the context is the pointer the case passed to
    // `iznik_pane_attach`, which outlives the attachment.
    Some(unsafe { &*context.cast::<Mutex<Watched>>() })
}

/// Every handler these cases use.
fn handlers() -> PaneCallbacks {
    PaneCallbacks {
        output: Some(output),
        screen: Some(screen),
        mark: None,
        detached: Some(detached),
    }
}

/// A temporary directory of this case's own, removed when the guard drops.
#[derive(Debug)]
struct Scratch {
    /// Where it is.
    path: PathBuf,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.path);
    }
}

/// A scratch directory named for `case`.
///
/// # Errors
///
/// When it cannot be made.
fn scratch(case: &str) -> Result<Scratch, Failed> {
    let base = std::env::var_os("TMPDIR").map_or_else(std::env::temp_dir, PathBuf::from);
    let path = base.join(format!("iznik-pipe-{case}-{}", std::process::id()));
    let _gone = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path)?;
    Ok(Scratch { path })
}

/// A runtime for the daemon a case stands up.
///
/// # Errors
///
/// When it cannot be built.
fn runtime() -> Result<Runtime, Failed> {
    Ok(RuntimeBuilder::new_multi_thread().enable_all().build()?)
}

/// An error struct to be filled in.
fn blank() -> Error {
    Error {
        code: OK,
        layer: Layer::Client,
        message: core::ptr::null(),
    }
}

/// What an error says.
fn said(error: &Error) -> String {
    if error.message.is_null() {
        return String::new();
    }
    // SAFETY: iznik's own promise: valid until the next call on this thread.
    unsafe { core::ffi::CStr::from_ptr(error.message) }
        .to_string_lossy()
        .into_owned()
}

/// A client, a daemon, and a session with one pane on it.
///
/// # Errors
///
/// When any of the three will not come up.
fn connected(held: &Scratch, runtime: &Runtime) -> Result<(Stack, *mut Client, CString), Failed> {
    let stack = runtime.block_on(Stack::start(StackOptions::default()))?;
    let directory = CString::new(held.path.join("runtime").display().to_string())?;
    let configuration = Configuration {
        runtime_directory: directory.as_ptr(),
        artifacts_directory: core::ptr::null(),
        askpass_program: core::ptr::null(),
        log_path: core::ptr::null(),
    };
    let mut error = blank();
    // SAFETY: the configuration is alive for this call and its string is
    // null-terminated.
    let client = unsafe { iznik_client_new(&raw const configuration, &raw mut error) };
    if client.is_null() {
        return Err(format!("no client: {}", said(&error)).into());
    }
    let alias = CString::new(format!("unix:{}", stack.socket().display()))?;
    // SAFETY: the client is live and the alias null-terminated.
    let added = unsafe { iznik_host_add(client, alias.as_ptr(), &raw mut error) };
    if added != OK {
        return Err(format!("the host was not taken: {}", said(&error)).into());
    }
    make_a_session(client, &alias)?;
    Ok((stack, client, alias))
}

/// Makes a session, and waits until the host has one.
///
/// # Errors
///
/// When the command will not go, or the session never appears.
fn make_a_session(client: *mut Client, alias: &CString) -> Result<(), Failed> {
    let asked = encode_session_command(&SessionCommand::CreateSession {
        name: "work".to_owned(),
        columns: COLUMNS,
        rows: ROWS,
        working_directory: None,
    })?;
    let mut error = blank();
    let mut number: u64 = 0;
    let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
    while Instant::now() < expires {
        // SAFETY: every pointer is alive for the call and the bytes are this
        // case's own.
        let sent = unsafe {
            iznik_command(
                client,
                alias.as_ptr(),
                asked.as_ptr(),
                asked.len(),
                &raw mut number,
                &raw mut error,
            )
        };
        if sent == OK {
            // The pane exists once the host has answered; the attach below
            // waits for what the host says about it.
            std::thread::sleep(Duration::from_millis(200));
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(format!("no session: {}", said(&error)).into())
}

/// Waits until what a pane's handlers were given satisfies `wanted`.
///
/// # Errors
///
/// When it never does.
fn await_watched(
    watched: *mut Mutex<Watched>,
    what: &str,
    patience: Duration,
    wanted: impl Fn(&Watched) -> bool,
) -> Result<(), Failed> {
    let expires = Instant::now().checked_add(patience).ok_or("no clock")?;
    while Instant::now() < expires {
        // SAFETY: this case's own box, alive until the case ends.
        let held = unsafe { &*watched };
        if held.lock().is_ok_and(|kept| wanted(&kept)) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // SAFETY: this case's own box, alive until the case ends.
    let said = unsafe { &*watched }.lock().map_or_else(
        |_broken| "a record nothing can read".to_owned(),
        |kept| format!("{:?}", String::from_utf8_lossy(&kept.output)),
    );
    Err(format!("{what} never happened; the pane said {said}").into())
}

/// Types one line into a pane.
///
/// # Errors
///
/// When the boundary refuses it.
fn typed(client: *mut Client, alias: &CString, pane: u64, text: &str) -> Result<(), Failed> {
    let bytes = text.as_bytes();
    let mut error = blank();
    // SAFETY: every pointer is alive for the call.
    let sent = unsafe {
        iznik_pane_input(
            client,
            alias.as_ptr(),
            pane,
            bytes.as_ptr(),
            bytes.len(),
            &raw mut error,
        )
    };
    if sent == OK {
        return Ok(());
    }
    Err(format!("the input was refused: {}", said(&error)).into())
}

/// Attaches to a pane, with a record of this case's own.
///
/// # Errors
///
/// When the boundary refuses it.
fn attach(client: *mut Client, alias: &CString, pane: u64) -> Result<*mut Mutex<Watched>, Failed> {
    let watched: *mut Mutex<Watched> = Box::into_raw(Box::new(Mutex::new(Watched::default())));
    let mut error = blank();
    // SAFETY: the client is live, the alias null-terminated, and the record
    // outlives the attachment.
    let taken = unsafe {
        iznik_pane_attach(
            client,
            alias.as_ptr(),
            pane,
            handlers(),
            watched.cast::<c_void>(),
            &raw mut error,
        )
    };
    if taken != OK {
        return Err(format!("the pane was not taken: {}", said(&error)).into());
    }
    Ok(watched)
}

/// One caller's own pattern, which is what their line carries.
fn pattern(index: usize) -> String {
    format!("{index:02}").repeat(REPEATS)
}

/// Every line a caller typed, as the echo has it: what follows one of their
/// marks, up to the end of the line it is on, with the shell's own prompt
/// taken out.
///
/// A line is found by its mark rather than by beginning with one, because the
/// shell writes on the same line as the echo. Nothing is filtered: a line that
/// is not one caller's own pattern is what this case is looking for, so it has
/// to be returned rather than passed over. The last line is left out while it
/// has no end yet — it is still being echoed, not malformed.
fn typed_lines(output: &[u8]) -> Vec<String> {
    let said = String::from_utf8_lossy(output).replace(PROMPTED, "");
    said.split(SILENT)
        .skip(1)
        .filter_map(|piece| {
            piece
                .split_once(['\r', '\n'])
                .map(|(line, _rest)| line.to_owned())
        })
        .collect()
}

/// Whether a byte string holds another.
fn holds(held: &[u8], wanted: &[u8]) -> bool {
    !wanted.is_empty() && held.windows(wanted.len()).any(|piece| piece == wanted)
}

/// # Panics
///
/// When a pane's screen does not come before its bytes, or the bytes are not
/// what the pane said.
#[test]
fn pane_byte_pipe_draws_a_screen_before_it_hands_over_bytes() {
    let case = || -> Result<(), Failed> {
        let held = scratch("attach")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let watched = attach(client, &alias, PANE)?;
        await_watched(watched, "a screen", PROMPT, |kept| !kept.screens.is_empty())?;
        typed(client, &alias, PANE, "echo piped-$((6*7))\n")?;
        await_watched(watched, "the pane's own bytes", PROMPT, |kept| {
            holds(&kept.output, b"piped-42")
        })?;
        {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*watched }
                .lock()
                .map_err(|_broken| "the record")?;
            assert!(
                !kept.output_before_screen,
                "nothing is drawn before the screen that says what to draw it on"
            );
            let (_at, columns, rows, drawn) =
                kept.screens.first().ok_or("a screen was given")?.clone();
            assert_eq!((columns, rows), (COLUMNS, ROWS), "with the pane's size");
            // And it is a screen: an emulator fed it shows something.
            let mut mirrored = Vt::new(columns, rows).map_err(|error| error.to_string())?;
            mirrored.feed(&drawn);
            let shown = mirrored.snapshot().map_err(|error| error.to_string())?;
            assert!(
                shown.contains("size 80x24"),
                "that an emulator can reproduce: {shown}"
            );
        }
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once, after the client
        // that could have called into it is gone.
        drop(unsafe { Box::from_raw(watched) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a pane that is given no credit keeps flowing, or one beside it stops.
#[test]
fn pane_byte_pipe_stops_a_pane_that_returns_no_credit() {
    let case = || -> Result<(), Failed> {
        let held = scratch("credit")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let watched = attach(client, &alias, PANE)?;
        await_watched(watched, "a screen", PROMPT, |kept| !kept.screens.is_empty())?;
        // Far more than the window the host may send before it is given more.
        typed(
            client,
            &alias,
            PANE,
            &format!("head -c {FLOOD} /dev/zero | tr '\\0' 'x'\n"),
        )?;
        // It stops, having sent what it was allowed and no more.
        await_watched(watched, "the window running out", PROMPT, |kept| {
            kept.output.len() >= WINDOW
        })?;
        std::thread::sleep(BRIEF);
        let stalled = {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*watched }
                .lock()
                .map_err(|_broken| "the record")?;
            kept.output.len()
        };
        assert!(
            stalled < FLOOD,
            "a pane that returned no credit stopped at {stalled} of {FLOOD}"
        );
        // Returning credit lets it go on.
        let mut error = blank();
        // SAFETY: the client is live and the alias null-terminated.
        let returned = unsafe {
            iznik_pane_credit(
                client,
                alias.as_ptr(),
                PANE,
                u32::try_from(stalled).unwrap_or(u32::MAX),
                &raw mut error,
            )
        };
        assert_eq!(returned, OK, "credit is returned: {}", said(&error));
        await_watched(watched, "the pane going on", PROMPT, |kept| {
            kept.output.len() > stalled
        })?;
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once.
        drop(unsafe { Box::from_raw(watched) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a hundred callers typing at once have their messages interleaved.
#[test]
fn pane_byte_pipe_keeps_one_call_one_message() {
    let case = || -> Result<(), Failed> {
        let held = scratch("atomic")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let watched = attach(client, &alias, PANE)?;
        await_watched(watched, "a screen", PROMPT, |kept| !kept.screens.is_empty())?;
        // Something that reads without re-arming the terminal, so that what is
        // counted is what arrived rather than what a shell had got to.
        typed(client, &alias, PANE, READER)?;
        // Not that the shell echoed the name — that the thing it named is
        // reading: a line typed now comes back twice, once echoed and once
        // written back by what is reading it.
        typed(client, &alias, PANE, &format!("{SILENT}{PROBE}\n"))?;
        await_watched(watched, "the reader reading", PROMPT, |kept| {
            kept.output
                .windows(PROBE.len())
                .filter(|window| *window == PROBE.as_bytes())
                .count()
                >= TWICE
        })?;
        // A hundred callers, each with a pattern of its own, typing at once.
        let reachable = Reachable(client);
        let refused: Vec<usize> = std::thread::scope(|scope| {
            let typists: Vec<_> = (0..TYPISTS)
                .map(|index| {
                    let alias = &alias;
                    scope.spawn(move || {
                        // The whole wrapper, not its field: capturing the
                        // field would capture a raw pointer, and a raw
                        // pointer is not what this promises may cross a
                        // thread.
                        let carried = reachable;
                        let text = format!("{SILENT}{}\n", pattern(index));
                        typed(carried.0, alias, PANE, &text).is_err()
                    })
                })
                .collect();
            // Every one of them is joined: a typist still running is one
            // whose answer this case has not seen, and skipping it would let
            // the check below pass on nobody.
            typists
                .into_iter()
                .enumerate()
                .filter_map(|(index, typist)| typist.join().unwrap_or(true).then_some(index))
                .collect()
        });
        assert!(
            refused.is_empty(),
            "every caller's line was taken: {refused:?} were not"
        );
        // Every caller heard from, rather than a count of lines: a line
        // arrives twice, once echoed by the terminal and once written back by
        // the reader, but a terminal given more at once than it can echo drops
        // the echo — never the input, which is the reader's copy and is what
        // is waited for here.
        await_watched(watched, "the callers' lines", PROMPT, |kept| {
            let echoed = typed_lines(&kept.output);
            (0..TYPISTS).all(|index| echoed.contains(&pattern(index)))
        })?;
        // A moment for the rest, so the check reads every line that arrived
        // rather than the first few.
        std::thread::sleep(BRIEF);
        // What atomicity means: every line the pane says is one caller's own
        // and unbroken. Two calls that became one message on the wire, or one
        // call that became two, would show as a line carrying bytes from two
        // of them — which is neither caller's pattern, so it is at once a line
        // nobody asked for and a caller's line missing.
        {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*watched }
                .lock()
                .map_err(|_broken| "the record")?;
            // Everything the pane said except the line this case typed itself
            // to learn that the reader was reading: what is being read here is
            // the callers'.
            let echoed: Vec<String> = typed_lines(&kept.output)
                .into_iter()
                .filter(|line| line != PROBE)
                .collect();
            let mut every: Vec<String> = echoed.clone();
            every.sort();
            every.dedup();
            let mut wanted: Vec<String> = (0..TYPISTS).map(pattern).collect();
            wanted.sort();
            let missing: Vec<&String> =
                wanted.iter().filter(|line| !every.contains(line)).collect();
            let extra: Vec<&String> = every.iter().filter(|line| !wanted.contains(line)).collect();
            assert!(
                missing.is_empty() && extra.is_empty(),
                "every caller's line is there and no other: \
                 missing {missing:?}, extra {extra:?}, in {:?}",
                String::from_utf8_lossy(&kept.output)
            );
            for line in &echoed {
                let named = line.get(..WIDTH).unwrap_or_default();
                assert_eq!(
                    line,
                    &named.repeat(REPEATS),
                    "every line is one caller's own, unbroken by another's: {:?}",
                    String::from_utf8_lossy(&kept.output)
                );
            }
        }
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once.
        drop(unsafe { Box::from_raw(watched) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a resize does not reach the program in the pane.
#[test]
fn pane_byte_pipe_resizes_what_the_program_sees() {
    let case = || -> Result<(), Failed> {
        let held = scratch("resize")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let watched = attach(client, &alias, PANE)?;
        await_watched(watched, "a screen", PROMPT, |kept| !kept.screens.is_empty())?;
        let mut error = blank();
        // SAFETY: the client is live and the alias null-terminated.
        let resized = unsafe {
            iznik_pane_resize(client, alias.as_ptr(), PANE, WIDER, TALLER, &raw mut error)
        };
        assert_eq!(resized, OK, "the pane is resized: {}", said(&error));
        // The application decides the size, and the program in the pane is
        // the one that has to believe it.
        typed(client, &alias, PANE, "stty size\n")?;
        await_watched(
            watched,
            "the program's own account of its size",
            PROMPT,
            |kept| holds(&kept.output, format!("{TALLER} {WIDER}").as_bytes()),
        )?;
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once.
        drop(unsafe { Box::from_raw(watched) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a pane goes on being delivered after it was let go.
#[test]
fn pane_byte_pipe_stops_when_it_is_let_go() {
    let case = || -> Result<(), Failed> {
        let held = scratch("detach")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let watched = attach(client, &alias, PANE)?;
        await_watched(watched, "a screen", PROMPT, |kept| !kept.screens.is_empty())?;
        typed(client, &alias, PANE, "echo before-$((6*7))\n")?;
        await_watched(watched, "the pane's bytes", PROMPT, |kept| {
            holds(&kept.output, b"before-42")
        })?;
        let mut error = blank();
        // SAFETY: the client is live and the alias null-terminated.
        let gone = unsafe { iznik_pane_detach(client, alias.as_ptr(), PANE, &raw mut error) };
        assert_eq!(gone, OK, "the pane is let go: {}", said(&error));
        let held_bytes = {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*watched }
                .lock()
                .map_err(|_broken| "the record")?;
            kept.output.len()
        };
        // Whatever the pane says now is nobody's business here.
        typed(client, &alias, PANE, "echo after-$((6*7))\n")?;
        std::thread::sleep(BRIEF);
        {
            // SAFETY: this case's own box, alive here.
            let kept = unsafe { &*watched }
                .lock()
                .map_err(|_broken| "the record")?;
            assert!(
                !holds(&kept.output, b"after-42"),
                "nothing arrives for a pane that was let go"
            );
            assert!(
                kept.output.len() >= held_bytes,
                "and what had arrived is still what arrived"
            );
        }
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once.
        drop(unsafe { Box::from_raw(watched) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When letting a pane go from inside its own handler waits for that handler
/// to finish, or when anything arrives for it afterwards.
#[test]
fn pane_byte_pipe_lets_a_pane_go_from_inside_its_own_handler() {
    let case = || -> Result<(), Failed> {
        let held = scratch("leaving")?;
        let runtime = runtime()?;
        let (stack, client, alias) = connected(&held, &runtime)?;
        let leaving: *mut Leaving = Box::into_raw(Box::new(Leaving {
            client: Reachable(client),
            alias: alias.clone(),
            told: Mutex::new(Left::default()),
        }));
        let handlers = PaneCallbacks {
            output: Some(leaves),
            screen: None,
            mark: None,
            detached: None,
        };
        let mut error = blank();
        // SAFETY: the client is live, the alias null-terminated, and the
        // context outlives the attachment — this case frees it at the end.
        let taken = unsafe {
            iznik_pane_attach(
                client,
                alias.as_ptr(),
                PANE,
                handlers,
                leaving.cast::<c_void>(),
                &raw mut error,
            )
        };
        assert_eq!(taken, OK, "the pane was taken: {}", said(&error));
        typed(client, &alias, PANE, "#hello\n")?;
        // The handler lets the pane go while it is running. Without the rule
        // that a handler waits for nothing, this never answers.
        let expires = Instant::now().checked_add(PROMPT).ok_or("no clock")?;
        while Instant::now() < expires {
            // SAFETY: this case's own box, alive here.
            let answered = unsafe { &*leaving }
                .told
                .lock()
                .ok()
                .and_then(|told| told.answered);
            if answered.is_some() {
                assert_eq!(answered, Some(OK), "and it answers that it let go");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let calls = {
            // SAFETY: this case's own box, alive here.
            let told = unsafe { &*leaving }
                .told
                .lock()
                .map_err(|_broken| "the record")?;
            assert_eq!(told.answered, Some(OK), "letting go answered");
            told.calls
        };
        // And nothing more arrives for a pane that was let go, however much
        // the shell goes on saying.
        typed(client, &alias, PANE, "#more\n")?;
        std::thread::sleep(BRIEF);
        {
            // SAFETY: this case's own box, alive here.
            let told = unsafe { &*leaving }
                .told
                .lock()
                .map_err(|_broken| "the record")?;
            assert_eq!(told.calls, calls, "nothing arrived after it let go");
        }
        // SAFETY: it came from `iznik_client_new` and is freed once.
        unsafe { iznik_client_free(client) };
        // SAFETY: the box this case made, taken back once, after the client
        // that could have called into it is gone.
        drop(unsafe { Box::from_raw(leaving) });
        drop(stack);
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
