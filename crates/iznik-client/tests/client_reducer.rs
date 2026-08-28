//! The server's messages applied to the client's model.
//!
//! The property that matters is convergence, and it is inherited rather than
//! rebuilt: the reducer calls the protocol's own reconciler, which plan 0003
//! proved against models the generator built directly. So the case here is
//! that a *client* — one that decodes what came over a wire, routes it by host
//! and keeps its own subscriptions beside it — arrives at the same place, over
//! a thousand generated sequences, in well under the five seconds section 8
//! calls the limit of a case worth running.

use core::time::Duration;
use std::time::Instant;

use iznik_client::host::identity::HostId;
use iznik_client::model::{ClientModel, HostView};
use iznik_client::reduce::{Effect, Notification, arrived, reduce};
use iznik_protocol::command::{CommandOutcome, Created, encode_command_outcome};
use iznik_protocol::delta::{Delta, encode_delta};
use iznik_protocol::identity::{CommandId, Generation, PaneId, Sequence, SessionId};
use iznik_protocol::message::{MarkKind, ToClient};
use iznik_protocol::model::{HostModel, encode_host_model};
use iznik_testkit::generate::ModelGenerator;

/// How many generated sequences the convergence case walks.
const SEQUENCES: usize = 1_000;

/// How many changes each carries.
const CHANGES: usize = 5;

/// The seed the generator starts from, so a failure is reproducible.
const SEED: u64 = 0x2026_0828_2117_0005;

/// The longest the convergence case may take. Section 8 calls five seconds a
/// review rejection; this is the bound, not the expectation.
const CONVERGENCE_CEILING: Duration = Duration::from_secs(5);

/// The channel a pane's bytes arrive on.
const CHANNEL: u8 = 3;

/// Another, for the pane that is not being looked at.
const OTHER_CHANNEL: u8 = 4;

/// Where a subscription starts.
const FROM: Sequence = Sequence(1_000);

/// How many bytes arrive at once.
const ARRIVED: usize = 512;

/// The pane these cases subscribe to.
const PANE: PaneId = PaneId(1);

/// Anything a case can fail on.
type Failed = Box<dyn std::error::Error>;

/// The host most of these cases talk to.
fn work() -> HostId {
    HostId("work".to_owned())
}

/// Another, for the routing case.
fn build() -> HostId {
    HostId("build".to_owned())
}

/// A model with one host known, at `model`.
fn knowing(host: &HostId, model: HostModel) -> ClientModel {
    let mut held = ClientModel::default();
    let _first = held.insert(host.clone(), HostView::of(model));
    held
}

/// The snapshot message that carries `model`.
///
/// # Errors
///
/// When the model cannot be encoded.
fn snapshot(model: &HostModel) -> Result<ToClient, Failed> {
    Ok(ToClient::Snapshot {
        generation: model.generation,
        payload: encode_host_model(model)?,
    })
}

/// The delta message that carries `delta` as the model's next.
///
/// # Errors
///
/// When the delta cannot be encoded.
fn numbered(at: Generation, delta: &Delta) -> Result<ToClient, Failed> {
    Ok(ToClient::Delta {
        generation: Generation(at.0.saturating_add(1)),
        payload: encode_delta(delta)?,
    })
}

/// The model one host holds.
///
/// # Errors
///
/// When the client does not know that host.
fn held(model: &ClientModel, host: &HostId) -> Result<HostModel, Failed> {
    model
        .host(host)
        .map(|view| view.model.clone())
        .ok_or_else(|| format!("{host} is not known").into())
}

/// # Panics
///
/// When a client fed a snapshot and its deltas does not arrive at the model
/// the generator built, or takes longer than a case worth running may.
#[test]
fn client_reducer_converges_on_the_model_the_host_holds() {
    let case = || -> Result<(), Failed> {
        let started = Instant::now();
        let mut generator = ModelGenerator::new(SEED);
        let host = work();
        for index in 0..SEQUENCES {
            let start = generator.model();
            let sequence = generator.changes(&start, CHANGES);
            let mut model = ClientModel::default();
            // A client that knows nothing is made to know this host by the
            // snapshot itself, which is what a first connection is.
            let _made = reduce(&mut model, &host, &snapshot(&sequence.start)?);
            let halfway = sequence.changes.len().checked_div(2).unwrap_or(0);
            for (at, change) in sequence.changes.iter().enumerate() {
                for delta in change {
                    let generation = held(&model, &host)?.generation;
                    let taken = reduce(&mut model, &host, &numbered(generation, delta)?);
                    assert!(
                        taken.is_empty(),
                        "sequence {index}: a delta that fits asks for nothing: {taken:?}"
                    );
                }
                // Halfway through, the host sends the whole model instead —
                // which a reconnection does — and the rest of the deltas must
                // carry on from it.
                if at == halfway {
                    let standing = held(&model, &host)?;
                    let _replaced = reduce(&mut model, &host, &snapshot(&standing)?);
                }
            }
            assert_eq!(
                held(&model, &host)?,
                sequence.finish,
                "sequence {index}: the client did not arrive where the host is"
            );
        }
        let taken = started.elapsed();
        assert!(
            taken < CONVERGENCE_CEILING,
            "{SEQUENCES} sequences in {taken:?}, which is past {CONVERGENCE_CEILING:?}"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a missed number is guessed at rather than asked about.
#[test]
fn client_reducer_asks_for_a_snapshot_when_it_misses_a_number() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let start = generator.model();
        let sequence = generator.changes(&start, CHANGES);
        let host = work();
        let mut model = knowing(&host, sequence.start.clone());
        let Some(delta) = sequence.deltas().first().cloned() else {
            return Err("the generator made no deltas".into());
        };
        // One past the next: a change this client never saw came first.
        let skipped = Generation(sequence.start.generation.0.saturating_add(1));
        let taken = reduce(&mut model, &host, &numbered(skipped, &delta)?);
        assert_eq!(
            taken,
            vec![Effect::RequestSnapshot],
            "a gap asks for the whole model"
        );
        assert_eq!(
            held(&model, &host)?,
            sequence.start,
            "and nothing is guessed at: the model is exactly what it was"
        );
        // And the snapshot that follows settles it.
        let _replaced = reduce(&mut model, &host, &snapshot(&sequence.finish)?);
        assert_eq!(
            held(&model, &host)?,
            sequence.finish,
            "the snapshot resolves the gap"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a subscription does not follow the host's announcements, or its cursor
/// does not follow the bytes.
#[test]
fn client_reducer_follows_a_pane_from_its_channel_to_its_screen() {
    let case = || -> Result<(), Failed> {
        let host = work();
        let mut generator = ModelGenerator::new(SEED);
        let mut model = knowing(&host, generator.model());
        let opened = reduce(
            &mut model,
            &host,
            &ToClient::PaneChannel {
                pane: PANE,
                channel: CHANNEL,
                sequence: FROM,
            },
        );
        assert!(opened.is_empty(), "an announcement asks for nothing");
        let Some(view) = model.host(&host) else {
            return Err("the host is known".into());
        };
        assert_eq!(
            view.subscription(PANE)
                .map(|held| (held.channel, held.cursor)),
            Some((CHANNEL, FROM)),
            "the subscription is open on the channel the host named, at the byte it named"
        );
        // Bytes are not a message: they arrive raw on the pane's own channel,
        // and only how many of them there were matters to the model.
        let _moved = arrived(&mut model, &host, CHANNEL, ARRIVED);
        let _elsewhere = arrived(&mut model, &host, OTHER_CHANNEL, ARRIVED);
        let cursor = model
            .host(&host)
            .and_then(|known| known.subscription(PANE))
            .map(|held| held.cursor);
        assert_eq!(
            cursor,
            Some(Sequence(FROM.0 + u64::try_from(ARRIVED)?)),
            "the cursor moved by exactly what arrived on its own channel, and not by what \
             arrived on another"
        );
        // A screen puts it back where the server says, and says so.
        let redrawn = reduce(
            &mut model,
            &host,
            &ToClient::Screen {
                pane: PANE,
                sequence: FROM,
                columns: 80,
                rows: 24,
                bytes: Vec::new(),
            },
        );
        assert_eq!(
            redrawn,
            vec![Effect::Screen {
                pane: PANE,
                sequence: FROM,
            }],
            "a screen is passed on with the byte it is exact at"
        );
        assert_eq!(
            model
                .host(&host)
                .and_then(|known| known.subscription(PANE))
                .map(|held| held.cursor),
            Some(FROM),
            "and the cursor stands where the screen does"
        );
        // And when the host stops sending, the channel goes back.
        let released = reduce(
            &mut model,
            &host,
            &ToClient::PaneDetached {
                pane: PANE,
                channel: CHANNEL,
            },
        );
        assert_eq!(
            released,
            vec![Effect::ReleaseChannel { channel: CHANNEL }],
            "the number may be used again"
        );
        assert_eq!(
            model.host(&host).and_then(|known| known.subscription(PANE)),
            None,
            "and the subscription is gone"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When a message for one host touches another host's view.
#[test]
fn client_reducer_routes_by_host_before_anything_else() {
    let case = || -> Result<(), Failed> {
        let mut generator = ModelGenerator::new(SEED);
        let start = generator.model();
        let sequence = generator.changes(&start, CHANGES);
        let mut model = knowing(&work(), sequence.start.clone());
        let _second = model.insert(build(), HostView::of(sequence.start.clone()));
        let untouched = model.host(&build()).cloned();
        for delta in sequence.deltas() {
            let generation = held(&model, &work())?.generation;
            let _taken = reduce(&mut model, &work(), &numbered(generation, &delta)?);
        }
        let _opened = reduce(
            &mut model,
            &work(),
            &ToClient::PaneChannel {
                pane: PANE,
                channel: CHANNEL,
                sequence: FROM,
            },
        );
        let _moved = arrived(&mut model, &work(), CHANNEL, ARRIVED);
        assert_eq!(
            model.host(&build()).cloned(),
            untouched,
            "everything that happened to one host left the other exactly as it was"
        );
        assert_ne!(
            held(&model, &work())?,
            sequence.start,
            "and the host it was addressed to did change"
        );
        // A host this client has never heard of is not made known by a delta.
        let stranger = HostId("stranger".to_owned());
        let taken = reduce(
            &mut model,
            &stranger,
            &ToClient::PaneDetached {
                pane: PANE,
                channel: CHANNEL,
            },
        );
        assert!(taken.is_empty(), "and a stranger's message does nothing");
        assert_eq!(model.host(&stranger), None, "and makes no view");
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When an answered command or a shell event does not reach whoever is
/// watching, carrying the host and the identity.
#[test]
fn client_reducer_notifies_about_commands_and_marks() {
    let case = || -> Result<(), Failed> {
        let host = work();
        let mut generator = ModelGenerator::new(SEED);
        let mut model = knowing(&host, generator.model());
        let outcome = CommandOutcome::Applied {
            generation: Generation(2),
            created: Created::Session(SessionId(1)),
        };
        let answered = reduce(
            &mut model,
            &host,
            &ToClient::CommandResult {
                command_id: CommandId(7),
                payload: encode_command_outcome(&outcome)?,
            },
        );
        assert_eq!(
            answered,
            vec![Effect::Notify(Notification::CommandFinished {
                host: host.clone(),
                command: CommandId(7),
                outcome,
            })],
            "the answer carries the host and this client's number for the command"
        );
        let kind = MarkKind::CommandFinished {
            exit_status: Some(0),
        };
        let marked = reduce(
            &mut model,
            &host,
            &ToClient::Mark {
                pane: PANE,
                sequence: FROM,
                kind: kind.clone(),
            },
        );
        assert_eq!(
            marked,
            vec![Effect::Notify(Notification::Mark {
                host,
                pane: PANE,
                sequence: FROM,
                kind,
            })],
            "and a shell event carries the host, the pane and where in its stream it sits"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}

/// # Panics
///
/// When something the host said that this cannot read is passed over in
/// silence, or leaves the model half-changed.
#[test]
fn client_reducer_says_when_it_cannot_read_what_arrived() {
    let case = || -> Result<(), Failed> {
        let host = work();
        let mut generator = ModelGenerator::new(SEED);
        let start = generator.model();
        let mut model = knowing(&host, start.clone());
        let generation = Generation(start.generation.0.saturating_add(1));
        let taken = reduce(
            &mut model,
            &host,
            &ToClient::Delta {
                generation,
                payload: vec![0xff, 0xff, 0xff],
            },
        );
        let Some(Effect::Notify(Notification::Malformed { host: named, .. })) = taken.first()
        else {
            return Err(format!("a change that could not be read said nothing: {taken:?}").into());
        };
        assert_eq!(named, &host, "the complaint names the host");
        assert!(
            taken.contains(&Effect::RequestSnapshot),
            "and asks for the whole model, which is the one recovery that cannot be wrong"
        );
        assert_eq!(
            held(&model, &host)?,
            start,
            "and the model is exactly what it was"
        );
        Ok(())
    };
    case().unwrap_or_else(|error| panic!("{error}"));
}
