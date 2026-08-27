//! The blocking descriptor as async streams, proven against `cat`: bytes are
//! carried whole and identical whatever the chunk boundaries, concurrent writes
//! stay contiguous, a stalled child backs input up to a cap rather than dropping
//! it, the stream ends exactly at the child's last byte, and one blocked pane
//! does not slow another.

use std::process::Command;
use std::time::{Duration, Instant};

use iznik_server::pty::spawn::{Program, SpawnOptions, spawn};
use iznik_server::pty::streams::{InputError, InputHandle, MAXIMUM_PENDING_INPUT_BYTES, streams};
use iznik_testkit::corpus::generated;

/// The seed the generated corpus is drawn from.
const SEED: u64 = 0x1234_5678;

/// How long to let a shell put its terminal into raw mode before writing.
const SETTLE: Duration = Duration::from_millis(250);

/// Spawn options for a `cat` on a raw terminal — no echo, no line editing, so
/// its output is its input byte-for-byte.
fn raw_cat() -> SpawnOptions {
    SpawnOptions {
        program: Program::Command {
            path: "sh".into(),
            arguments: vec!["-c".to_owned(), "stty raw -echo; exec cat".to_owned()],
        },
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
    }
}

/// Writes `data` to an input handle in `chunk`-sized pieces, waiting out a
/// backlog rather than dropping.
async fn write_all(input: &InputHandle, data: &[u8], chunk: usize) {
    for piece in data.chunks(chunk) {
        loop {
            match input.write(piece.to_vec()) {
                Ok(()) => break,
                Err(InputError::Backlog { .. }) => {
                    tokio::time::sleep(Duration::from_millis(1)).await;
                }
                Err(InputError::Closed) => return,
            }
        }
    }
}

/// A `cat` on a raw terminal echoes 16 MiB back byte-for-byte in under three
/// seconds, whatever chunks the stream chose.
///
/// # Panics
///
/// When the bytes read back are not the bytes written.
#[tokio::test(flavor = "multi_thread")]
async fn pty_streams_bytes_are_carried_identically() {
    let process = spawn(&raw_cat()).expect("cat starts");
    let (mut output, input) = streams(&process).expect("streams open");
    tokio::time::sleep(SETTLE).await;
    // Synchronize past the shell's mode-setting preamble: write a marker and
    // drain until cat has echoed it, so what follows is the data alone.
    let marker: Vec<u8> = vec![0, 1, 2, 3, 4, 5, 6, 7];
    input.write(marker.clone()).expect("the marker is written");
    let mut prefix = Vec::new();
    while find_subslice(&prefix, &marker).is_none() {
        match tokio::time::timeout(Duration::from_secs(2), output.next()).await {
            Ok(Some(chunk)) => prefix.extend_from_slice(&chunk),
            _other => break,
        }
    }
    let data = generated(SEED, 16 * 1024 * 1024);
    let sent = data.clone();
    // Keep the original `input` alive through the read: dropping the last handle
    // ends the writer thread, whose descriptor drop signals the child an end of
    // input that it would echo after the data.
    let writer_input = input.clone();
    let writer = tokio::spawn(async move { write_all(&writer_input, &sent, 64 * 1024).await });
    let started = Instant::now();
    let mut received = Vec::with_capacity(data.len());
    while received.len() < data.len() {
        match output.next().await {
            Some(chunk) => received.extend_from_slice(&chunk),
            None => break,
        }
    }
    writer.await.expect("the writer finishes");
    drop(input);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "16 MiB round trip is quick"
    );
    assert_eq!(received.len(), data.len(), "every byte comes back");
    assert!(received == data, "the bytes come back identical");
}

/// One hundred concurrent writers each put a distinct 4 KiB pattern through, and
/// every pattern lands contiguous — none is interleaved with another's.
///
/// # Panics
///
/// When any pattern is split.
#[tokio::test(flavor = "multi_thread")]
async fn pty_streams_concurrent_writes_stay_whole() {
    let process = spawn(&raw_cat()).expect("cat starts");
    let (mut output, input) = streams(&process).expect("streams open");
    tokio::time::sleep(SETTLE).await;
    let writers = 100_usize;
    let pattern = 4 * 1024;
    let mut tasks = Vec::new();
    for index in 0..writers {
        let input = input.clone();
        let byte = u8::try_from(index).unwrap_or_default();
        tasks.push(tokio::spawn(async move {
            let _written = input.write(vec![byte; pattern]);
        }));
    }
    for task in tasks {
        task.await.expect("a writer finishes");
    }
    let total = writers.saturating_mul(pattern);
    let mut received = Vec::with_capacity(total);
    while received.len() < total {
        match output.next().await {
            Some(chunk) => received.extend_from_slice(&chunk),
            None => break,
        }
    }
    let runs = run_lengths(&received);
    assert_eq!(runs.len(), writers, "one run per pattern: {:?}", runs.len());
    assert!(
        runs.iter().all(|&(_byte, length)| length == pattern),
        "every run is a whole pattern"
    );
}

/// The start of the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The runs of equal bytes in a buffer, as `(byte, length)`.
fn run_lengths(bytes: &[u8]) -> Vec<(u8, usize)> {
    let mut runs: Vec<(u8, usize)> = Vec::new();
    for &byte in bytes {
        match runs.last_mut() {
            Some((last, length)) if *last == byte => *length = length.saturating_add(1),
            _ => runs.push((byte, 1)),
        }
    }
    runs
}

/// A stopped child backs input up to the cap and refuses more with `Backlog`,
/// dropping nothing; once it runs again, every accepted byte arrives in order.
///
/// # Panics
///
/// When a byte is dropped or arrives out of order, or the cap is not enforced.
#[tokio::test(flavor = "multi_thread")]
async fn pty_streams_a_stall_backs_up_and_never_drops() {
    let process = spawn(&raw_cat()).expect("cat starts");
    let (mut output, input) = streams(&process).expect("streams open");
    tokio::time::sleep(SETTLE).await;
    stop_signal(process.process_id(), "-STOP");
    let mut accepted: Vec<u8> = Vec::new();
    let mut counter = 0_u8;
    loop {
        let chunk = vec![counter; 64 * 1024];
        match input.write(chunk.clone()) {
            Ok(()) => accepted.extend_from_slice(&chunk),
            Err(InputError::Backlog { pending }) => {
                assert!(pending <= MAXIMUM_PENDING_INPUT_BYTES, "the cap holds");
                break;
            }
            Err(InputError::Closed) => panic!("the child closed unexpectedly"),
        }
        counter = counter.wrapping_add(1);
    }
    assert!(
        !accepted.is_empty(),
        "some input was accepted before the backlog"
    );
    stop_signal(process.process_id(), "-CONT");
    let mut received = Vec::with_capacity(accepted.len());
    let deadline = Instant::now() + Duration::from_secs(5);
    while received.len() < accepted.len() && Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(1), output.next()).await {
            Ok(Some(chunk)) => received.extend_from_slice(&chunk),
            Ok(None) => break,
            Err(_elapsed) => {}
        }
    }
    assert_eq!(received.len(), accepted.len(), "nothing was dropped");
    assert!(received == accepted, "every accepted byte arrived in order");
}

/// Sends a signal to a process by number, through `kill`.
fn stop_signal(process_id: u32, signal: &str) {
    let _status = Command::new("kill")
        .args([signal, &process_id.to_string()])
        .status();
}

/// The stream ends exactly at the child's last byte: `next` yields the output,
/// then `None`, once the child has exited.
///
/// # Panics
///
/// When the output is wrong or the stream does not end.
#[tokio::test(flavor = "multi_thread")]
async fn pty_streams_the_stream_ends_at_the_last_byte() {
    let process = spawn(&SpawnOptions {
        program: Program::Command {
            path: "sh".into(),
            arguments: vec!["-c".to_owned(), "printf hello".to_owned()],
        },
        columns: 80,
        rows: 24,
        working_directory: None,
        terminfo_directory: None,
    })
    .expect("sh starts");
    let (mut output, _input) = streams(&process).expect("streams open");
    let mut received = Vec::new();
    while let Some(chunk) = tokio::time::timeout(Duration::from_secs(5), output.next())
        .await
        .unwrap_or(None)
    {
        received.extend_from_slice(&chunk);
    }
    assert_eq!(String::from_utf8_lossy(&received).trim_end(), "hello");
}

/// A pane whose output nobody reads does not slow another pane: the reading
/// pane's round trip stays quick while the other's reader is stalled.
///
/// # Panics
///
/// When the reading pane's round trip is slow.
#[tokio::test(flavor = "multi_thread")]
async fn pty_streams_a_blocked_pane_does_not_stall_another() {
    let stalled = spawn(&raw_cat()).expect("cat starts");
    let (_stalled_output, stalled_input) = streams(&stalled).expect("streams open");
    let lively = spawn(&raw_cat()).expect("cat starts");
    let (mut lively_output, lively_input) = streams(&lively).expect("streams open");
    tokio::time::sleep(SETTLE).await;
    // Fill the stalled pane's output channel and never read it, so its reader
    // thread and child block; the lively pane must be unaffected.
    let flood = generated(SEED, 4 * 1024 * 1024);
    let _flooded = tokio::spawn(async move { write_all(&stalled_input, &flood, 64 * 1024).await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let started = Instant::now();
    lively_input
        .write(b"ping".to_vec())
        .expect("the write is accepted");
    let mut echoed = Vec::new();
    while !echoed.ends_with(b"ping") {
        match tokio::time::timeout(Duration::from_secs(2), lively_output.next()).await {
            Ok(Some(chunk)) => echoed.extend_from_slice(&chunk),
            Ok(None) | Err(_) => break,
        }
    }
    assert!(echoed.ends_with(b"ping"), "the lively pane answered");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the lively pane stayed quick"
    );
}
