//! The use order of the loaded buffers.
//!
//! The session records each buffer that becomes active, so an unload can show
//! the previously used buffer, as Neovim's `:bd` shows the alternate buffer.
//! The order is session-wide. Neovim keeps one alternate buffer for each
//! window. See `docs/files.md`.

use kvim_workspace::BufferId;

/// Loaded buffers from the least to the most recently used.
///
/// The order holds one entry for each loaded buffer at most. The session
/// forgets a buffer when it leaves the buffer list, so the size is bounded by
/// `BUFFERS_MAX` through the list itself. It is a structural bound, not a
/// tunable limit.
#[derive(Debug, Default)]
pub(super) struct RecentBuffers {
    order: Vec<BufferId>,
}

impl RecentBuffers {
    /// Marks one buffer as the most recently used.
    pub(super) fn touch(&mut self, id: BufferId) {
        self.forget(id);
        self.order.push(id);
    }

    /// Drops one buffer that left the buffer list.
    pub(super) fn forget(&mut self, id: BufferId) {
        self.order.retain(|recent| *recent != id);
    }

    /// Returns the most recently used buffer other than the named one.
    pub(super) fn most_recent_other(&self, than: BufferId) -> Option<BufferId> {
        self.order.iter().rev().copied().find(|id| *id != than)
    }

    /// Returns every recorded buffer, for invariant checks.
    pub(super) fn ids(&self) -> impl Iterator<Item = BufferId> + '_ {
        self.order.iter().copied()
    }
}
