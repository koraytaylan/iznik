//! The ring copied for an adoption names the same bytes the sequences do, even
//! while the child is still writing.

use std::time::{Duration, Instant};

use iznik_server::pane::Pane;
use iznik_server::pty::spawn::{Program, SpawnOptions};
use iznik_server::terminal::mirror::MirrorThread;

/// How much history the pane keeps, small enough that the child fills it.
const HISTORY_BYTES: usize = 64;

/// How long a step that should happen at once may take.
const PROMPT: Duration = Duration::from_secs(10);

/// How long between looks.
const POLL: Duration = Duration::from_millis(20);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// # Panics
///
/// When a copy reports a range it does not contain, or a range it does contain
/// comes back empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_carried_ring_matches_the_sequences_while_the_child_writes() {
    let case = async {
        let mirrors = MirrorThread::start().map_err(|error| error.to_string())?;
        let pane = Pane::spawn(
            &SpawnOptions {
                program: Program::LoginShell,
                columns: 80,
                rows: 24,
                working_directory: None,
                terminfo_directory: None,
                agent_socket: None,
            },
            HISTORY_BYTES,
            &mirrors,
        )
        .await?;
        pane.input(b"while true; do printf x; done\n".to_vec())?;
        let started = Instant::now();
        let mut saw_bytes = false;
        while started.elapsed() < PROMPT {
            let carried = pane.carried_ring().await?;
            let length = u64::try_from(carried.bytes.len()).map_err(|error| error.to_string())?;
            let span = carried.newest.0.saturating_sub(carried.oldest.0);
            if span != length {
                return Err(format!(
                    "the copy spans {span} from {:?} to {:?} but holds {length}",
                    carried.oldest, carried.newest
                )
                .into());
            }
            if span > 0 && carried.bytes.is_empty() {
                return Err("a non-empty range was copied as nothing".into());
            }
            let capacity = u64::try_from(HISTORY_BYTES).map_err(|error| error.to_string())?;
            if carried.newest.0 > capacity && length > 0 {
                if length > capacity {
                    return Err(
                        format!("the copy holds {length}, past the ring's {capacity}").into(),
                    );
                }
                saw_bytes = true;
                break;
            }
            pane.resume_output();
            tokio::time::sleep(POLL).await;
        }
        pane.resume_output();
        if !saw_bytes {
            return Err("the child never filled the ring".into());
        }
        Ok::<(), Failed>(())
    };
    case.await.unwrap_or_else(|error| panic!("{error}"));
}
