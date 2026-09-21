# The `iznik/1` wire protocol

What a second implementation needs to speak to this server, and nothing it
does not. The macOS repository reads this document; it does not read the Rust.

**The golden fixtures are the arbiter.** Every layout below names the line of
`crates/iznik-protocol/tests/fixtures/*.jsonl` that pins it, by its
`description` field. Where this document and a fixture disagree, the fixture is
right and this document is a bug. Every discriminant below is checked against
the constant of the same name in the source by
`crates/iznik-protocol/tests/protocol_reference.rs`, which fails if either
side moves without the other.

## 1. Primitives

Every integer is little-endian and unsigned unless it says otherwise. There is
no alignment and no padding: a field begins where the previous one ended.

| Notation | Bytes | Meaning |
|---|---|---|
| `u8`, `u16`, `u32`, `u64` | 1, 2, 4, 8 | A little-endian unsigned integer. |
| `i32` | 4 | A little-endian two's-complement signed integer. |
| `id` | 8 | A `u64` naming a pane, tab, session or command. |
| `generation`, `sequence` | 8 | A `u64`; see §7. |
| `bytes` | 4 + *n* | A `u32` length and that many bytes. A string is its UTF-8 bytes; a non-UTF-8 string is a decoding error, never a lossy conversion. |
| `opt` | 1, or 1 + `bytes` | A presence byte — `0` absent, `1` present — and, when present, a `bytes`. Any other presence byte is an error. |
| `count` | 4 | A `u32` saying how many elements follow. It is never allocated for: a decoder reads as far as the bytes go and then reports truncation. |
| `flag` | 1 | `0` false, `1` true. Any other byte is an error. |

A decoder refuses, rather than guesses:

| Refusal | When |
|---|---|
| Truncated | A field ends before its bytes do. It names the discriminant it was reading, how many bytes it needed and how many were left. |
| TrailingBytes | Bytes follow the last field of a message. A message is exactly its fields. |
| UnknownDiscriminant | A tag no variant claims — a message, a mark kind, an error code, a layout node, a split direction, a delta, a removal reason, an exit status, a command, an outcome, a created thing or a rejection code. |
| Utf8 | A string field is not UTF-8. |
| Oversize | An encoding would exceed the maximum payload length. It is refused before anything is allocated. |
| LayoutTooDeep | A layout nests past `MAXIMUM_LAYOUT_DEPTH`. It is refused on the way down, so a hostile arrangement cannot exhaust the stack. |

A refusal that arises before any discriminant has been read — inside a
`Snapshot`'s model, say, which carries no tag of its own — reports the
discriminant `NO_DISCRIMINANT` (255) rather than pretending to be message 0.

## 2. Limits

| Constant | Value | What it bounds |
|---|---|---|
| `MAXIMUM_PAYLOAD_LENGTH` | 1 048 576 (1 MiB) | The largest frame payload. A longer length on the wire is a protocol error, not an allocation. |
| `HEADER_LENGTH` | 5 | The frame header: 4 length bytes and 1 channel byte. |
| `MAXIMUM_LAYOUT_DEPTH` | 64 | The deepest a layout tree may nest, refused by the decoder, the encoder and the validator alike. |
| `PROTOCOL_VERSION` | 1 | The version this document describes. |
| `CHANNEL_CONTROL` | 0 | The channel control messages travel on. |
| `NO_DISCRIMINANT` | 255 | What a refusal reports when it has no discriminant to name. |

## 3. Framing

A frame is a 4-byte little-endian payload length, a 1-byte channel, and the
payload:

```
+--------+--------+--------+--------+--------+============+
|      length (u32, LE)             |channel |  payload   |
+--------+--------+--------+--------+--------+============+
```

The length counts the payload only; the header is not included. A frame with
an empty payload is five bytes and is legal.

Channel `0` carries the structured control messages of §4 and §5. Channels
`1..=255` carry pane output as **raw bytes**: the payload *is* the terminal
data, and it is never deserialized, re-encoded or copied on the way past. A
reader that treats a pane channel's payload as anything but bytes is wrong.

*Fixtures:* `frame.jsonl` — "an empty payload on channel 1", "a one-byte
payload on channel 1", "a control message on channel 0", and the refusal of a
length past the maximum.

A decoder must resume across any read boundary: a frame may arrive in as many
reads as the network chooses, and two frames may arrive in one.

## 4. Client to server

Each message is one payload on channel 0: a discriminant byte, then its
fields in the order given.

| Message | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Hello` | `server_tag::HELLO` | 0 | `u16` protocol version, `bytes` client version, `u32` capability bits |
| `SnapshotRequest` | `server_tag::SNAPSHOT_REQUEST` | 1 | none |
| `Command` | `server_tag::COMMAND` | 2 | `id` command id, `bytes` payload (§6) |
| `Subscribe` | `server_tag::SUBSCRIBE` | 3 | `id` pane |
| `Unsubscribe` | `server_tag::UNSUBSCRIBE` | 4 | `id` pane |
| `Resume` | `server_tag::RESUME` | 5 | `id` pane, `sequence` first byte the client does not hold |
| `ScreenRequest` | `server_tag::SCREEN_REQUEST` | 6 | `id` pane |
| `Credit` | `server_tag::CREDIT` | 7 | `u8` channel, `u32` bytes consumed |
| `ChannelReleased` | `server_tag::CHANNEL_RELEASED` | 8 | `u8` channel |
| `Input` | `server_tag::INPUT` | 9 | `id` pane, `bytes` input |
| `Resize` | `server_tag::RESIZE` | 10 | `id` pane, `u16` columns, `u16` rows |
| `Focus` | `server_tag::FOCUS` | 11 | `id` pane |
| `Ping` | `server_tag::PING` | 12 | none |

*Fixtures:* `message.jsonl`, "Hello with both known capabilities" through
"Ping", and the refusals from "an empty payload to the server" onward.

The codec does not police channel numbers: a `Credit` or `ChannelReleased`
naming channel 0 decodes to exactly that, and it is the server's connection
loop that refuses it. `message.jsonl` pins this in "Credit naming channel 0,
which the codec does not police".

## 5. Server to client

| Message | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Hello` | `client_tag::HELLO` | 0 | `u16` protocol version, `bytes` server version, `u32` capability bits |
| `Snapshot` | `client_tag::SNAPSHOT` | 1 | `generation`, `bytes` payload (§7) |
| `Delta` | `client_tag::DELTA` | 2 | `generation`, `bytes` payload (§8) |
| `CommandResult` | `client_tag::COMMAND_RESULT` | 3 | `id` command id, `bytes` payload (§6.3) |
| `PaneChannel` | `client_tag::PANE_CHANNEL` | 4 | `id` pane, `u8` channel, `sequence` first byte the channel carries |
| `PaneDetached` | `client_tag::PANE_DETACHED` | 5 | `id` pane, `u8` channel |
| `Screen` | `client_tag::SCREEN` | 6 | `id` pane, `sequence`, `u16` columns, `u16` rows, `bytes` VT bytes |
| `Mark` | `client_tag::MARK` | 7 | `id` pane, `sequence`, mark kind (§5.2) |
| `Pong` | `client_tag::PONG` | 8 | none |
| `Error` | `client_tag::ERROR` | 9 | `u8` error code (§5.1), `bytes` message |

*Fixtures:* `message.jsonl`, "Hello reply with both known capabilities"
through "Error with an empty message".

### 5.1 Error codes

| Code | Constant | Value | Meaning |
|---|---|---|---|
| `ProtocolVersion` | `error_tag::PROTOCOL_VERSION` | 0 | The client's protocol version is not the server's. |
| `InputBacklog` | `error_tag::INPUT_BACKLOG` | 1 | The client sent input faster than the pane consumes it. |
| `UnknownPane` | `error_tag::UNKNOWN_PANE` | 2 | The pane does not exist. |
| `ChannelsExhausted` | `error_tag::CHANNELS_EXHAUSTED` | 3 | No channel number is free. |
| `NotSubscribed` | `error_tag::NOT_SUBSCRIBED` | 4 | The client acted on a pane it is not subscribed to. |

### 5.2 Mark kinds

A mark kind is a discriminant byte and then its fields. Marks are fully typed
on the wire because both ends act on them; a client never re-parses the shell
integration sequences out of the byte stream.

| Kind | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `PromptStart` | `mark_tag::PROMPT_START` | 0 | none |
| `CommandStart` | `mark_tag::COMMAND_START` | 1 | none |
| `CommandExecuted` | `mark_tag::COMMAND_EXECUTED` | 2 | none |
| `CommandFinished` | `mark_tag::COMMAND_FINISHED` | 3 | `flag` presence, then `i32` exit status when present |
| `WorkingDirectory` | `mark_tag::WORKING_DIRECTORY` | 4 | `bytes` path |
| `Title` | `mark_tag::TITLE` | 5 | `bytes` title |
| `AlternateScreen` | `mark_tag::ALTERNATE_SCREEN` | 6 | `flag`: entered, or left |

*Fixtures:* `message.jsonl`, "Mark PromptStart" through "Mark AlternateScreen
left", including both signs and both extremes of an exit status.

## 6. Session commands

The `Command` payload of §4 and the `CommandResult` payload of §5.

### 6.1 Placement

Where a new pane goes: beside one that is already there. The target's leaf is
replaced by a split of the two, weights equal, in the direction given, the new
pane before or after the target.

| Field | Encoding |
|---|---|
| target | `id` |
| direction | `u8` split direction (§7.3) |
| before | `flag` |

### 6.2 Commands

A creating command ends with a size and a working directory, written as
`u16` columns, `u16` rows, `opt` working directory. That trailer is written
`start` below.

| Command | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `CreateSession` | `command_tag::CREATE_SESSION` | 0 | `bytes` name, `start` |
| `RenameSession` | `command_tag::RENAME_SESSION` | 1 | `id` session, `bytes` name |
| `CloseSession` | `command_tag::CLOSE_SESSION` | 2 | `id` session |
| `CreateTab` | `command_tag::CREATE_TAB` | 3 | `id` session, `bytes` name, `start` |
| `RenameTab` | `command_tag::RENAME_TAB` | 4 | `id` tab, `bytes` name |
| `CloseTab` | `command_tag::CLOSE_TAB` | 5 | `id` tab |
| `ReorderTabs` | `command_tag::REORDER_TABS` | 6 | `id` session, `count`, that many `id` tabs |
| `CreatePane` | `command_tag::CREATE_PANE` | 7 | `id` tab, placement (§6.1), `start` |
| `ClosePane` | `command_tag::CLOSE_PANE` | 8 | `id` pane |
| `MovePane` | `command_tag::MOVE_PANE` | 9 | `id` pane, `id` destination tab, placement (§6.1) |
| `SetLayout` | `command_tag::SET_LAYOUT` | 10 | `id` tab, layout (§7.2) |
| `ReorderSessions` | `command_tag::REORDER_SESSIONS` | 11 | `count`, that many `id` sessions |

`ReorderTabs` carries the whole order, never a swap: a swap applied to the
wrong arrangement silently produces a third arrangement nobody has.
`ReorderSessions` carries the whole order of the host's sessions the same way.

*Fixtures:* `command.jsonl`, "create a session, with a working directory"
through "set a tab's layout, carried whole".

### 6.3 Outcomes

Every command is answered exactly once, with one of:

| Outcome | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Applied` | `outcome_tag::APPLIED` | 0 | `generation`, created (below) |
| `Rejected` | `outcome_tag::REJECTED` | 1 | `u8` rejection code, `bytes` message |

| Created | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Nothing` | `created_tag::NOTHING` | 0 | none |
| `Session` | `created_tag::SESSION` | 1 | `id` session |
| `Tab` | `created_tag::TAB` | 2 | `id` tab |
| `Pane` | `created_tag::PANE` | 3 | `id` pane |

| Rejection | Constant | Value |
|---|---|---|
| `UnknownSession` | `rejection_tag::UNKNOWN_SESSION` | 0 |
| `UnknownTab` | `rejection_tag::UNKNOWN_TAB` | 1 |
| `UnknownPane` | `rejection_tag::UNKNOWN_PANE` | 2 |
| `EmptyName` | `rejection_tag::EMPTY_NAME` | 3 |
| `InvalidOrder` | `rejection_tag::INVALID_ORDER` | 4 |
| `InvalidLayout` | `rejection_tag::INVALID_LAYOUT` | 5 |
| `SpawnFailed` | `rejection_tag::SPAWN_FAILED` | 6 |
| `UnknownCommand` | `rejection_tag::UNKNOWN_COMMAND` | 7 |

A `Command` whose tag or payload the server cannot read is answered with
`UnknownCommand` and the connection stays open. Two peers of one protocol
version are not necessarily one build — a released server and a local one both
say "protocol 1" — so an unknown command is a request the server cannot serve,
not a peer speaking garbage, and ending the connection over it would take every
pane on the link with it.

A rejected command changes nothing: the generation is unchanged and no delta
is emitted. *Fixtures:* `command.jsonl`, "applied, creating nothing" through
"rejected with an empty message".

## 7. The host model

The `Snapshot` payload. Sessions hold ordered tabs; a tab holds panes and a
layout tree over exactly those panes.

| Part | Encoding |
|---|---|
| model | `generation`, `count` of sessions, that many sessions |
| session | `id`, `bytes` name, `count` of tabs, that many tabs |
| tab | `id`, `bytes` name, `count` of panes, that many panes, layout |
| pane | `id`, `bytes` title, `opt` working directory, `u16` columns, `u16` rows |

*Fixtures:* `model.jsonl`, "an empty host: no sessions at all" through "the
widest value every field holds, with empty strings the codec carries".

### 7.1 Invariants

A well-formed model — one the decoder accepts — is not necessarily a valid
one. A valid model holds all of:

- Every id is unique across the host: no two sessions, tabs or panes share one.
- Every session holds at least one tab, and every tab at least one pane.
- A tab's layout leaves are exactly its panes, each appearing once.
- Every weight is at least one.
- The layout is normalized (§7.2).
- No name is empty.

A client may check these; the server holds to them. A model that breaks one is
a defect to report, not a reason to drop the connection.

### 7.2 Layout

| Node | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Split` | `layout_tag::SPLIT` | 0 | `u8` direction, `count` of children, then for each child: the child node, then its `u32` weight |
| `Leaf` | `layout_tag::LEAF` | 1 | `id` pane |

A child's weight follows the child, not the other way round, because a child
is itself a node of arbitrary size and the weight is the fixed field.

**Normalized** means all of: a split holds no split of its own direction — a
nested one is lifted into its parent, its children's weights multiplied
through; a split with one child is that child; a split with no children is
dropped from its parent; and a split's weights share no common divisor above
one. The last is what makes an arrangement one tree rather than many: without
it the product a lifting multiplies by accumulates until it saturates, and two
encodings of the same arrangement compare unequal.

The tree says how an arrangement is restored, deliberately not how a cell size
is computed. *Fixtures:* `model.jsonl`, "nested splits in both directions with
unequal weights".

### 7.3 Split direction

| Direction | Constant | Value |
|---|---|---|
| `Horizontal` | `direction_tag::HORIZONTAL` | 0 |
| `Vertical` | `direction_tag::VERTICAL` | 1 |

## 8. Deltas

The `Delta` payload. Each variant is the smallest thing that can happen;
anything larger is several of them, and anything that cannot be expressed as
some of them is a `Snapshot`.

| Delta | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `SessionAdded` | `delta_tag::SESSION_ADDED` | 0 | session (§7) |
| `SessionRenamed` | `delta_tag::SESSION_RENAMED` | 1 | `id` session, `bytes` name |
| `SessionRemoved` | `delta_tag::SESSION_REMOVED` | 2 | `id` session |
| `TabAdded` | `delta_tag::TAB_ADDED` | 3 | `id` session, tab (§7), `count` index |
| `TabRenamed` | `delta_tag::TAB_RENAMED` | 4 | `id` tab, `bytes` name |
| `TabRemoved` | `delta_tag::TAB_REMOVED` | 5 | `id` tab |
| `TabsReordered` | `delta_tag::TABS_REORDERED` | 6 | `id` session, `count`, that many `id` tabs |
| `PaneAdded` | `delta_tag::PANE_ADDED` | 7 | `id` tab, pane (§7) |
| `PaneRemoved` | `delta_tag::PANE_REMOVED` | 8 | `id` pane, removal reason (§8.1) |
| `PaneMoved` | `delta_tag::PANE_MOVED` | 9 | `id` pane, `id` destination tab |
| `LayoutChanged` | `delta_tag::LAYOUT_CHANGED` | 10 | `id` tab, layout (§7.2) |
| `PaneTitle` | `delta_tag::PANE_TITLE` | 11 | `id` pane, `bytes` title |
| `PaneWorkingDirectory` | `delta_tag::PANE_WORKING_DIRECTORY` | 12 | `id` pane, `bytes` path |
| `PaneResized` | `delta_tag::PANE_RESIZED` | 13 | `id` pane, `u16` columns, `u16` rows |
| `SessionsReordered` | `delta_tag::SESSIONS_REORDERED` | 14 | `count`, that many `id` sessions |

An index is a position in an ordered list and is written the width a `count`
is. *Fixtures:* `delta.jsonl`, "a session appears, carrying its first tab and
that tab's first pane" through "a pane is resized to nothing at all".

### 8.1 Removal reason and exit status

| Reason | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Closed` | `reason_tag::CLOSED` | 0 | none |
| `Exited` | `reason_tag::EXITED` | 1 | exit status, below |

| Status | Constant | Value | Fields after the discriminant |
|---|---|---|---|
| `Exited` | `status_tag::EXITED` | 0 | `i32` code the child chose |
| `Signalled` | `status_tag::SIGNALLED` | 1 | `i32` signal number |

A pane's going is not reported until it can be reported truthfully. *Fixtures:*
`delta.jsonl`, "a pane is removed because a client closed it" through "a pane
is removed because its shell was killed by a signal".

### 8.2 Applying them

A delta arrives with the generation it produces, which must be exactly the
client's model's generation plus one. Anything else is a gap, and the client's
answer to a gap is `SnapshotRequest` — never an attempt to reconstruct what it
missed.

Every check runs before the first mutation: a delta that would break an
invariant leaves the model untouched. Layout deltas are normalized on
application, so a client that normalizes as this document describes and a
server that does compare equal.

The model is whole *between changes*, not between the deltas of one change.
One operation may emit several deltas — a pane appearing is `PaneAdded` and
then `LayoutChanged` — and between them the model holds a pane its layout does
not yet place. A client that renders after each delta must tolerate that; a
client that renders after each *frame batch* will not see it.

## 9. Sequence numbers

Every pane has one absolute byte sequence counting from its creation, and it
counts bytes the pane produced, not bytes anyone was sent. A `PaneChannel`
names the position its stream starts at, and the output frames on that channel
are contiguous from there, so a client always knows exactly which byte it
holds: after receiving *n* bytes on the channel, it holds through
`sequence + n`.

A `Screen` and a `Mark` each name the sequence they are exact at, and a
`Screen` that arrives on channel 0 while bytes flow on a pane channel is exact
at a position within that byte stream — the bytes that follow it on the pane
channel begin exactly where the screen ends.

## 10. Subscribing, resuming and the screen

A subscription is started by one of three messages, and the server answers
each by deciding one of two things: continue from a position, or send the
truth and continue from there.

| Request | Answer |
|---|---|
| `Subscribe { pane }` | The truth at the newest byte: a `PaneChannel` naming that sequence, then a `Screen` at it, then bytes from it. |
| `Resume { pane, from_sequence }` | When the ring still holds `from_sequence` — that is, `oldest <= from <= newest` — a `PaneChannel` at exactly `from_sequence` and then bytes, with no `Screen`. Otherwise exactly the `Subscribe` answer. |
| `ScreenRequest { pane }` | The truth at the newest byte, on the channel the pane already has, and the cursor moves there, so no byte is delivered twice. |

`Screen` is the only resynchronization mechanism. There is no desync marker
for a client to interpret: whenever the server cannot deliver contiguous
bytes, it delivers truth instead. A `Screen` is VT bytes — feed them to a
terminal emulator sized to the `columns` and `rows` it carries and it
reproduces the pane, scrollback included.

`Unsubscribe` is answered with `PaneDetached { pane, channel }`, and so is a
subscribed pane whose child exits. **The channel number is not free until the
client sends `ChannelReleased { channel }`**: a frame already in flight when
the server detached the pane would otherwise be read as the output of whatever
pane took the number next. A client that never acknowledges simply runs out of
channels; a client that acknowledges before it has drained the channel
corrupts its own rendering.

## 11. Credit

Each pane channel has a credit window in bytes. The server may send at most
what the window holds; the client returns credit with `Credit { channel,
bytes }` as it consumes.

**A client never needs to know the sizes.** It returns credit for what it has
received and the window follows; the figures below are this server's policy,
not the protocol's, and they live in `crates/iznik-server/src/multiplexer/`
`credit.rs`. They are stated here for orientation, and a client that depends
on them is depending on something it was never told.

| Window | Value | Meaning |
|---|---|---|
| Background | 262 144 (256 KiB) | What an unfocused pane's channel starts with. |
| Focused | 1 048 576 (1 MiB) | What the pane the client is looking at holds. |
| Frame payload | 65 536 (64 KiB) | The most one pane frame carries, so a keystroke echo waits behind at most one frame per active pane. |
| Stale threshold | 4 194 304 (4 MiB) | The lag past which a background channel stops being streamed. |

A window never grows past its ceiling: a client that returns more credit than
it was ever sent is confused, and letting the window grow on its word would
let one pane fill the server's memory. `Focus { pane }` moves the larger
window and first service to that pane; the difference between the two window
sizes is added when focus arrives and subtracted when it leaves, so what is
outstanding at the client plus what may still be sent stays inside the
ceiling.

A channel at zero credit is skipped, never waited on. A background channel
that falls further behind than the stale threshold stops being streamed
altogether; its history is kept, and rather than the megabytes it missed it is
sent a `PaneChannel` at the newest byte and a `Screen` — as soon as it is next
served with credit to spend, and immediately when the client focuses it. A
client must therefore be ready for a `PaneChannel` and a `Screen` on a channel
it is not looking at. It may still `Resume` from an older sequence afterwards:
the ring keeps every byte it holds regardless.

The acceptance figure for all of this is measured, not asserted: with one pane
flooding at line rate and another echoing keystrokes, both subscribed, the
keystroke-to-echo round trip through the multiplexer stays under **25 ms at
the 99th percentile** over a thousand samples.

## 12. Handshake and compression

Both ends send `Hello` first. The protocol version is a value the handshake
refuses on mismatch; the codec never guesses across versions.

Capabilities are a `u32` bit set:

| Capability | Bit | Value |
|---|---|---|
| `ZSTD` | 0 | 1 |
| `RESUME` | 1 | 2 |
| `REORDER_SESSIONS` | 2 | 4 |

Unknown bits are preserved rather than dropped, so a newer peer round-trips
its own advertisement intact and can tell what it advertised from what came
back. *Fixtures:* `message.jsonl`, "Hello with protocol version 2 and only an
unknown capability bit, preserved".

`REORDER_SESSIONS` is a capability rather than something implied by the
protocol version because the two ends are upgraded separately, and a remote's
server is only ever replaced on purpose: a client must not send
`ReorderSessions` to a server built before the command existed, because that
server refuses the unknown tag as garbage and ends the whole connection on it.
A client sends the command only to a server that advertised this bit.

When **both** `Hello`s carried `ZSTD`, everything after them is a single zstd
stream in each direction — one context per connection, not per frame, so the
compression window spans frames and a keystroke echo is not inflated by a
fresh header. Both contexts are primed with a dictionary trained on a
committed corpus of real terminal output, so the first kilobytes of a session
are not the expensive ones. Each frame is flushed as it is written: nothing
waits in a compressor's buffer for the next frame that may never come.

**The leftover-bytes rule.** A plain reader reads in blocks, so by the time it
has parsed the peer's `Hello` it has almost certainly read some of the peer's
*first compressed bytes* as well. Those bytes have not been consumed and are
not on the socket any more. They must be handed to the compressed stream as
its first input; a implementation that discards them will fail to decode the
first frame after the handshake, and will do so intermittently, according to
how the block boundary fell. There is no framing marker to recover from this:
the leftover is the start of the zstd stream.

Two committed numbers decide whether the capability stays advertised, both
measured against the fidelity corpus:

- **Ratio.** At least 3.0 as data. The corpus is 316 bytes of payload across
  seventeen constructs and the 378-byte dictionary takes it to 101 bytes, a
  ratio of 3.129. As the link actually frames it — seventeen frames with a
  flush after each, which is what the latency number buys — it costs 299 bytes
  against 411 uncompressed, a saving of 1.375. Both figures are asserted
  together so neither can be quoted alone.
- **Latency.** At most 1 ms added at the 99th percentile for a one-byte frame
  in process. Measured: 740 ns.

## 13. Geometry

The client owns size. It sends `Resize`, the server sets the pseudoterminal
and its mirror, and the resulting `PaneResized` delta is what every client —
including the sender — renders. When two clients look at one pane the last
`Resize` wins and both observe it.
