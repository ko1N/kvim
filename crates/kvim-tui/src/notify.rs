//! The notification board that the bottom-right overlay paints.
//!
//! The overlay reports the work-done progress of each language server, and
//! nothing else. Every ordinary editor report stays on the message line and the
//! statusline. The board is a pure value. It reads no clock: the event loop
//! passes the elapsed time into every call, and the board reports the elapsed
//! time of its next visible change through
//! [`NotificationBoard::next_deadline`]. The loop waits for that time beside its
//! terminal events, so the spinner needs no frame loop. See
//! `docs/responsiveness.md` and `docs/language-services.md`.

use std::time::Duration;

use kvim_language::{
    LanguageServerId, ProgressPercentage, ProgressReport, ProgressStage, ProgressToken,
    SessionGeneration,
};
use kvim_settings::NotificationSettings;

use super::session::Redraw;
use super::theme::ThemeRole;

/// The state word of one running item.
const RUNNING_LABEL: &str = "In progress...";

/// The state icon of one finished item.
///
/// The reference `fidget.nvim` configuration names the same icon.
const DONE_ICON: &str = "✓";

/// The animated spinner of one group that holds a running item.
///
/// The frames are the `dots` pattern of the reference `fidget.nvim`
/// configuration. One complete cycle takes
/// [`NotificationSettings::spinner_period`].
const SPINNER_FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// The shortest time between two spinner frames.
///
/// A period that divides into nothing would leave a deadline that no transition
/// clears, and the event loop would stop serving input.
const SPINNER_FRAME_MIN: Duration = Duration::from_millis(1);

/// The state of one progress item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProgressState {
    /// The operation runs, and the server may report its completion.
    Running {
        /// The completion that the server reported last.
        percentage: Option<ProgressPercentage>,
    },
    /// The operation finished at this elapsed time.
    Finished {
        /// The elapsed time of the last report of the operation.
        at: Duration,
    },
}

/// One item of the board: one server operation of one language server.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NotificationItem {
    /// The order of the item, which names the oldest item of the board.
    sequence: u64,
    /// The text that the overlay shows beside the state.
    message: String,
    /// The token that the `begin` of the operation assigned.
    token: ProgressToken,
    /// The state of the operation.
    state: ProgressState,
}

impl NotificationItem {
    /// Returns the elapsed time at which the item finished, or `None` while it
    /// still runs.
    const fn finished_at(&self) -> Option<Duration> {
        match self.state {
            ProgressState::Finished { at } => Some(at),
            ProgressState::Running { .. } => None,
        }
    }

    /// Returns the row that the overlay paints for this item.
    fn row(&self) -> NotificationRow<'_> {
        let (state, percentage) = match self.state {
            ProgressState::Running { percentage } => (RowState::Running, percentage),
            ProgressState::Finished { .. } => (RowState::Finished, None),
        };
        NotificationRow::Item {
            state,
            message: &self.message,
            percentage,
        }
    }
}

/// One group of the board: everything that one language server reports.
#[derive(Clone, Debug, Eq, PartialEq)]
struct NotificationGroup {
    /// The server that owns the session.
    ///
    /// One language can run several servers, so the group key is the server
    /// and never the adapter alone. Each server therefore keeps its own title
    /// row, its own items, and its own session generation.
    server: LanguageServerId,
    /// The title row of the group.
    title: &'static str,
    /// The newest session attempt whose reports the group accepts.
    generation: SessionGeneration,
    /// The items of the group, oldest first.
    items: Vec<NotificationItem>,
}

/// The state column of one item row.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RowState {
    /// One server operation that still runs.
    Running,
    /// One server operation that finished.
    Finished,
}

impl RowState {
    /// Returns the text of the state column.
    #[must_use]
    pub(super) const fn label(self) -> &'static str {
        match self {
            Self::Running => RUNNING_LABEL,
            Self::Finished => DONE_ICON,
        }
    }

    /// Returns the theme role of the state column.
    #[must_use]
    pub(super) const fn role(self) -> ThemeRole {
        match self {
            Self::Running => ThemeRole::NotificationRunning,
            Self::Finished => ThemeRole::NotificationDone,
        }
    }
}

/// One row that the overlay paints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum NotificationRow<'a> {
    /// One item row: the state, the message, and the reported completion.
    Item {
        /// The state column of the row.
        state: RowState,
        /// The message of the item.
        message: &'a str,
        /// The completion that the server reported, in percent.
        percentage: Option<ProgressPercentage>,
    },
    /// The title row of one group, with the spinner after the title.
    Group {
        /// The title of the group.
        title: &'a str,
        /// The spinner frame, or `None` while the group holds no running item.
        spinner: Option<&'static str>,
    },
}

/// The animated spinner of the board.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Spinner {
    /// The index of the visible frame.
    frame: usize,
    /// The elapsed time at which the board last advanced the frame.
    advanced_at: Duration,
}

/// The bounded notification state of one editor session.
///
/// The board holds one group for each language server. Every group holds
/// bounded items, and the board drops its oldest item above
/// [`NotificationSettings::rows_max`] rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct NotificationBoard {
    groups: Vec<NotificationGroup>,
    next_sequence: u64,
    spinner: Spinner,
}

impl Default for NotificationBoard {
    fn default() -> Self {
        Self {
            groups: Vec::new(),
            next_sequence: 0,
            spinner: Spinner {
                frame: 0,
                advanced_at: Duration::ZERO,
            },
        }
    }
}

impl NotificationBoard {
    /// Reports whether the board holds no row at all.
    pub(super) fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Returns the rows of the overlay, oldest group first.
    ///
    /// Each group contributes its items and then its own title row, which is
    /// the layout of the reference configuration.
    pub(super) fn rows(&self) -> Vec<NotificationRow<'_>> {
        let mut rows = Vec::with_capacity(self.row_count());
        for group in &self.groups {
            rows.extend(group.items.iter().map(NotificationItem::row));
            rows.push(NotificationRow::Group {
                title: group.title,
                spinner: group
                    .items
                    .iter()
                    .any(|item| item.finished_at().is_none())
                    .then(|| SPINNER_FRAMES[self.spinner.frame % SPINNER_FRAMES.len()]),
            });
        }
        rows
    }

    /// Applies one work-done progress report of one language server.
    ///
    /// A report of an attempt that a restart replaced changes nothing, and a
    /// report for a token that no `begin` created creates no item. Both keep the
    /// board free of state that no server owns.
    pub(super) fn report(
        &mut self,
        server: LanguageServerId,
        report: &ProgressReport,
        now: Duration,
        settings: NotificationSettings,
    ) -> Redraw {
        // A generation raise drops every row of the attempt that failed, which
        // is a visible change even when the report itself addresses no item.
        let mut invalidated = Redraw::Skipped;
        let index = match self.groups.iter().position(|group| group.server == server) {
            Some(index) => {
                let group = &mut self.groups[index];
                if report.generation < group.generation {
                    // A later attempt already reported, so this report belongs
                    // to a server that no longer runs.
                    return Redraw::Skipped;
                }
                if report.generation > group.generation {
                    // The new attempt assigns its own tokens, so no item of the
                    // attempt that failed stays addressable.
                    group.generation = report.generation;
                    if !group.items.is_empty() {
                        group.items.clear();
                        invalidated = Redraw::Needed;
                    }
                }
                index
            }
            None => {
                self.groups.push(NotificationGroup {
                    server,
                    title: report.server,
                    generation: report.generation,
                    items: Vec::new(),
                });
                self.groups.len() - 1
            }
        };
        let sequence = self.next_sequence;
        let group = &mut self.groups[index];
        let found = group
            .items
            .iter()
            .position(|item| item.token == report.token);
        let applied = match (&report.stage, found) {
            (
                ProgressStage::Begin {
                    title,
                    message,
                    percentage,
                },
                _,
            ) => {
                // A second `begin` for one token replaces the item, so one token
                // never owns two rows.
                if let Some(index) = found {
                    group.items.remove(index);
                }
                group.items.push(NotificationItem {
                    sequence,
                    message: message.clone().unwrap_or_else(|| title.clone()),
                    token: report.token.clone(),
                    state: ProgressState::Running {
                        percentage: *percentage,
                    },
                });
                Applied::Started
            }
            (
                ProgressStage::Report {
                    message,
                    percentage,
                },
                Some(index),
            ) => {
                let item = &mut group.items[index];
                if let Some(message) = message {
                    item.message.clone_from(message);
                }
                item.state = ProgressState::Running {
                    percentage: *percentage,
                };
                Applied::Changed
            }
            (ProgressStage::End { message }, Some(index)) => {
                let item = &mut group.items[index];
                if let Some(message) = message {
                    item.message.clone_from(message);
                }
                item.state = ProgressState::Finished { at: now };
                Applied::Changed
            }
            // A report or an end for a token that no `begin` created addresses
            // no item, so it changes nothing.
            (ProgressStage::Report { .. } | ProgressStage::End { .. }, None) => Applied::Ignored,
        };
        if applied == Applied::Ignored {
            self.drop_empty_groups();
            return invalidated;
        }
        if applied == Applied::Started {
            self.next_sequence = self.next_sequence.saturating_add(1);
            // A first running item starts its own spinner cycle, so the
            // animation never jumps on the frame that it appears.
            if self.running_items() == 1 {
                self.spinner.advanced_at = now;
            }
        }
        self.enforce_bound(settings);
        Redraw::Needed
    }

    /// Returns the elapsed time of the next change that no event causes.
    ///
    /// The time is the earlier of the next spinner frame and the removal of the
    /// oldest finished item. A board without a running item and without a
    /// finished item reports no time, so the event loop then waits for an event
    /// alone.
    pub(super) fn next_deadline(&self, settings: NotificationSettings) -> Option<Duration> {
        let spinner = (self.running_items() > 0).then(|| {
            self.spinner
                .advanced_at
                .saturating_add(frame_interval(settings))
        });
        let expiry = self
            .items()
            .filter_map(NotificationItem::finished_at)
            .map(|at| at.saturating_add(settings.done_ttl))
            .min();
        match (spinner, expiry) {
            (Some(spinner), Some(expiry)) => Some(spinner.min(expiry)),
            (Some(time), None) | (None, Some(time)) => Some(time),
            (None, None) => None,
        }
    }

    /// Applies the changes that the elapsed time alone causes.
    ///
    /// The call removes every item that passed its lifetime and advances the
    /// spinner at most one frame. Both leave a strictly later deadline behind,
    /// so the event loop always returns to waiting for an event.
    pub(super) fn advance(&mut self, now: Duration, settings: NotificationSettings) -> Redraw {
        let mut redraw = Redraw::Skipped;
        let expired = |item: &NotificationItem| {
            item.finished_at()
                .is_some_and(|at| at.saturating_add(settings.done_ttl) <= now)
        };
        for group in &mut self.groups {
            let before = group.items.len();
            group.items.retain(|item| !expired(item));
            if group.items.len() != before {
                redraw = Redraw::Needed;
            }
        }
        self.drop_empty_groups();
        if self.running_items() > 0
            && now
                >= self
                    .spinner
                    .advanced_at
                    .saturating_add(frame_interval(settings))
        {
            self.spinner.frame = self.spinner.frame.wrapping_add(1) % SPINNER_FRAMES.len();
            self.spinner.advanced_at = now;
            redraw = Redraw::Needed;
        }
        redraw
    }

    /// Returns every item of every group.
    fn items(&self) -> impl Iterator<Item = &NotificationItem> {
        self.groups.iter().flat_map(|group| group.items.iter())
    }

    /// Returns the number of items that still run.
    fn running_items(&self) -> usize {
        self.items()
            .filter(|item| item.finished_at().is_none())
            .count()
    }

    /// Returns the number of rows that the overlay paints.
    fn row_count(&self) -> usize {
        self.groups
            .iter()
            .map(|group| group.items.len().saturating_add(1))
            .sum()
    }

    /// Drops the oldest item until the board fits its row bound.
    fn enforce_bound(&mut self, settings: NotificationSettings) {
        while self.row_count() > settings.rows_max {
            let Some(oldest) = self.items().map(|item| item.sequence).min() else {
                break;
            };
            for group in &mut self.groups {
                group.items.retain(|item| item.sequence != oldest);
            }
            self.drop_empty_groups();
        }
        debug_assert!(
            self.row_count() <= settings.rows_max || self.groups.is_empty(),
            "the loop above drops items until the board fits its row bound"
        );
    }

    /// Removes every group that holds no item, so no empty title row stays.
    fn drop_empty_groups(&mut self) {
        self.groups.retain(|group| !group.items.is_empty());
    }
}

/// What one applied progress report did to the board.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Applied {
    /// The report created one item.
    Started,
    /// The report changed one existing item.
    Changed,
    /// The report addressed no item of the board.
    Ignored,
}

/// Returns the time between two spinner frames.
fn frame_interval(settings: NotificationSettings) -> Duration {
    let frames = u32::try_from(SPINNER_FRAMES.len()).unwrap_or(1);
    (settings.spinner_period / frames.max(1)).max(SPINNER_FRAME_MIN)
}
