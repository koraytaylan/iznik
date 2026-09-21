---
id: ssh-config-hosts
title: "Discover hosts in the person's ssh configuration, and add one by writing it"
workstream: "0003"
kind: task
depends_on:
  - command-palette
  - tab-and-session-bars
gated: false
touches:
  - crates/iznik-app/README.md
  - crates/iznik-app/src/bridge.rs
  - crates/iznik-app/src/bars.rs
  - crates/iznik-app/src/chrome.rs
  - crates/iznik-app/src/follow.rs
  - crates/iznik-app/src/lib.rs
  - crates/iznik-app/src/palette.rs
  - crates/iznik-app/src/prompt.rs
  - crates/iznik-app/src/ssh_config.rs
  - crates/iznik-app/src/stage.rs
  - crates/iznik-app/src/tab_actions.rs
  - crates/iznik-app/src/window.rs
  - crates/iznik-app/tests/add_host.rs
  - crates/iznik-app/tests/end_to_end.rs
  - crates/iznik-app/tests/flow.rs
  - crates/iznik-app/tests/ssh_config.rs
  - crates/iznik-app/tests/tab_menu.rs
  - regression/claims/ssh-config-hosts.toml
  - regression/claims/tab-and-session-bars.toml
status: done
merged_as: ""
---
# Discover hosts in the person's ssh configuration, and add one by writing it

Add Host reads the concrete aliases a person's `~/.ssh/config` defines and offers them; a name it does not define is asked for an address and appended as a `Host`/`HostName` stanza before the host is held. The tab right-click menu is repaired to open from the shell's own state, because the kit's context-menu wrapper drops its open menu on the next layout pass.

**Steps:**

1. `crates/iznik-app/src/ssh_config.rs`: `parsed_aliases`, `aliases`, `defines`, `parsed_known_hosts`, `known_hosts`, `append_host`, `default_path`, `default_known_hosts_path` and `SshConfigError` — the concrete-pattern rule, the names `known_hosts` remembers, the block written, and what is refused.
2. `prompt.rs`: `Expected::Alias` carries the configuration's aliases, `Expected::HostAddress` follows a name nothing defines, and `Answer::AddHostWithAddress` is the pair the stanza is written from. `choices` offers the aliases plus the typed answer's own row.
3. `palette.rs`: `submit_prompt` turns an undefined alias into the address question; `perform` writes the stanza through `WindowShell::add_host_with_address` before holding the host. `ShellOptions::ssh_config_path` points a test at its own file.
4. `stage.rs`: the Welcome stage carries the hosts the shell read — the configuration's aliases, then the names `known_hosts` remembers that it does not — and renders them as one-click buttons, so the first thing the window asks for is a host to choose; `chrome.rs` reads them from the shell when it builds the stage. The shell never resolves `$HOME` itself: the binary hands it the person's path, so a case that names none reads no file the machine holds.
5. `tab_actions.rs`: `OpenMenu` owns the built `PopupMenu` and its dismiss subscription; the shell renders it anchored where the right click landed. `bars.rs` opens it from the tab's right mouse down.
6. Write the tests: `ssh_config.rs` for the parser and the writer, `add_host.rs` for the prompt rules and the live write, `flow.rs` for the welcome listing and clicking a configured host, `tab_menu.rs` for a menu that survives the window's timed repaints, and the end-to-end tab-bar case through the real right click.
7. Declare the claims in `regression/claims/ssh-config-hosts.toml` and extend `tab-and-session-bars.toml`.

**Tests:**

- A configuration's concrete `Host` patterns are read in order; `*`, `?` and `!` patterns are not hosts; matching is case-free.
- The names `known_hosts` remembers are read in order with duplicates collapsed; hashed names, bracketed ports, bare addresses and comments are left out, and a name that looks like hex (`cafe.be`) is still a name.
- The configuration's hosts lead and the record's names the configuration does not hold follow them, so a configuration that is only `Host *` still offers the machines its person reached.
- Appending leaves every earlier byte and writes `Host <alias>` with an indented `HostName <address>`; an alias already defined, an alias that cannot stand as a pattern, and an address that cannot stand on its line are refused without touching the file.
- The welcome lists those hosts and clicking one holds and connects it; a shell given no configuration path and a configuration naming none both show no list, and still offer Add Host.
- Add Host lists the configuration's aliases; a `unix:` answer is offered as itself; an undefined name asks for its address; the live flow reads a scratch configuration, writes a new block, and holds a local host through the palette.
- A tab's right click opens its menu, and the menu is still rendered after the window's next repaints.

- **Done when:** `timeout 600 cargo nextest run --package iznik-app --test ssh_config --test add_host --test tab_menu --test end_to_end` passes every case above, `timeout 900 cargo xtask claims verify --task ssh-config-hosts` reports every claim proven.
