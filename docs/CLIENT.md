# The iznik client contract

This is what an application is built against. It is written for the GPUI
application and for somebody
holding `include/iznik.h` and a compiler, who cannot read the Rust in this
repository and cannot ask it questions.

**Where this document and the implementation disagree, this document wins and
the implementation is the bug.** Report it as one.

Everything here is about `libiznik` — the static archive or the shared
library — reached through `include/iznik.h`. The header is generated from the
implementation and committed, so it and the implementation cannot drift; this
document is checked against the header by a test that fails if either names
something the other does not, or states an obligation the other does not.

## What iznik is, from outside

A client engine. It holds hosts, each reached over SSH or over a socket on
this machine; it installs and starts a server on a host that has none; it
keeps the model of what each host has — sessions, tabs, panes — and it carries
a pane's bytes to you and your keystrokes back.

It draws nothing. There is one user interface and it is your application.

## Threading

Every function is safe to call from any thread. Calls are serialized inside,
so two threads calling at once is allowed and one of them waits.

**Every callback arrives on one thread, and always the same one.** State your
handler keeps needs no lock of its own against other callbacks. It does need
one against your own threads.

Nothing of iznik's is held while a callback runs, so **a handler may call back
into iznik.** It may attach, detach, credit, type, resize, focus, send a
command, or replace the event callback.

**The one call a handler may not make is `iznik_client_free`.** That waits for
the callback thread, and a handler that called it would be waiting for itself.

Three calls take away something a callback is reading, and each waits for a
handler that is already running before it returns: `iznik_pane_detach`,
`iznik_pane_attach` over an existing attachment, and `iznik_set_event_callback`.
When one of them answers, nothing is reading what you gave it any more, and
you may free it. A handler that makes one of those calls about its own pane
waits for nothing — it *is* the call that would be waited for.

## Ownership of memory

**Every buffer iznik passes to a callback is valid for that callback and no
longer.** If you need it afterwards, copy it while you have it.

**Every buffer you pass in is read before the call returns.** You may free it
as soon as the call answers.

An `iznik_error`'s `message` is iznik's, and stays valid until your next call
on the same thread.

A context pointer is neither: iznik never copies it and never frees it. You
keep it alive for as long as the thing it was given to lives — see the
obligations below, which say exactly how long that is.

There is one exception to the copying rule and it is the pane output path,
where the bytes go straight into your surface without being copied first.
That is the point of it.

## The obligations, as the header states them

These are quoted from `include/iznik.h`. They are the contract; the prose
around them is explanation.

`iznik_client_new`:

> **Obligation:** the pointer this gives back is freed exactly once, with `iznik_client_free`, and not used afterwards. Everything the configuration points at may be freed as soon as this returns.

`iznik_client_free`:

> **Obligation:** the pointer came from `iznik_client_new`, has not been freed, and is not used afterwards. A null pointer is nothing to free and is ignored. Not from inside a callback: this waits for the thread the callbacks arrive on, and a handler that called it would be waiting for itself. Letting a pane go and taking the event callback away may both be done from a handler; ending the client may not.

`iznik_set_event_callback`:

> **Obligation:** whatever `context` points at outlives the client, or the callback is set to null before it goes away.

`iznik_host_add`, and every other call that names a host:

> **Obligation:** `alias` is a null-terminated UTF-8 string, and may be freed as soon as this returns.

`iznik_command`:

> **Obligation:** the bytes are the application's and may be freed as soon as this returns; iznik reads them before it does.

`iznik_pane_attach`:

> **Obligation:** whatever `context` points at outlives the attachment — it is detached, the client is freed, or another attachment takes its place, before it goes away. Any of the three is enough on its own: each waits for a handler that is running before it returns, so the moment one of them answers, nothing is reading it any more.

`iznik_pane_input`:

> **Obligation:** the bytes are read before this returns and may be freed as soon as it does.

The pane's `output` callback:

> **Obligation:** the bytes are valid for this call only. Feed them to a surface before returning; do not keep the pointer.

The pane's `screen` callback:

> **Obligation:** reset the surface to `columns` by `rows` and feed it these bytes before any further output. What came before them is gone.

## Making a client

`iznik_client_new` takes an `iznik_configuration`, whose every field may be
null for the default:

- `runtime_directory` — where iznik keeps its own files.
- `artifacts_directory` — where the servers this build carries are, for
  installing on a host that has none.
- `askpass_program` — see *Upgrades and passphrases*.
- `log_path` — a file to write what happens to. Where a process writes its
  log is settled once and outlives the client that asked for it, so a second
  client naming a different file is refused rather than quietly writing
  nowhere.

It answers null when it cannot, having filled in the `iznik_error` you gave
it. `iznik_client_free` ends everything: every host let go, every task
stopped, the callback thread joined.

## Errors

Every call that can refuse takes an `iznik_error *` and returns `IZNIK_OK`,
`IZNIK_INVALID_ARGUMENT`, `IZNIK_UNKNOWN_HOST` or `IZNIK_REFUSED`.

An `iznik_error` carries the code, a `message` you may display verbatim, and
an `iznik_layer` saying where it came from: `IZNIK_LAYER_TRANSPORT`,
`IZNIK_LAYER_BOOTSTRAP`, `IZNIK_LAYER_SERVER`, `IZNIK_LAYER_PROTOCOL` or
`IZNIK_LAYER_CLIENT`. The layer is the first question anybody asks about a
system this tall, and it is answered as precisely as what is known allows.

## Hosts

`iznik_host_add` begins holding a host and returns at once; what happens next
arrives on the event callback. An alias is whatever the user typed. `unix:`
followed by a path names a daemon socket on this machine, which is how a Mac
attaches to itself and how every test drives this; anything else is handed to
`ssh` untouched, so a host from the user's own SSH configuration works
because it is their SSH that resolves it.

`iznik_host_remove` lets one go. `iznik_host_reconnect` asks for a
reconnection now rather than after the backoff.

## Events

`iznik_set_event_callback` installs one function that receives every
`iznik_event`. Its `kind` says what it is:

- `IZNIK_EVENT_KIND_HOST_STATE` — a host moved from one state to another. The
  payload is the state as UTF-8, for a person to read.
- `IZNIK_EVENT_KIND_SNAPSHOT` — the host's whole model, with `generation`
  saying which generation of it this is.
- `IZNIK_EVENT_KIND_DELTA` — one numbered change, with `generation` saying
  which generation it produces.
- `IZNIK_EVENT_KIND_COMMAND_RESULT` — the answer to something you sent, with
  `command_id` carrying the number you were given.
- `IZNIK_EVENT_KIND_MARK` — a shell-integration event in a pane.
- `IZNIK_EVENT_KIND_NOTIFICATION` — words for a person. When it is about a
  command, `command_id` carries that command's number.
- `IZNIK_EVENT_KIND_PANE_BYTES` — a pane's own bytes, for a pane nothing is
  attached to.
- `IZNIK_EVENT_KIND_SCREEN` — a pane's screen, with `columns` and `rows`.
- `IZNIK_EVENT_KIND_PANE_DETACHED` — the host has stopped sending a pane's
  output. There is no payload; `pane` names the pane.

Every payload is in `iznik/1`'s own encoding — the same schema the wire uses,
so there is one format and not two. The Rust crate that reads it is
`iznik_protocol`; the encoding is documented with the protocol.

**The events carry what the host has said, and never what the client is
showing ahead of it.** If you want a rename on screen before the host has
agreed to it, make that change yourself and put it back when the
`IZNIK_EVENT_KIND_COMMAND_RESULT` for your number says the host refused.

## Commands

`iznik_command` sends a session command encoded as the protocol encodes it,
and gives back the number its answer will carry. What the command did arrives
as a delta; whether it was allowed arrives as a command result.

## Panes

`iznik_pane_attach` takes an `iznik_pane_callbacks` — `output`, `screen`,
`mark` and `detached`, any of which may be null — and a context pointer that
every one of them receives.

A pane you have attached delivers through those handlers. A pane you have not
delivers through the event callback instead, as `IZNIK_EVENT_KIND_PANE_BYTES`
and `IZNIK_EVENT_KIND_SCREEN`.

`iznik_pane_detach` ends an attachment. `iznik_pane_focus` says which pane the
person is looking at, which is what a host uses to decide who gets bandwidth
first.

### The screen comes first

When you attach, the first thing you receive is a `screen`: the pane as it
stands, with the size it was drawn at. Reset your surface to that size and
feed it those bytes before anything else. Everything after it is a change to
it.

You will receive a screen again after a reconnection, and whenever the host
decides the cheapest way to make you right is to send the truth rather than
the difference. Each time, the obligation is the same and what came before is
gone.

### Credit

Flow control is yours to keep. The host sends you a window of bytes and stops
until you say you have consumed them. `iznik_pane_credit` is how you say it,
in bytes.

**An application that never returns credit stalls its own pane and nothing
else** — no other pane, no other host, and not the daemon. This is on purpose:
a surface that cannot keep up should fall behind rather than make everybody
else fall behind with it.

Return credit for what you consumed, as you consume it. The bytes of a
`screen` are not part of that window and are not credited.

### Geometry

**The application decides how large a pane is.** `iznik_pane_resize` tells the
host, the host tells the program in the pane, and every client attached to
that pane observes the new size — including yours, as a `PaneResized` change
in a delta.

The last resize wins. Two clients disagreeing about a size is two clients
taking turns, not an error.

### Query responses

A program in a pane may ask the terminal about itself — the cursor's position,
the colours it has, what it can do. Somebody has to answer.

**When you are attached, your emulator answers.** Feed the bytes to your
emulator as the `output` obligation says; your emulator produces a response;
you forward that response with `iznik_pane_input`, exactly as if the person
had typed it. That is what makes the answer describe *your* surface.

When nobody is attached, the host's own emulator answers, so a program does
not hang waiting for a reply that no one is there to give.

### Marks

`IZNIK_EVENT_KIND_MARK` carries shell-integration events: a prompt beginning,
a command starting, a command finishing with its exit status, and the pane
entering or leaving the alternate screen.

From the prompt and command marks you can build what a person actually wants:
jump to the previous prompt, select the output of one command, tell at a
glance which command failed. From the alternate-screen marks you can tell
"the pane is running something full-screen" from "the pane is at a shell",
which is the difference between a scrollback worth keeping and one that is
about to be thrown away.

## Reconnection

A link that drops is not a session that ends. Iznik reconnects on its own,
with a backoff, and the host's daemon has kept the sessions running the whole
time.

**What you keep:** everything. Your surfaces, your attachments, the pane
contents you have drawn.

**What you discard:** nothing, until a `screen` callback tells you to. When
one arrives, what came before it is gone and you reset to what it says.

You will see the host move through states on the event callback while this
happens, and the model you have may be replaced by a snapshot. Apply the
snapshot; it is the truth.

## Upgrades and passphrases

A host may be running an older server than this build carries. That is
reported to you, and `iznik_host_upgrade` acts on it. An upgrade is explicit
because the daemon *is* the sessions: replacing it ends them. A daemon holding
live panes refuses an upgrade and says how many, unless you force it.

`iznik_host_uninstall` takes everything iznik put on a host back off it. A
tool that installs binaries on other people's machines owes them that.

If reaching a host needs a passphrase, `ssh` asks for it the way it always
does — through the program named by `askpass_program` in your configuration.
Iznik never prompts, never reads a terminal, and never handles a passphrase
itself.

## Diagnosing

`iznik doctor <host>`, from the command-line tool beside this library, writes
one JSON document saying which layer is wrong: what this build is, what `ssh`
would do for that host, what a probe found, what the server says it is with
the tail of its own log, the state the host is in with every state it went
through, and a measured keystroke round trip. A section that could not be
filled carries the error that prevented it, and the sections under it say they
were skipped — so the shape of the document is the diagnosis.

It carries no secrets. Of everything `ssh` would say it reads a named handful,
never an identity file, an agent socket or a proxy command; it does not read
your environment; and what a transport says when it refuses is reported by its
kind rather than in its own words, because those words can contain whatever a
proxy command printed.

Send it with a bug report.

## What is reserved, and what will not change

The header is generated and committed, so any change to it is a change in a
commit somebody reviewed. Beyond that, two promises:

**Existing structures do not grow.** `iznik_configuration`, `iznik_error`,
`iznik_event` and `iznik_pane_callbacks` are passed across the boundary by
value or by pointer, so adding a field to one changes its size and breaks
every application built against the old one. New capability arrives as a new
function and, where it is something that happens, a new `iznik_event_kind`
added after the existing ones — both of which an application that does not
know about them ignores.

**Predictive local echo is the shape that is reserved.** Showing a keystroke
before the host has confirmed it is the one thing this boundary is expected to
grow, and it will arrive that way: your application keeps its own predictions
and draws them, and iznik gains a call to say which prediction the host
confirmed and a kind of event to say when one was wrong. No existing
signature changes, and an application that ignores the new kind behaves
exactly as it does today.

## The whole surface

Functions: `iznik_client_new`, `iznik_client_free`, `iznik_set_event_callback`,
`iznik_host_add`, `iznik_host_remove`, `iznik_host_reconnect`,
`iznik_host_upgrade`, `iznik_host_uninstall`, `iznik_command`,
`iznik_pane_attach`, `iznik_pane_detach`, `iznik_pane_credit`,
`iznik_pane_input`, `iznik_pane_resize`, `iznik_pane_focus`.

Types: `iznik_client`, `iznik_configuration`, `iznik_error`, `iznik_layer`,
`iznik_event`, `iznik_event_kind`, `iznik_event_callback`,
`iznik_pane_callbacks`.

Codes: `IZNIK_OK`, `IZNIK_INVALID_ARGUMENT`, `IZNIK_UNKNOWN_HOST`,
`IZNIK_REFUSED`.

## A worked example

Attaching a pane to a surface, in the order it happens.

```c
#include "iznik.h"

struct surface { /* yours */ };

static void on_screen(void *context, uint64_t sequence, uint16_t columns,
                      uint16_t rows, const uint8_t *bytes, size_t length) {
    struct surface *held = context;
    /* What came before is gone: resize, then feed, before any output. */
    surface_resize(held, columns, rows);
    surface_feed(held, bytes, length);
    (void)sequence;
}

static void on_output(void *context, const uint8_t *bytes, size_t length) {
    struct surface *held = context;
    /* Valid for this call only. Feed it now; do not keep the pointer. */
    surface_feed(held, bytes, length);

    /* Your emulator may have produced an answer to a query the program
     * asked. Forward it as if the person had typed it. */
    const uint8_t *answer = NULL;
    size_t answered = surface_take_response(held, &answer);
    if (answered > 0) {
        iznik_pane_input(held->client, held->alias, held->pane, answer,
                         answered, NULL);
    }

    /* And say you have consumed them, or the pane stops. */
    iznik_pane_credit(held->client, held->alias, held->pane,
                      (uint32_t)length, NULL);
}

static void on_mark(void *context, uint64_t sequence, const uint8_t *bytes,
                    size_t length) {
    struct surface *held = context;
    /* A prompt, a command starting or finishing, or the pane entering or
     * leaving the alternate screen. Decode with the protocol's own reader. */
    surface_note_mark(held, sequence, bytes, length);
}

static void on_detached(void *context) {
    struct surface *held = context;
    surface_note_gone(held);
}

void attach(struct surface *held, iznik_client *client, const char *alias,
            uint64_t pane) {
    iznik_pane_callbacks callbacks = {on_output, on_screen, on_mark,
                                      on_detached};
    iznik_error error = {0, IZNIK_LAYER_CLIENT, NULL};
    if (iznik_pane_attach(client, alias, pane, callbacks, held, &error)
        != IZNIK_OK) {
        report(error.message, error.layer);
        return;
    }
    /* From here: a screen arrives first, then output. When you are done,
     * iznik_pane_detach answers only once nothing is reading `held` any
     * more, and it may then be freed. */
}
```
