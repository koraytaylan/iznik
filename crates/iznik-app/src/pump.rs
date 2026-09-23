//! The window's event-driven pump.
//!
//! The engine bridge and the emulator thread each raise a [`WakeSignal`] after
//! they queue something, and the pump drains both channels when either is
//! raised. An echoed keystroke is therefore drawn on the next turn of the
//! window's thread, not on the next tick of a timer, and an idle window sleeps.
//! A slow fallback timer remains, and is what paces the settings file poll.
//!
//! [`WakeSignal`]: crate::wake::WakeSignal

use std::time::{Duration, Instant};

use gpui_kit::{Context, Focusable as _, Task, Window};

use crate::bridge::EngineBridge;
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
            let focused = surface
                .read(context)
                .focus_handle(context)
                .contains_focused(window, context);
            // Focus alone is not enough: the window keeps naming the last
            // focused pane after its tab is hidden, so a pane in a background
            // tab — of this host or another — could otherwise still copy.
            let allowed = focused && self.settings.clipboard_write && shown.contains(&key);
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
        self.poll_settings(context);
        self.write_session_tabs(false);
        engine_events >= cap || terminal_events >= cap
    }

    /// Look at the configured settings file, at most once per settings interval.
    fn poll_settings(&mut self, context: &mut Context<'_, Self>) {
        let now = Instant::now();
        if self.settings_polled.is_some_and(|last| {
            now.saturating_duration_since(last) < self.options.settings_interval
        }) {
            return;
        }
        self.settings_polled = Some(now);
        let outcome = {
            let Some(watcher) = self.settings_watcher.as_mut() else {
                return;
            };
            watcher.reload(&mut self.settings)
        };
        match outcome {
            Ok(true) => {
                crate::settings::report_unknown(self, context);
                crate::settings::apply_saved(self, context);
            }
            Ok(false) => {}
            Err(refusal) => crate::settings::refuse(self, &refusal, context),
        }
    }
}
