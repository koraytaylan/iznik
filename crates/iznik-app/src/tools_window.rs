//! The developer tools window, laid out like Settings: a sidebar with two pages.
//!
//! Hosts lists what the session bar does not: which server answered, which
//! capabilities it advertised, each pane's identity, and how far a subscription
//! has been read. Log is the process log, oldest first, following the newest
//! line. Both redraw when what they show changes.

use std::fmt::Write as _;

use gpui_kit::component::description_list::DescriptionList;
use gpui_kit::component::setting::{
    SettingGroup, SettingItem, SettingPage, Settings as SettingsPanel,
};
use gpui_kit::component::{Root, TitleBar};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FollowMode, InteractiveElement, IntoElement,
    ListAlignment, ListState, ParentElement, Render, StatefulInteractiveElement, Styled,
    Subscription, Task, TestSupportExt, Window, WindowBounds, WindowOptions, div, list, px, size,
};
use iznik_client::host::identity::HostId;
use iznik_client::host::state::HostState;
use iznik_client::model::HostView;
use iznik_protocol::capabilities::Capabilities;
use iznik_protocol::model::Pane;

use crate::host_ui::{EngineState, HostReport, Notice};
use crate::window::{TabKey, WindowShell};

/// Initial width of the developer tools window.
const WINDOW_WIDTH: f32 = 960.0;
/// Initial height of the developer tools window.
const WINDOW_HEIGHT: f32 = 680.0;
/// Title shown in the window's title bar.
const WINDOW_TITLE: &str = "Developer Tools";
/// How wide a description label is.
const LABEL_WIDTH: f32 = 160.0;
/// How tall the log console is inside its page.
const LOG_VIEW_HEIGHT: f32 = 480.0;
/// How far past the visible log the list prepares rows, so scrolling does not
/// wait to measure them.
const LOG_OVERDRAW: f32 = 64.0;
/// What the hosts page says it is.
const HOSTS_PAGE: &str = "Hosts";
/// What the log page says it is.
const LOG_PAGE: &str = "Log";
/// Shown when the engine has no host.
const EMPTY_REPORT: &str = "No host is held.";
/// Shown when the main window has gone.
const CLOSED_REPORT: &str = "The main window has closed.";

/// The lines the window draws for `state`.
///
/// `selected` marks the tab on screen. `failure` is the notice the main
/// window is showing, when it is showing one.
#[must_use]
pub fn report_lines(
    state: &EngineState,
    selected: Option<&TabKey>,
    failure: Option<&Notice>,
) -> Vec<String> {
    let cards = host_cards(state, selected, failure);
    if cards.is_empty() {
        return vec![EMPTY_REPORT.to_owned()];
    }
    let mut lines = Vec::new();
    for card in cards {
        lines.push(card.alias);
        for (label, value) in card.rows {
            lines.push(format!("  {label}: {value}"));
        }
    }
    lines
}

/// Open the developer tools window over `context`'s shell.
pub fn open(context: &mut Context<'_, WindowShell>) {
    let shell = context.entity();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(
            size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)),
            context,
        )),
        ..TitleBar::window_options()
    };
    context.defer(move |app| {
        let _window = app.open_window(options, move |window, app| {
            let view = app.new(|view_context| ToolsWindow::new(&shell, view_context));
            app.new(|root_context| Root::new(view, window, root_context))
        });
    });
}

/// Every host the connection map or the model knows, in alias order.
fn listed_hosts(state: &EngineState) -> Vec<HostId> {
    let mut hosts: Vec<HostId> = state.hosts().map(|(host, _report)| host.clone()).collect();
    for host in state.model().hosts.keys() {
        if !hosts.iter().any(|held| held == host) {
            hosts.push(host.clone());
        }
    }
    hosts.sort();
    hosts
}

/// One host as the window shows it: a title and labeled rows.
struct HostCard {
    /// The host alias, or the failure heading.
    alias: String,
    /// Label and value pairs, in display order.
    rows: Vec<(String, String)>,
}

/// Every card the window has to draw, failure first.
fn host_cards(
    state: &EngineState,
    selected: Option<&TabKey>,
    failure: Option<&Notice>,
) -> Vec<HostCard> {
    let mut cards = Vec::new();
    if let Some(failure) = failure {
        cards.push(HostCard {
            alias: format!("failure {}", failure.host),
            rows: vec![("Detail".to_owned(), failure.detail.clone())],
        });
    }
    for host in listed_hosts(state) {
        let report = state
            .host(&host)
            .cloned()
            .unwrap_or_else(HostReport::unknown);
        cards.push(host_card(
            &host,
            &report,
            state.model().host(&host),
            selected,
        ));
    }
    cards
}

/// The labeled rows for one host.
fn host_card(
    host: &HostId,
    report: &HostReport,
    view: Option<&HostView>,
    selected: Option<&TabKey>,
) -> HostCard {
    let mut rows = vec![("Connection".to_owned(), report.connection.to_string())];
    if let HostState::Connected { capabilities, .. } = report.connection {
        rows.push(("Capabilities".to_owned(), capability_names(capabilities)));
        let missing = capabilities.missing_features();
        if missing.bits() != 0 {
            rows.push(("Missing".to_owned(), capability_names(missing)));
        }
    }
    if let Some(offer) = &report.upgrade {
        rows.push(("Upgrade".to_owned(), offer.summary(host)));
    }
    if let Some(view) = view {
        rows.extend(model_rows(host, view, selected));
    }
    HostCard {
        alias: host.to_string(),
        rows,
    }
}

/// Generation, focus, waiting commands, and one row per pane.
fn model_rows(host: &HostId, view: &HostView, selected: Option<&TabKey>) -> Vec<(String, String)> {
    let mut rows = vec![("Generation".to_owned(), view.model.generation.0.to_string())];
    if let Some(focus) = view.focus {
        rows.push(("Focus".to_owned(), format!("pane {}", focus.0)));
    }
    if !view.pending.is_empty() {
        rows.push((
            "Waiting".to_owned(),
            format!("{} commands", view.pending.len()),
        ));
    }
    if view.model.sessions.is_empty() {
        rows.push(("Sessions".to_owned(), "none".to_owned()));
        return rows;
    }
    for session in &view.model.sessions {
        for tab in &session.tabs {
            let shown = selected.is_some_and(|key| {
                key.host == *host && key.session == session.id && key.tab == tab.id
            });
            let mark = if shown { " shown" } else { "" };
            let place = format!(
                "session {} {} / tab {} {}{mark}",
                session.id.0, session.name, tab.id.0, tab.name
            );
            for pane in &tab.panes {
                rows.push((place.clone(), pane_line(pane, view)));
            }
        }
    }
    rows
}

/// One pane, including the subscription cursor when this client has one.
fn pane_line(pane: &Pane, view: &HostView) -> String {
    let mut line = format!("      pane {} {}x{}", pane.id.0, pane.columns, pane.rows);
    if !pane.title.is_empty() {
        let _written = write!(line, " \"{}\"", pane.title);
    }
    if let Some(directory) = &pane.working_directory {
        let _written = write!(line, " {directory}");
    }
    if let Some(subscription) = view.subscriptions.get(&pane.id) {
        let _written = write!(
            line,
            " cursor {} credit {}",
            subscription.cursor.0, subscription.credit_outstanding
        );
    }
    line
}

/// The known capability names `capabilities` carries, then any unknown bits.
fn capability_names(capabilities: Capabilities) -> String {
    let mut names = Vec::new();
    for (bit, name) in known_capabilities() {
        if capabilities.contains(bit) {
            names.push(name.to_owned());
        }
    }
    let unknown = capabilities.unknown_bits();
    if unknown != 0 {
        names.push(format!("unknown {unknown:#x}"));
    }
    if names.is_empty() {
        "none".to_owned()
    } else {
        names.join(" ")
    }
}

/// Known capability bits, in the order the protocol defines them.
fn known_capabilities() -> Vec<(Capabilities, &'static str)> {
    vec![
        (Capabilities::ZSTD, "zstd"),
        (Capabilities::RESUME, "resume"),
        (Capabilities::REORDER_SESSIONS, "reorder"),
        (Capabilities::ADOPT, "adopt"),
        (Capabilities::INSTANCE, "instance"),
        (Capabilities::ANSWERED, "answered"),
        (Capabilities::IDENTIFY, "identify"),
        (Capabilities::BUILD, "build"),
        (Capabilities::UPLOAD, "upload"),
    ]
}

/// The developer tools window: the same sidebar layout as settings, with a
/// hosts page and a log page.
struct ToolsWindow {
    /// The main window whose model is listed.
    shell: gpui_kit::WeakEntity<WindowShell>,
    /// Redraws this window when the shell's model changes.
    _watch: Subscription,
    /// Log lines currently drawn, oldest first.
    log_lines: Vec<String>,
    /// Tail-following list for the log page. Rows are only as tall as the text.
    log_list: ListState,
    /// Wakes this window when a log line is stored. Held so the wait is cancelled
    /// with the window.
    _log_task: Task<()>,
}

impl ToolsWindow {
    /// Watch `shell` and the process log.
    fn new(shell: &Entity<WindowShell>, context: &mut Context<'_, Self>) -> Self {
        let watch = context.observe(shell, |_tools, _shell, context| context.notify());
        let log_lines = crate::tools_log::snapshot();
        let log_list = log_list(log_lines.len());
        let log_task = watch_log(context);
        Self {
            shell: shell.downgrade(),
            _watch: watch,
            log_lines,
            log_list,
            _log_task: log_task,
        }
    }

    /// Replace the drawn log when the record has changed, and keep the console
    /// following its tail.
    fn adopt_log(&mut self) {
        let next = crate::tools_log::snapshot();
        if next == self.log_lines {
            return;
        }
        self.log_lines = next;
        self.log_list.reset(self.log_lines.len());
        self.log_list.set_follow_mode(FollowMode::Tail);
    }

    /// The host cards to draw, or one card when the main window has closed.
    fn cards(&self, app: &App) -> Vec<HostCard> {
        self.shell.upgrade().map_or_else(
            || {
                vec![HostCard {
                    alias: CLOSED_REPORT.to_owned(),
                    rows: Vec::new(),
                }]
            },
            |shell| {
                let shell = shell.read(app);
                host_cards(
                    shell.hosts.state(),
                    shell.selected(),
                    shell.notices.failure(),
                )
            },
        )
    }

    /// The report text the hosts page exposes to assistive technology.
    fn report(&self, app: &App) -> String {
        self.shell.upgrade().map_or_else(
            || CLOSED_REPORT.to_owned(),
            |shell| {
                let shell = shell.read(app);
                report_lines(
                    shell.hosts.state(),
                    shell.selected(),
                    shell.notices.failure(),
                )
                .join("\n")
            },
        )
    }
}

/// Redraw when a log line arrives, until this window is gone.
fn watch_log(context: &mut Context<'_, ToolsWindow>) -> Task<()> {
    let wake = crate::tools_log::wake();
    context.spawn(async move |view, asynchronous| {
        loop {
            wake.raised().await;
            let refreshed = view.update(asynchronous, |tools, update_context| {
                tools.adopt_log();
                update_context.notify();
            });
            if refreshed.is_err() {
                break;
            }
        }
    })
}

/// The hosts page: one group per host, each a labeled list.
fn hosts_page(cards: &[HostCard], report: &str) -> SettingPage {
    let mut page = SettingPage::new(HOSTS_PAGE)
        .resettable(false)
        .description("The connection, the server, and every pane this client is showing.");
    if cards.is_empty() {
        let label = report.to_owned();
        return page.group(
            SettingGroup::new().item(SettingItem::render(move |_, _, _| {
                div()
                    .id("developer-hosts")
                    .test_support()
                    .aria_label(label.clone())
                    .text_sm()
                    .child(EMPTY_REPORT)
            })),
        );
    }
    let mut first = true;
    for card in cards {
        let rows = card.rows.clone();
        let alias = card.alias.clone();
        let label = first.then(|| report.to_owned());
        first = false;
        page = page.group(
            SettingGroup::new()
                .title(alias.clone())
                .item(SettingItem::render(move |_, _, _| {
                    host_list(alias.clone(), rows.clone(), label.clone())
                })),
        );
    }
    page
}

/// One host's rows, as a description list. The first host carries the whole
/// report as its accessible label, which is what a test reads.
fn host_list(
    alias: String,
    rows: Vec<(String, String)>,
    label: Option<String>,
) -> impl IntoElement {
    let mut list = DescriptionList::vertical()
        .columns(1)
        .label_width(px(LABEL_WIDTH));
    for (name, value) in rows {
        list = list.item(name, value, 1);
    }
    let identifier = if label.is_some() {
        drop(alias);
        "developer-hosts".to_owned()
    } else {
        let mut identifier = "developer-host-".to_owned();
        identifier.push_str(&alias);
        drop(alias);
        identifier
    };
    let mut block = div().id(identifier).test_support().w_full();
    if let Some(label) = label {
        block = block.aria_label(label);
    }
    block.child(list)
}

/// The log page: a console that follows the newest line, with a jump back to it.
/// A log list that follows its last line. Rows carry no padding of their own.
fn log_list(count: usize) -> ListState {
    let list = ListState::new(count, ListAlignment::Top, px(LOG_OVERDRAW));
    list.set_follow_mode(FollowMode::Tail);
    list
}

/// The log page: the same type as the rest of the window, one line against the next.
fn log_page(lines: &[String], log_list: ListState) -> SettingPage {
    let count = lines.len();
    let drawn = lines.to_vec();
    let description = if count == 0 {
        "No log lines yet. The level is IZNIK_LOG, or info when that is unset. Newest line at the bottom."
            .to_owned()
    } else {
        "The level is IZNIK_LOG, or info when that is unset. Newest line at the bottom.".to_owned()
    };
    SettingPage::new(LOG_PAGE)
        .resettable(false)
        .description(description)
        .group(
            SettingGroup::new().item(SettingItem::render(move |_, _, _| {
                let stored = drawn.clone();
                div().h(px(LOG_VIEW_HEIGHT)).w_full().child(
                    list(log_list.clone(), move |index, _, _| {
                        let line = stored.get(index).cloned().unwrap_or_default();
                        div()
                            .w_full()
                            .min_w_0()
                            .text_sm()
                            .child(line)
                            .into_any_element()
                    })
                    .size_full(),
                )
            })),
        )
}

impl Render for ToolsWindow {
    fn render(
        &mut self,
        _window: &mut Window,
        context: &mut Context<'_, Self>,
    ) -> impl IntoElement {
        self.adopt_log();
        let cards = self.cards(context);
        let report = self.report(context);
        let log_lines = self.log_lines.clone();
        let log_list = self.log_list.clone();
        div()
            .id("developer-tools")
            .test_support()
            .size_full()
            .flex()
            .flex_col()
            .child(TitleBar::new().child(WINDOW_TITLE))
            .child(
                div().flex_1().min_h_0().child(
                    SettingsPanel::new("iznik-developer")
                        .page(hosts_page(&cards, &report))
                        .page(log_page(&log_lines, log_list)),
                ),
            )
    }
}
