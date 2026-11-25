//! Terminal rendering and ETA tracking for cache pruning.

use std::fmt;
use std::time::Duration;

use indicatif::style::TemplateError;
use indicatif::{HumanDuration, ProgressBar, ProgressState, ProgressStyle};

#[cfg(test)]
mod tests;

const SCAN_TEMPLATE: &str = "{spinner} [{elapsed_precise}] Scanning incremental caches";
const PROCESS_TEMPLATE: &str = concat!(
    "{spinner} [{elapsed_precise}] [{wide_bar}] {pos}/{len} sessions",
    " | ETA {remaining} | {msg}\n  {prefix}",
);
const PROGRESS_TICK: Duration = Duration::from_millis(100);

/// Status shown while checking sessions.
pub const CHECKING: &str = "Checking";

/// Compound operations for the terminal display and the hidden ETA tracker.
pub trait ProgressBarExt {
    /// Starts the scan spinner and returns whether the display is visible.
    fn start_scan(&self) -> bool;

    /// Switches the spinner to a progress bar for the discovered sessions.
    fn start_processing(&self, sessions: usize, message: String, style: ProgressStyle);

    /// Starts measuring the processing rate against the estimated work.
    fn start_estimate(&self, total_work: u64);

    /// Credits the work performed and reconciles the estimate for remaining sessions.
    fn record_work(&self, completed: u64, remaining: u64);

    /// Advances the session count and updates the totals message when supplied.
    fn record_session(&self, message: Option<String>);
}

impl ProgressBarExt for ProgressBar {
    fn start_scan(&self) -> bool {
        if self.is_hidden() {
            return false;
        }
        self.tick();
        self.enable_steady_tick(PROGRESS_TICK);
        true
    }

    fn start_processing(&self, sessions: usize, message: String, style: ProgressStyle) {
        self.set_length(sessions as u64);
        self.set_prefix(CHECKING);
        self.set_message(message);
        self.set_style(style);
        self.force_draw();
    }

    fn start_estimate(&self, total_work: u64) {
        self.set_length(total_work);
        self.reset_eta();
    }

    fn record_work(&self, completed: u64, remaining: u64) {
        self.update(|state| {
            let position = state.pos().saturating_add(completed);
            state.set_len(position.saturating_add(remaining));
            state.set_pos(position);
        });
    }

    fn record_session(&self, message: Option<String>) {
        self.inc(1);
        if let Some(message) = message {
            self.set_message(message);
        }
    }
}

/// Creates the display, the hidden ETA tracker, and the processing template.
pub fn new() -> Result<(ProgressBar, ProgressBar, ProgressStyle), TemplateError> {
    let display =
        ProgressBar::new_spinner().with_style(ProgressStyle::with_template(SCAN_TEMPLATE)?);
    let work = ProgressBar::hidden();
    let eta = work.clone();
    let style = ProgressStyle::with_template(PROCESS_TEMPLATE)?.with_key(
        "remaining",
        move |state: &ProgressState, writer: &mut dyn fmt::Write| {
            let duration = eta.eta();
            let _ = if duration.is_zero() && state.len() != Some(state.pos()) {
                writer.write_str("--")
            } else {
                write!(writer, "{:#}", HumanDuration(duration))
            };
        },
    );
    Ok((display, work, style))
}
