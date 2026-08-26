//! The metrics from `/proc`: resident memory grows with a touched
//! allocation, CPU time grows with a spinning child, and an unknown process
//! is an error naming it.

use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use iznik_testkit::metrics::{MetricsError, cpu_time, resident_memory};

/// The allocation the memory test touches: 64 MiB.
const ALLOCATION: usize = 64 * 1024 * 1024;

/// The stride that touches every page of the allocation.
const PAGE: usize = 4096;

/// How long the CPU test lets a child spin.
const SPIN: Duration = Duration::from_millis(100);

/// The least CPU time the spinning child must show.
const SPIN_FLOOR: Duration = Duration::from_millis(50);

/// `resident_memory` of this process grows by at least the size of a 64 MiB
/// allocation that is touched.
///
/// # Panics
///
/// When the growth is smaller than the allocation, or the metric fails.
#[test]
fn metrics_resident_memory_grows_with_a_touched_allocation() {
    let process_id = std::process::id();
    let before = resident_memory(process_id).expect("this process is known");
    let mut block = vec![0_u8; ALLOCATION];
    for offset in (0..ALLOCATION).step_by(PAGE) {
        block[offset] = 1;
    }
    let after = resident_memory(process_id).expect("this process is known");
    std::hint::black_box(&block);
    let growth = after.saturating_sub(before);
    assert!(
        growth >= u64::try_from(ALLOCATION).expect("fits"),
        "resident memory grew by {growth} bytes, less than the {ALLOCATION} touched"
    );
}

/// `cpu_time` of a child spinning for 100 milliseconds reports at least 50.
///
/// # Panics
///
/// When the child cannot be spawned, or its CPU time is below the floor.
#[test]
fn metrics_cpu_time_grows_with_a_spinning_child() {
    let mut child = Command::new("yes")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("yes spawns");
    thread::sleep(SPIN);
    let spent = cpu_time(child.id());
    child.kill().expect("yes is killed");
    child.wait().expect("yes is reaped");
    let spent = spent.expect("the child is known");
    assert!(spent >= SPIN_FLOOR, "the child spent {spent:?} of CPU");
}

/// An unknown process id is a `MetricsError` naming it.
///
/// # Panics
///
/// When either metric succeeds for the id, or the error does not name it.
#[test]
fn metrics_unknown_process_is_an_error_naming_it() {
    let unknown = u32::MAX;
    let memory = resident_memory(unknown).expect_err("no such process");
    assert!(matches!(memory, MetricsError::Read { process_id, .. } if process_id == unknown));
    assert!(
        memory.to_string().contains(&unknown.to_string()),
        "{memory}"
    );
    let time = cpu_time(unknown).expect_err("no such process");
    assert!(time.to_string().contains(&unknown.to_string()), "{time}");
}
