# Scope — Plan 0008

> Replace the server on a host without ending the sessions it holds: an upgrade
> that execs the new binary in place, hands it the pseudoterminal masters it
> already owns, and rebuilds every mirror from the state carried across.

## Why this plan

The root architecture says it plainly: *the daemon is the sessions*. Every pane
is a pseudoterminal master fd, a child process group and a history ring; every
client-facing screen is a `libghostty-vt` mirror built from that ring. Replacing
the daemon's binary therefore ends all of it, which is why an upgrade warns and
is only ever done by somebody asking. That honesty has a cost: closing a
capability gap on a host — a server built before a command existed — needs the
running server replaced, and today that means the person loses every shell they
had open on it.

`ARCHITECTURE.md` names the alternative and defers it: *"Hot upgrade by
descriptor passing is deferred, not forgotten."* This plan is that work. It is
its own plan rather than a task under 0007 because the change is mostly on the
server side — the daemon, the registry, the PTY owner and the mirror — and it
needs a container proof of its own, not another item on the application's list.

## What it must make true

- A person can upgrade a host whose server is behind without losing the shells
  running on it: the same sessions, the same tabs, the same pane ids, the same
  on-screen contents and scrollback, the same live child processes and their
  exit statuses still observed.
- If the new binary cannot adopt what it was handed — it fails to start, its
  protocol is not this state's, its adoption is refused — the host is left
  exactly as it was, with the old daemon still holding everything, or the
  failure is said in full and the upgrade is retried rather than half-done.
- The wire does not carry pseudoterminal descriptors to a *client*. Adoption is
  strictly between two incarnations of the daemon on the host, over a socket
  and a state file that live in the host's own runtime directory, mode `0700`,
  owned by the same user.
- Nothing about the client changes: a client that was connected is disconnected
  by the replacement and reconnects, resuming each pane from the byte it holds,
  exactly as it does after a link drop. The pane's sequence numbering survives,
  so resume is byte-exact rather than a fresh `Screen`.

## In scope

- **0101 — The adoption boundary.** The one place `unsafe` is unavoidable:
  wrapping an inherited raw descriptor in an owning handle. The workspace
  forbids `unsafe` outside `iznik-ffi`, so this task decides where the primitive
  lives — a named addition to the FFI crate's surface, or a justified change to
  the unsafe-boundary rule recorded in `AGENTS.md` — and implements it with a
  `PtyProcess::adopt` constructor over `portable-pty`.
- **0102 — Carrying the state across.** `--adopt-state` on the daemon: the
  serialized registry — model, per-pane history ring and absolute sequence,
  pane-to-child mapping, terminal modes — written by the old incarnation and
  read by the new, versioned and refused rather than guessed when it does not
  match. Pure over bytes, and proven so.
- **0103 — The in-place replacement.** The staged binary, the `execv` in the
  daemon after it stops accepting and clears `CLOEXEC` on the masters it keeps,
  the inherited-fd map, and the rollback: an adoption that fails leaves the old
  daemon holding everything, and an adoption that half-succeeds is a bug the
  container proof hunts.
- **0104 — Rebuilding the mirrors.** Each adopted pane's mirror is rebuilt from
  its ring and its reported screen before the first client reconnects, so the
  first `Screen` after the upgrade is exact and the resume from a held byte is
  contiguous with what was there.
- **0105 — The application offers it.** `host: upgrade` keeps its warning and
  gains the choice between ending the sessions and keeping them; the kept path
  is taken only when the host's server advertises it can adopt, which is a
  capability like any other, and falls back to the ending path with the same
  warning when it cannot.

## Out of scope

- Migrating sessions between machines, or between two daemons that were never
  the same incarnation. Adoption is same-host, same-user, same-runtime-directory.
- Preserving a pane whose child has already exited, or whose terminal a program
  has put into a mode the new mirror cannot reproduce; those are panes that were
  ending anyway, and the upgrade reports them rather than pretending.
- Any change to `PROTOCOL_VERSION`: adoption is internal to the host and does
  not alter the client protocol. The `ADOPT` capability is the only wire-visible
  part.
