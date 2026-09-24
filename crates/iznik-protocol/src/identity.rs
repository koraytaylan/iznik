//! The newtypes every message uses: panes, tabs, sessions and commands are
//! numbered, models are numbered by generation, pane output by sequence. A
//! number of one kind never stands for another.

/// A pane, numbered by the host for its lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PaneId(pub u64);

/// A tab, numbered by the host for its lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TabId(pub u64);

/// A session, numbered by the host for its lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SessionId(pub u64);

/// A command a client sent, numbered by that client so the result can be
/// matched to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CommandId(pub u64);

/// A version of the host model: every change advances it by one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(pub u64);

/// A byte position in a pane's output, counted from the pane's creation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Sequence(pub u64);

/// One run of a host's daemon, chosen at random when it starts.
///
/// Pane, tab and session numbers begin again with every daemon, so a number a
/// client holds names a pane only together with the daemon that minted it. A
/// client compares this across connections: the same value is the same daemon
/// and a resume can carry on, a different one is a host whose numbers mean
/// something else now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DaemonInstance(pub u128);

/// The SHA-256 of the server binary a daemon was started from, as the
/// bootstrap that installed it wrote it beside the binary.
///
/// Every build of one version gives the same version, so this is what tells
/// two builds apart: a client compares it with the digest of the server it
/// carries to know whether the daemon answering is its own build.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildDigest(pub [u8; BUILD_DIGEST_LENGTH]);

/// How many bytes a [`BuildDigest`] is: a SHA-256.
pub const BUILD_DIGEST_LENGTH: usize = 32;

/// One client's hold on a host, chosen at random by the client when it
/// starts holding it.
///
/// Command numbers are the client's own, so a number names a command only
/// together with the client that sent it. A client names itself with
/// `Identify` on every connection, and a server that remembers what it
/// answered that client can answer a command sent again after a dropped link
/// rather than apply it twice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClientIdentity(pub u128);
