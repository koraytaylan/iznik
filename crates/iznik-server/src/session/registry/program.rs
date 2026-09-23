//! Sampling the foreground program of each pane and publishing it as the pane
//! title and directory, until a shell reports its own directory or a program
//! sets a title of its own.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use iznik_protocol::delta::Delta;
use iznik_protocol::identity::PaneId;
use iznik_protocol::model::HostModel;
use iznik_protocol::program::next_title;
use tokio::sync::Notify;

use crate::pane::Pane;
use crate::pty::program::{ProgramSample, read_programs};

use super::Registry;

/// The panes being sampled, and how to stop the task that samples them.
#[derive(Debug)]
pub(super) struct ProgramWatch {
    /// One entry per live pane.
    entries: Arc<Mutex<BTreeMap<PaneId, ProgramEntry>>>,
    /// Wakes the task to sample immediately, so a new pane is not waiting out
    /// the whole interval.
    wake: Arc<Notify>,
    /// Ends the task when the registry is dropped.
    stop: Arc<Notify>,
    /// Cleared on drop, so a sample already in flight does not start another.
    running: Arc<AtomicBool>,
    /// How many clients are attached. Nobody sees a tab's name while none
    /// is, and on a host without `/proc` every sample forks `ps` and `lsof`,
    /// so a daemon nobody is looking at samples nothing.
    clients: Arc<AtomicUsize>,
}

/// One attached client, counted while it lives: while any is, the sampler
/// runs. Dropping it releases the count.
#[derive(Debug)]
pub struct ClientAttachment {
    /// The count it belongs to.
    clients: Arc<AtomicUsize>,
}

impl Drop for ClientAttachment {
    fn drop(&mut self) {
        let _before = self.clients.fetch_sub(1, Ordering::AcqRel);
    }
}

/// What the sampler knows about one pane.
#[derive(Debug)]
struct ProgramEntry {
    /// The pane, for its foreground process.
    pane: Arc<Pane>,
    /// The newest sample, when one has been read.
    latest: Option<ProgramSample>,
    /// Whether [`Registry::apply_programs`] has not yet published `latest`.
    fresh: bool,
    /// The title sampling last published, so a later sample can replace it
    /// and leave a title the program set itself.
    written_title: Option<String>,
    /// The shell has reported a directory, which sampling must not replace:
    /// a shell inside the pane can name a directory the host cannot see.
    shell_directory: bool,
}

/// A sample copied out of the table so it can be published without holding
/// the table lock.
struct PendingSample {
    /// The pane it belongs to.
    pane: PaneId,
    /// What was read.
    sample: ProgramSample,
    /// The title sampling had published before this sample.
    written_title: Option<String>,
    /// Whether the shell owns the directory.
    shell_directory: bool,
}

/// The process a sample was asked about, paired with its pane.
struct Asked {
    /// The pane.
    pane: PaneId,
    /// The foreground process that was read.
    process_id: u32,
}

impl ProgramWatch {
    /// Starts the sampler. The registry's signal is raised when a sample
    /// changes, which is when [`Registry::ingest`] publishes it.
    ///
    /// # Panics
    ///
    /// When called outside a Tokio runtime: the sampler is a task.
    pub(super) fn start(interval: Duration, signal: Arc<Notify>) -> ProgramWatch {
        let entries = Arc::new(Mutex::new(BTreeMap::new()));
        let wake = Arc::new(Notify::new());
        let stop = Arc::new(Notify::new());
        let running = Arc::new(AtomicBool::new(true));
        let watched = Arc::clone(&entries);
        let wake_now = Arc::clone(&wake);
        let stop_now = Arc::clone(&stop);
        let still_running = Arc::clone(&running);
        let clients = Arc::new(AtomicUsize::new(0));
        let looking = SampleLoop {
            interval,
            signal,
            stop: stop_now,
            wake: wake_now,
            running: still_running,
            clients: Arc::clone(&clients),
        };
        tokio::spawn(async move {
            run(watched, looking).await;
        });
        ProgramWatch {
            entries,
            wake,
            stop,
            running,
            clients,
        }
    }

    /// Counts a client in, and samples at once rather than after an interval
    /// of names that were not being kept up to date.
    pub(super) fn attach(&self) -> ClientAttachment {
        let _before = self.clients.fetch_add(1, Ordering::AcqRel);
        self.wake.notify_one();
        ClientAttachment {
            clients: Arc::clone(&self.clients),
        }
    }

    /// Samples `pane` from now on.
    pub(super) fn watch(&self, pane: PaneId, process: Arc<Pane>) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        entries.insert(
            pane,
            ProgramEntry {
                pane: process,
                latest: None,
                fresh: false,
                written_title: None,
                shell_directory: false,
            },
        );
        drop(entries);
        self.wake.notify_one();
    }

    /// Stops sampling `pane`.
    pub(super) fn forget(&self, pane: PaneId) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        let _removed = entries.remove(&pane);
    }

    /// The shell reported a directory, so later samples leave it alone.
    pub(super) fn note_shell_directory(&self, pane: PaneId) {
        let mut entries = self.entries.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(entry) = entries.get_mut(&pane) {
            entry.shell_directory = true;
        }
    }

    /// Copies fresh samples out and marks them taken.
    fn take_pending(&self) -> Vec<PendingSample> {
        pending_samples(&self.entries)
    }
}

impl Drop for ProgramWatch {
    /// Stops the sampler. A sample already in flight finishes, and then the
    /// task drops the panes it still holds.
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        self.stop.notify_waiters();
    }
}

impl Registry {
    /// Publishes fresh program samples as title and directory deltas.
    pub(super) fn apply_programs(&mut self) {
        let pending = self
            .programs
            .as_ref()
            .map(ProgramWatch::take_pending)
            .unwrap_or_default();
        for sample in pending {
            publish(self, sample);
        }
    }
}

/// What the sampling task runs by.
struct SampleLoop {
    /// How often it samples.
    interval: Duration,
    /// Raised when a sample changed.
    signal: Arc<Notify>,
    /// Ends it.
    stop: Arc<Notify>,
    /// Makes it sample at once.
    wake: Arc<Notify>,
    /// Cleared when the watch is dropped.
    running: Arc<AtomicBool>,
    /// How many clients are attached; with none it samples nothing.
    clients: Arc<AtomicUsize>,
}

/// Samples until [`ProgramWatch`] is dropped, while any client is attached.
async fn run(entries: Arc<Mutex<BTreeMap<PaneId, ProgramEntry>>>, looking: SampleLoop) {
    while looking.running.load(Ordering::Acquire) {
        let attached = looking.clients.load(Ordering::Acquire) > 0;
        let asked = if attached {
            asked_of(&entries)
        } else {
            Vec::new()
        };
        if !asked.is_empty() {
            let process_ids: Vec<u32> = asked.iter().map(|one| one.process_id).collect();
            let samples = read_programs(&process_ids).await;
            if record_samples(&entries, &asked, &samples) {
                looking.signal.notify_one();
            }
        }
        // With nobody attached there is no interval to keep: the next client
        // to attach wakes it.
        tokio::select! {
            biased;
            () = looking.stop.notified() => break,
            () = looking.wake.notified() => {}
            () = tokio::time::sleep(looking.interval), if attached => {}
        }
    }
}

/// The foreground process of every pane still being watched.
fn asked_of(entries: &Mutex<BTreeMap<PaneId, ProgramEntry>>) -> Vec<Asked> {
    let entries = entries.lock().unwrap_or_else(PoisonError::into_inner);
    entries
        .iter()
        .map(|(pane, entry)| Asked {
            pane: *pane,
            process_id: entry.pane.foreground_process_id(),
        })
        .collect()
}

/// Records samples that differ from the last one and says whether any did.
fn record_samples(
    entries: &Mutex<BTreeMap<PaneId, ProgramEntry>>,
    asked: &[Asked],
    samples: &BTreeMap<u32, ProgramSample>,
) -> bool {
    let mut entries = entries.lock().unwrap_or_else(PoisonError::into_inner);
    let mut changed = false;
    for one in asked {
        let Some(sample) = samples.get(&one.process_id) else {
            continue;
        };
        let Some(entry) = entries.get_mut(&one.pane) else {
            continue;
        };
        if entry.latest.as_ref() == Some(sample) {
            continue;
        }
        entry.latest = Some(sample.clone());
        entry.fresh = true;
        changed = true;
    }
    changed
}

/// Copies fresh samples out and marks them taken.
fn pending_samples(entries: &Mutex<BTreeMap<PaneId, ProgramEntry>>) -> Vec<PendingSample> {
    let mut entries = entries.lock().unwrap_or_else(PoisonError::into_inner);
    entries
        .iter_mut()
        .filter(|(_, entry)| entry.fresh)
        .map(|(pane, entry)| {
            entry.fresh = false;
            PendingSample {
                pane: *pane,
                sample: entry.latest.clone().unwrap_or(ProgramSample {
                    program: String::new(),
                    directory: None,
                }),
                written_title: entry.written_title.clone(),
                shell_directory: entry.shell_directory,
            }
        })
        .collect()
}

/// Publishes one sample, recording the title it wrote.
fn publish(registry: &mut Registry, pending: PendingSample) {
    let Some((current_title, current_directory)) = pane_text(&registry.model, pending.pane) else {
        return;
    };
    let mut written = pending.written_title;
    if let Some(title) = next_title(&current_title, written.as_deref(), &pending.sample.program) {
        registry.announce(Delta::PaneTitle {
            pane: pending.pane,
            title: title.clone(),
        });
        written = Some(title);
    }
    if !pending.shell_directory
        && let Some(directory) = pending.sample.directory
        && current_directory.as_deref() != Some(directory.as_str())
    {
        registry.announce(Delta::PaneWorkingDirectory {
            pane: pending.pane,
            path: directory,
        });
    }
    keep_title(registry.programs.as_ref(), pending.pane, written);
}

/// The title and directory the model holds for `pane`.
fn pane_text(model: &HostModel, pane: PaneId) -> Option<(String, Option<String>)> {
    model
        .sessions
        .iter()
        .flat_map(|session| session.tabs.iter())
        .flat_map(|tab| tab.panes.iter())
        .find(|held| held.id == pane)
        .map(|held| (held.title.clone(), held.working_directory.clone()))
}

/// Records the title sampling published, when the pane is still watched.
fn keep_title(programs: Option<&ProgramWatch>, pane: PaneId, written: Option<String>) {
    let Some(programs) = programs else {
        return;
    };
    let mut entries = programs
        .entries
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if let Some(entry) = entries.get_mut(&pane) {
        entry.written_title = written;
    }
}
