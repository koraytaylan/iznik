# The terminal mirror

Two tasks of plan 0002 — `terminal-mirror` and `screen-serializer` — confirmed
how `libghostty-vt` 0.2.1 behaves rather than assuming it, because the mirror is
only as trustworthy as the engine under it and the binding is young. This note
records what was found, what the mirror does about it, and the test in
`crates/iznik-server/tests/` that established each fact, so the next person does
not rediscover it by reading a panic.

Every claim here names a test that exists. Run one with
`cargo nextest run --package iznik-server --test <file> -E 'test(<name>)'`.

## Query responses

A program asks its terminal questions — the cursor's position, the device's
attributes, its colors — and the emulator delivers the answers two different
ways, both of which surface through the `on_pty_write` callback:

- **The emulator answers some itself.** Cursor-position (`CSI 6n`) and
  device-status (`CSI 5n`) reports are composed by the binding and written
  straight through `on_pty_write`. The mirror collects them from the pending
  buffer. Established by
  `terminal_mirror_reports_cursor_position_only_without_subscribers` in
  `tests/terminal_mirror.rs`.
- **The embedder answers the rest.** Device attributes (`CSI c`, `CSI >c`),
  XTVERSION (`CSI >q`), the enquiry character (`ENQ`), size reports (`CSI 18t`)
  and color-scheme queries reach dedicated callbacks
  (`on_device_attributes`, `on_xtversion`, `on_enquiry`, `on_size`,
  `on_color_scheme`). Each returns a structured value the terminal then formats
  and writes through `on_pty_write` — so the mirror still collects every answer
  from one place. The mirror answers these from iznik's own fixed values
  (`VT220`, `iznik-server <version>`, an empty answerback). Established by
  `terminal_mirror_answers_embedder_queries_only_without_subscribers` in
  `tests/terminal_mirror.rs`.

**The policy: answer only when nobody is subscribed.** An unattended program must
not hang waiting for a reply, so the mirror answers its queries; but the moment a
client attaches, the client's own emulator is the authority on its real colors
and capabilities, so the mirror stops answering and the query reaches the client
as `Input`. When several clients subscribe, each answers. Both tests above prove
the gate by asserting the pending buffer is empty once a subscriber is set. A
pane closes the same gate through `subscribe()`, proven end to end by
`pane_answers_queries_only_without_subscribers` in `tests/pane.rs`.

The effects the mirror deliberately drops — the bell, a clipboard write — produce
no reply, while a title and a working-directory report are still recorded off the
same bytes; established by `terminal_mirror_ignores_the_clients_effects` in
`tests/terminal_mirror.rs`.

## OSC is parsed by the observer, not by the binding

`libghostty-vt` 0.2.1 ships an `osc::Parser`, but it is **unusable for untrusted
input**: `end()` panics on a null command for the `vte.shell.preexec` mark bash
emits (`ESC ] 666 ; ... ! ...`), on an empty or unterminated payload, and on a
payload past roughly four kibibytes; title extraction always comes back empty;
and the command type it does return is too coarse to tell OSC 7 from a title.
So the shell-integration observer (`terminal::marks`) parses OSC itself, with a
Williams-style scanner that treats `ESC` as a universal restart and dispatches a
sequence at its terminator. It recognizes OSC 133 prompt marks, OSC 7
directories, titles and the alternate-screen `CSI` switches without modifying,
delaying or reordering a byte, and across any chunk boundary. Established by
`shell_integration_marks_noise_is_ignored` (the exact inputs that crash the
binding pass through untouched), `shell_integration_marks_split_at_every_byte_is_identical`
(the chunk-boundary property) and `shell_integration_marks_the_asset_emits_the_marks`
(a real bash), all in `tests/shell_integration_marks.rs`.

`Terminal::title()` and `Terminal::working_directory()`, unlike `osc::Parser`,
**do** work, and the mirror reads them for the reference-oracle comparison —
established by `terminal_mirror_agrees_with_the_oracle` in
`tests/terminal_mirror.rs`.

## Scrollback is a byte budget, not a line count

`Options::max_scrollback` is documented as lines but is spent as a memory budget:
setting it to 10000 keeps roughly a thousand short rows, not ten thousand. The
scrollback is still bounded, which is all the mirror needs. Established by
`terminal_mirror_bounds_its_scrollback` in `tests/terminal_mirror.rs`, which
floods far past the budget and four times over and sees the row count hold.

## Dimensions must be read live

A program can resize its own terminal — `DECCOLM` (`CSI ?3h`) switches to 132
columns. So `Terminal::cols()` and `rows()` are read live on every use rather
than cached at construction, and the mirror's `columns()`/`rows()` read through
to them. Established by `terminal_mirror_agrees_with_the_oracle` in
`tests/terminal_mirror.rs`, whose final case drives `DECCOLM` and checks the
mirror follows.

## What the formatter emits, and what it does not

The screen serializer reconstructs a pane's screen by formatting the emulator's
state as VT sequences (`Format::Vt`) with the cursor, styles, scrolling region,
tab stops, working directory, keyboard and charset modes, and **without** the
palette. Feeding those bytes into a fresh emulator of the same size reproduces
the mirror's **content, cursor and layout exactly** — proven across the fidelity
corpus and hundreds of random streams by
`screen_serializer_reproduces_the_mirror` in `tests/screen_serializer.rs`, and
for every construct through a real pane on the musl binary by
`fidelity_is_byte_identical_and_reproduces_every_construct` in
`tests/fidelity.rs`.

The reconstruction has edges this crate version inherits until the binding
matures, each worked around where it mattered and each recorded in `screen.rs`:

- **A scrolled screen whose bottom row is left blank comes back one row behind.**
  The property rests the cursor on content to target the state every program
  actually pauses at. (`screen_serializer_reproduces_the_mirror`.)
- **An overwritten or erased cell can come back with a stale style.** The
  reproduction property compares screen layout, not per-cell attributes; the
  styles a program relies on are proven exactly, by their values, by
  `screen_serializer_preserves_styles` in `tests/screen_serializer.rs`.
- **Hyperlinks (OSC 8) are never emitted**, even with hyperlinks enabled — noted
  by the same styles test, which asserts everything else survives.
- **A reflow after a resize is approximate**, so resize is proven through the
  live oracle rather than the exact-equality property.
- **The cursor is emitted before the tab-stops pass**, which moves it; the
  serializer re-emits the cursor position last to undo that.

The palette is deliberately absent — the server's palette is not the client's —
proven by `screen_serializer_emits_no_palette` in `tests/screen_serializer.rs`.

The output is bounded: a screen whose whole serialization would pass the byte cap
is brought within it by dropping its oldest scrollback rows, the newest still
reproducing. Established by `screen_serializer_bounds_the_output` in
`tests/screen_serializer.rs`.

## How the alternate screen is reproduced, and why

The formatter can serialize only the **active** screen, so when a program is on
the alternate screen the primary would be lost. The mirror cannot serialize an
inactive screen, and it cannot wait until the program leaves — the primary is
gone by then. So the serialized screen of a pane on the alternate screen is the
**primary as it stood at the switch, then the switch itself, then the live
alternate screen**: the observer recognizes the `1049`/`47`/`1047` switch, and
the pane serializes the primary before feeding it. Applying that to a fresh
surface puts it on the alternate screen showing the alternate content, and
leaving the alternate screen there reveals the primary — exactly as it does on
the server.

Established by `screen_serializer_remembers_the_primary_across_the_alternate_screen`
in `tests/screen_serializer.rs` (the mechanism), and end to end through a real
pane by `pane_remembers_the_alternate_screen` and — for several switches in one
read, enter, leave, enter, remembered in order — `pane_remembers_repeated_alternate_switches`,
both in `tests/pane.rs`.
