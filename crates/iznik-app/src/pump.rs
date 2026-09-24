//! The window's event-driven pump.
//!
//! The engine bridge and the emulator thread each raise a [`WakeSignal`] after
//! they queue something, and the pump drains both channels when either is
//! raised. An echoed keystroke is therefore drawn on the next turn of the
//! window's thread, not on the next tick of a timer, and an idle window sleeps.
//! A slow fallback timer remains, and is what paces the settings file poll.
//!
//! [`WakeSignal`]: crate::wake::WakeSignal

use std::time::Duration;

use gpui_kit::{Context, Focusable as _, Task, Window};

use crate::bridge::EngineBridge;
use crate::host_ui::NoticeKind;
use crate::vt::VtThread;
use crate::wake;
use crate::window::WindowShell;

/// Start the pump for `shell`: wait for either owner's signal or the fallback
/// timer, drain, and go straight round again while a drain stopped at its cap.
pub(crate) fn spawn(
    bridge: &EngineBridge,
    thread: &VtThread,
    fallback: Duration,
    window: &mut Window,
    context: &mut Context<'_, WindowShell>,
) -> Task<()> {
    let engine = bridge.wake_signal();
    let terminal = thread.wake_signal();
    context.spawn_in(window, async move |shell, asynchronous| {
        let mut more = false;
        loop {
            if more {
                wake::pass_turn().await;
            } else {
                let resting = asynchronous.background_executor().timer(fallback);
                wake::either(&engine, &terminal, resting).await;
            }
            match shell.update_in(asynchronous, |shell, target_window, update_context| {
                shell.drain(target_window, update_context)
            }) {
                Ok(remaining) => more = remaining,
                Err(_released) => break,
            }
        }
    })
}

impl WindowShell {
    /// Drain ready messages on the GPUI thread without waiting on either owner.
    pub fn update(&mut self, window: &mut Window, context: &mut Context<'_, Self>) {
        let _remaining = self.drain(window, context);
    }

    /// Read what each owner has queued, up to the per-update cap from each,
    /// and look at the settings file when its interval has passed. True when
    /// either owner may have more waiting, because its cap was reached.
    pub(crate) fn drain(&mut self, window: &mut Window, context: &mut Context<'_, Self>) -> bool {
        let cap = self.options.maximum_events_per_update;
        let mut engine_events = 0_usize;
        while engine_events < cap {
            let Some(event) = self.hosts().bridge().poll() else {
                break;
            };
            self.absorb(event, window, context);
            engine_events = engine_events.saturating_add(1);
        }
        self.show_failures(context);
        let mut terminal_events = 0_usize;
        let shown = self.shown_panes();
        while terminal_events < cap {
            let Some(event) = self.thread.poll() else {
                break;
            };
            terminal_events = terminal_events.saturating_add(1);
            let Some(surface) = self.panes.get(&event.key).map(|held| held.surface.clone()) else {
                continue;
            };
            let key = event.key.clone();
            if event.result.is_err() {
                // Where its emulator stands is no longer known, so asking for
                // the pane again must not resume from it.
                self.subscriptions.lost(&key);
            }
            let focused = surface
                .read(context)
                .focus_handle(context)
                .contains_focused(window, context);
            // Focus alone is not enough: the window keeps naming the last
            // focused pane after its tab is hidden, so a pane in a background
            // tab — of this host or another — could otherwise still copy.
            let allowed = focused && self.settings().clipboard_write && shown.contains(&key);
            let result = surface.update(context, |surface, surface_context| {
                surface.allow_program_clipboard(allowed);
                surface.receive(event, self.hosts().bridge(), surface_context)
            });
            if let Err(error) = result {
                self.failure(&key.host, error.to_string(), context);
            }
        }
        // A refusal's wait may have run out with nothing else said.
        self.attach_visible(context);
        self.synchronize_sizes(context);
        crate::settings::poll(self, self.options.settings_interval, context);
        self.write_session_tabs(false);
        engine_events >= cap || terminal_events >= cap
    }

    /// Take what the engine said since the last drain, showing its failures.
    ///
    /// Everything else it said is state the window already draws — a host's
    /// connection, an offer, an applied command — so it is let go here rather
    /// than kept: nothing else reads it, and kept it would grow for as long as
    /// the window is open.
    fn show_failures(&mut self, context: &mut Context<'_, Self>) {
        for notice in self.hosts.take_notices() {
            if notice.kind == NoticeKind::Failure {
                self.failure(&notice.host, notice.detail, context);
            }
        }
    }
}
