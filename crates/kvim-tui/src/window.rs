//! The kvim adapter over the generic window tree.
//!
//! [`kvim_ui::WindowTree`] owns the topology, the focus, the sidebars, and the
//! geometry. This module adds the parts that belong to the editor: the buffer
//! that each window shows, the view of each window, the split and resize
//! settings, and the semantic window commands.
//!
//! The module is deterministic and pure. It reads no clock, no filesystem, and
//! no terminal. It holds no buffer text and no color. See `docs/windows.md`.

use std::num::NonZeroU16;

use ratatui::layout::Rect;

use kvim_editor::{Viewport, WindowState};
use kvim_input::Command;
use kvim_settings::{HorizontalSplitPlacement, VerticalSplitPlacement, WindowSettings};
use kvim_ui::{
    ChildSide, CloseOutcome, Direction, LayoutChange, Orientation, RegionKind, Sidebar,
    SidebarSide, SplitError, WindowId, WindowLayout, WindowLimits, WindowTree,
};
use kvim_workspace::BufferId;

use super::buffer_view::WINBAR_ROWS;
use super::jumps::JumpList;

/// The view that one window keeps for its buffer.
///
/// The tree holds the buffer identity only, so the adapter owns the cursor, the
/// selection anchor, the viewport, and the jump list of each window. Two windows
/// that show one buffer therefore move, scroll, and walk their recorded
/// positions independently. See `docs/windows.md`.
#[derive(Clone, Debug, Eq, PartialEq)]
struct WindowView {
    id: WindowId,
    state: WindowState,
    jumps: JumpList,
}

impl WindowView {
    /// Returns the view that a new window starts with.
    fn new(id: WindowId) -> Self {
        Self {
            id,
            state: WindowState::new(Viewport::new(NonZeroU16::MIN, NonZeroU16::MIN)),
            jumps: JumpList::default(),
        }
    }
}

/// The rule that the adaptive split command applies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AdaptiveSplit {
    /// Select a vertical split for a wide window.
    Normal,
    /// Select a horizontal split for a wide window.
    Inverse,
}

/// The result of one window command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowOutcome {
    /// The command does not address the window tree.
    Ignored,
    /// The command changed the window tree, the focus, or the rectangles.
    Changed,
    /// The command addressed the window tree and changed nothing.
    Unchanged,
    /// The close command reached the last window.
    LastWindow,
}

/// The window tree, the buffer of each window, and the view of each window.
///
/// Every mutating operation recomputes the layout, so [`Windows::layout`]
/// always describes the current tree.
///
/// # Examples
///
/// ```
/// use ratatui::layout::Rect;
///
/// use kvim_settings::WindowSettings;
/// use kvim_tui::{Direction, LayoutChange, Orientation, Windows};
/// use kvim_workspace::BufferId;
///
/// let terminal = Rect::new(0, 0, 120, 40);
/// let mut windows = Windows::new(BufferId::new(1), terminal, WindowSettings::default());
///
/// // A vertical split opens the new window to the right and focuses it.
/// let right = windows.split(Orientation::Vertical).expect("the terminal is wide");
/// assert_eq!(windows.focused_window(), right);
/// assert_eq!(windows.window_count(), 2);
///
/// // Directional focus uses the rectangles, not the tree order.
/// assert_eq!(windows.focus_direction(Direction::Left), LayoutChange::Changed);
/// assert_eq!(windows.focus_direction(Direction::Left), LayoutChange::Unchanged);
/// ```
#[derive(Clone, Debug)]
pub struct Windows {
    tree: WindowTree<BufferId>,
    /// One view for each leaf window. The list follows the tree, so it never
    /// grows past the window limit of the tree.
    views: Vec<WindowView>,
    settings: WindowSettings,
}

impl Windows {
    /// Creates a tree with one window that shows the named buffer.
    #[must_use]
    pub fn new(buffer: BufferId, terminal: Rect, settings: WindowSettings) -> Self {
        let tree = WindowTree::new(buffer, terminal, window_limits(settings));
        let views = vec![WindowView::new(tree.focused_window())];
        let mut windows = Self {
            tree,
            views,
            settings,
        };
        windows.sync_viewports();
        windows
    }

    /// Returns the rectangle of every visible window and sidebar.
    #[must_use]
    pub const fn layout(&self) -> &WindowLayout {
        self.tree.layout()
    }

    /// Returns the terminal rectangle that produced the current layout.
    #[must_use]
    pub const fn terminal(&self) -> Rect {
        self.tree.area()
    }

    /// Returns the split, focus, and resize settings of the tree.
    #[must_use]
    pub const fn settings(&self) -> WindowSettings {
        self.settings
    }

    /// Returns the focused editor window.
    ///
    /// The value stays valid while a sidebar holds the focus.
    #[must_use]
    pub const fn focused_window(&self) -> WindowId {
        self.tree.focused_window()
    }

    /// Returns the region that holds the input focus.
    #[must_use]
    pub fn focused_region(&self) -> WindowId {
        self.tree.focused_region()
    }

    /// Returns the number of leaf windows in the tree.
    #[must_use]
    pub fn window_count(&self) -> usize {
        self.tree.window_count()
    }

    /// Returns every window in tree order.
    ///
    /// The list holds every window identity, including a window that the
    /// current layout hides.
    #[must_use]
    pub fn window_ids(&self) -> Vec<WindowId> {
        self.tree.window_ids()
    }

    /// Returns the buffer that the named window shows.
    #[must_use]
    pub fn buffer(&self, id: WindowId) -> Option<BufferId> {
        self.tree.surface(id).copied()
    }

    /// Points the named window at another buffer.
    ///
    /// Returns `false` when the tree does not hold the window.
    pub fn set_buffer(&mut self, id: WindowId, buffer: BufferId) -> bool {
        self.tree.replace_surface(id, buffer).is_ok()
    }

    /// Returns the cursor, the selection anchor, and the viewport of one window.
    #[must_use]
    pub fn state(&self, id: WindowId) -> Option<WindowState> {
        self.views
            .iter()
            .find(|view| view.id == id)
            .map(|view| view.state)
    }

    /// Returns the state of the named window for one change.
    ///
    /// A layout change keeps both scroll offsets and only replaces the window
    /// size. The caller holds the buffer, so the caller reconciles the viewport
    /// with the scroll margin after that change.
    pub fn state_mut(&mut self, id: WindowId) -> Option<&mut WindowState> {
        self.views
            .iter_mut()
            .find(|view| view.id == id)
            .map(|view| &mut view.state)
    }

    /// Returns the jump list of the named window for one change.
    ///
    /// Every push and every step changes the list, so the adapter hands out no
    /// shared reference to it. A closed window drops its list with its view, and
    /// two windows therefore walk independent histories. See `docs/windows.md`.
    pub(super) fn jumps_mut(&mut self, id: WindowId) -> Option<&mut JumpList> {
        self.views
            .iter_mut()
            .find(|view| view.id == id)
            .map(|view| &mut view.jumps)
    }

    /// Returns the viewport of the named window.
    #[must_use]
    pub fn viewport(&self, id: WindowId) -> Option<Viewport> {
        self.state(id).map(WindowState::viewport)
    }

    /// Recomputes the layout for a new terminal size.
    ///
    /// The tree structure and every window identity stay unchanged.
    pub fn set_terminal(&mut self, terminal: Rect) {
        self.tree.set_area(terminal);
        self.sync_viewports();
    }

    /// Returns the sidebar at the named edge.
    #[must_use]
    pub const fn sidebar(&self, side: SidebarSide) -> Option<Sidebar> {
        self.tree.sidebar(side)
    }

    /// Creates or replaces the sidebar at the named edge.
    ///
    /// Returns `None` when the tree cannot issue another region identity.
    pub fn open_sidebar(&mut self, side: SidebarSide, width_cells: u16) -> Option<WindowId> {
        let id = self.tree.open_sidebar(side, width_cells).ok()?;
        self.sync_viewports();
        Some(id)
    }

    /// Shows or hides the sidebar at the named edge.
    ///
    /// Hiding a sidebar that holds the focus returns the focus to the
    /// previously focused editor window.
    pub fn set_sidebar_visible(&mut self, side: SidebarSide, visible: bool) -> LayoutChange {
        let change = self.tree.set_sidebar_visible(side, visible);
        if change == LayoutChange::Changed {
            self.sync_viewports();
        }
        change
    }

    /// Moves the focus to the named region.
    ///
    /// Returns [`LayoutChange::Unchanged`] when the layout does not show the
    /// region, so a hidden sidebar never holds the focus.
    pub fn focus_region(&mut self, id: WindowId) -> LayoutChange {
        self.tree
            .focus_region(id)
            .unwrap_or(LayoutChange::Unchanged)
    }

    /// Moves the focus to the nearest region on the named side.
    ///
    /// The move compares layout rectangles, not tree order. The focus stays
    /// unchanged when no region touches that side.
    pub fn focus_direction(&mut self, direction: Direction) -> LayoutChange {
        self.tree.focus_direction(direction)
    }

    /// Splits the focused window and focuses the new window.
    ///
    /// The new window shows the same buffer, and it copies the cursor, the
    /// selection anchor, the viewport, and the jump list of the source window,
    /// so it opens at the same place and returns to the same recorded
    /// positions. Both lists grow apart from that moment, because each window
    /// owns its own. The settings decide which side receives it.
    ///
    /// # Errors
    ///
    /// Returns [`SplitError`] when the tree reaches a limit, or when the
    /// focused rectangle cannot show both new windows.
    pub fn split(&mut self, orientation: Orientation) -> Result<WindowId, SplitError> {
        let new_side = match orientation {
            Orientation::Horizontal => match self.settings.horizontal_split_placement {
                HorizontalSplitPlacement::Above => ChildSide::First,
                HorizontalSplitPlacement::Below => ChildSide::Second,
            },
            Orientation::Vertical => match self.settings.vertical_split_placement {
                VerticalSplitPlacement::Left => ChildSide::First,
                VerticalSplitPlacement::Right => ChildSide::Second,
            },
        };
        let source = self.tree.focused_window();
        let id = self.tree.split(orientation, new_side)?;
        // The new window opens at the same place as its source window.
        let source_view = self.views.iter().find(|view| view.id == source);
        let mut view = source_view.cloned().unwrap_or_else(|| {
            debug_assert!(false, "every leaf window of the tree owns one view");
            WindowView::new(id)
        });
        view.id = id;
        self.views.push(view);
        self.sync_viewports();
        Ok(id)
    }

    /// Returns the orientation that the adaptive split command selects.
    ///
    /// One rule comes before the ratio: a terminal that holds exactly one
    /// editor window always selects a vertical split, because a full-width
    /// terminal would otherwise divide into two short windows. The inverse
    /// command mirrors both the exception and the ratio.
    #[must_use]
    pub fn adaptive_orientation(&self, sense: AdaptiveSplit) -> Orientation {
        let ratio = self.settings.adaptive_split_ratio.get();
        let normal = if self.window_count() == 1 {
            Orientation::Vertical
        } else {
            let area = self
                .layout()
                .area(self.focused_window())
                .unwrap_or(self.terminal());
            if f32::from(area.width) > f32::from(area.height) * ratio {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            }
        };
        match sense {
            AdaptiveSplit::Normal => normal,
            AdaptiveSplit::Inverse => normal.inverse(),
        }
    }

    /// Splits the focused window with the adaptive rule.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Windows::split`].
    pub fn split_adaptive(&mut self, sense: AdaptiveSplit) -> Result<WindowId, SplitError> {
        self.split(self.adaptive_orientation(sense))
    }

    /// Closes the focused region.
    ///
    /// Closing a focused sidebar hides it and returns the focus to the
    /// previously focused editor window. Closing a window replaces its parent
    /// split node with the remaining sibling and focuses the first window of
    /// that sibling.
    pub fn close_focused(&mut self) -> CloseOutcome {
        let outcome = self.tree.close_focused();
        if let CloseOutcome::Closed(_) = outcome {
            // A closed window discards its view with it.
            let live = self.tree.window_ids();
            self.views.retain(|view| live.contains(&view.id));
            self.sync_viewports();
        }
        outcome
    }

    /// Moves one shared edge by the configured resize step.
    ///
    /// The command names the direction that the edge moves, not a size change.
    /// The far edge wins: a horizontal command prefers the right edge, and a
    /// vertical command prefers the bottom edge. See `docs/windows.md`.
    pub fn resize(&mut self, direction: Direction) -> LayoutChange {
        let change = self.tree.resize(direction, self.settings.resize_step_cells);
        if change == LayoutChange::Changed {
            self.sync_viewports();
        }
        change
    }

    /// Applies one semantic window command.
    ///
    /// The method ignores every command that does not address the window tree,
    /// so the event loop passes each command through one call.
    pub fn apply(&mut self, command: Command) -> WindowOutcome {
        match command {
            Command::FocusWindowLeft => self.focus_direction(Direction::Left).into(),
            Command::FocusWindowDown => self.focus_direction(Direction::Down).into(),
            Command::FocusWindowUp => self.focus_direction(Direction::Up).into(),
            Command::FocusWindowRight => self.focus_direction(Direction::Right).into(),
            Command::ResizeWindowLeft => self.resize(Direction::Left).into(),
            Command::ResizeWindowDown => self.resize(Direction::Down).into(),
            Command::ResizeWindowUp => self.resize(Direction::Up).into(),
            Command::ResizeWindowRight => self.resize(Direction::Right).into(),
            Command::SplitAdaptive => self.split_outcome(AdaptiveSplit::Normal),
            Command::SplitInverseAdaptive => self.split_outcome(AdaptiveSplit::Inverse),
            Command::CloseWindow => match self.close_focused() {
                CloseOutcome::Closed(_) => WindowOutcome::Changed,
                CloseOutcome::LastWindow => WindowOutcome::LastWindow,
            },
            _ => WindowOutcome::Ignored,
        }
    }

    fn split_outcome(&mut self, sense: AdaptiveSplit) -> WindowOutcome {
        match self.split_adaptive(sense) {
            Ok(_) => WindowOutcome::Changed,
            Err(_) => WindowOutcome::Unchanged,
        }
    }

    /// Resizes every visible viewport to the text rows of its window rectangle.
    ///
    /// The winbar row belongs to the window rectangle but shows no buffer line,
    /// so a viewport over the complete rectangle would reserve one row that the
    /// renderer never paints with text. The adapter therefore removes that row
    /// here. The gutter width depends on the buffer, which this module never
    /// holds, so the caller narrows the width after this call.
    ///
    /// A size change keeps both scroll offsets, so a split, a close, and a
    /// terminal resize never move the reader back to the start of the buffer.
    /// The caller holds the buffer and the cursor, so the caller reconciles the
    /// viewport with the scroll margin after the change.
    fn sync_viewports(&mut self) {
        let sizes: Vec<(WindowId, u16, u16)> = self
            .tree
            .layout()
            .regions()
            .iter()
            .filter(|region| region.kind == RegionKind::Surface)
            .map(|region| {
                (
                    region.id,
                    region.area.width,
                    region.area.height.saturating_sub(WINBAR_ROWS),
                )
            })
            .collect();
        for (id, width, height) in sizes {
            let (Some(width), Some(height)) = (NonZeroU16::new(width), NonZeroU16::new(height))
            else {
                continue;
            };
            let Some(state) = self.state_mut(id) else {
                continue;
            };
            let viewport = state.viewport();
            if viewport.height_rows() != height || viewport.width_cells() != width {
                *state = state.resized(height, width);
            }
        }
    }
}

/// Returns the minimum window dimensions that the settings request.
///
/// The settings hold plain cell counts, and a zero would remove the minimum, so
/// the conversion keeps one usable cell in each direction.
fn window_limits(settings: WindowSettings) -> WindowLimits {
    WindowLimits::new(
        NonZeroU16::new(settings.min_window_width_cells).unwrap_or(NonZeroU16::MIN),
        NonZeroU16::new(settings.min_window_height_rows).unwrap_or(NonZeroU16::MIN),
    )
}

impl From<LayoutChange> for WindowOutcome {
    fn from(change: LayoutChange) -> Self {
        match change {
            LayoutChange::Changed => Self::Changed,
            LayoutChange::Unchanged => Self::Unchanged,
        }
    }
}
