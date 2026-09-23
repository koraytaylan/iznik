//! The number that says which run of the daemon a client is talking to.
//!
//! Pane, tab and session numbers begin again at one with every daemon, so a
//! client that reconnects after a restart holds numbers that now name other
//! things. The registry picks one of these when it is made and a connection
//! announces it in `Hello`; a client that sees it change resumes nothing.
//!
//! It is not a secret and nothing is authenticated with it. What it has to be
//! is different from the last daemon's, which a clock and the operating
//! system's per-process random hashing keys make it with room to spare.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::time::{SystemTime, UNIX_EPOCH};

use iznik_protocol::identity::DaemonInstance;

/// How far the first half is shifted to make room for the second.
const HALF: u32 = u64::BITS;

/// A daemon instance nobody has picked before.
///
/// Two independently keyed hashes of the moment and this process's number,
/// one for each half: the keys are drawn from the operating system's random
/// source once per process, so two daemons started in the same nanosecond
/// with the same process number still differ.
#[must_use]
pub fn fresh_instance() -> DaemonInstance {
    let moment = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let process = std::process::id();
    let half = || {
        let mut hasher = RandomState::new().build_hasher();
        hasher.write_u128(moment);
        hasher.write_u32(process);
        u128::from(hasher.finish())
    };
    let high = half();
    let low = half();
    DaemonInstance(high.checked_shl(HALF).unwrap_or(0) | low)
}
