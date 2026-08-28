//! The `xterm-ghostty` terminfo source as a constant, with where it came from.
//!
//! The application renders with ghostty, so the programs on a remote host
//! should be told exactly that. `TERM=xterm-256color` is the nearest lie and
//! it costs real things: a program that asks whether the terminal has 24-bit
//! colour is told no, and one that asks about styled underlines is told
//! nothing at all.
//!
//! Most hosts do not have the entry — ncurses ships it from 6.5, and a host
//! installed before that will never see it — so iznik carries it and installs
//! it under its own prefix rather than asking anyone to update ncurses.
//!
//! **Where this came from.** `infocmp -x xterm-ghostty` on 2026-08-28, from
//! the terminfo database of ncurses 6.6.20251231 on this project's development
//! machine (`Linux 7.0.0-29-generic`, `x86_64`), where the entry is
//! `/usr/share/terminfo/x/xterm-ghostty`. `-x` keeps the user-defined
//! capabilities, which is where `Tc`, `Su` and `Smulx` live — the three this
//! exists for.
//!
//! One field of it is not `infocmp`'s. The entry's names were
//! `xterm-ghostty|ghostty|Ghostty`, and `tic` warns that a last field with no
//! blank in it may be read as a third alias by older versions rather than as
//! the description; it is `Ghostty terminal emulator` here, which is what
//! ncurses asks for and what makes the compile silent.

/// The terminal a pane's `TERM` names.
pub const TERMINAL_NAME: &str = "xterm-ghostty";

/// The terminfo source iznik installs on a host that has none.
pub const XTERM_GHOSTTY_TERMINFO: &str = include_str!("../../assets/xterm-ghostty.terminfo");
