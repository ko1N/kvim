//! One bounded strip of named surfaces that a host cycles through.
//!
//! A host names surfaces that a reader switches between: a chat, an editor, a
//! review, a diff of one range. A mapping for each one is a mapping to
//! memorise, so the strip lets a host enumerate them instead and walk them with
//! one pair of keys.
//!
//! The module owns no host value. A tab carries the opaque identity of the
//! host and one bounded label, and the host draws every cell through the render
//! callback. See `docs/windows.md`.
//!
//! The module is pure. It reads no clock, no filesystem, and no terminal.
//!
//! `crates/kvim-ui/examples/tab_strip.rs` is one complete host of one strip: it
//! opens three surfaces, walks them with one key, draws the band, and answers
//! the places that a mouse click would reach.

use std::fmt;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use thiserror::Error;

/// The largest number of tabs that one strip holds.
///
/// A strip that a reader walks with one key stays readable, and a host that
/// needs more surfaces than this needs a picker instead of a strip.
pub const TABS_MAX: usize = 32;

/// The largest number of characters that one tab label holds.
pub const TAB_LABEL_CHARS_MAX: usize = 32;

/// The number of cells that one tab pads on each side of its label.
const TAB_PADDING_CELLS: u16 = 1;

/// One tab of one strip: the host identity and its label.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Tab<'a, T> {
    /// The opaque identity that the host named.
    pub id: &'a T,
    /// The label that the strip draws.
    pub label: &'a str,
    /// Reports whether the tab owns the strip.
    pub active: bool,
}

/// The place of one tab inside one drawn strip.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TabPlacement<'a, T> {
    /// The tab that the placement draws.
    pub tab: Tab<'a, T>,
    /// The rectangle that the tab occupies.
    pub area: Rect,
}

/// Why one strip refused one tab.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum TabError {
    /// The strip already holds the largest number of tabs.
    #[error("the strip holds {max} tabs, which is its bound")]
    Limit {
        /// The bound that the strip holds.
        max: usize,
    },
    /// The label holds no character, or more than the bound.
    #[error("a tab label holds one to {max} characters, and this one holds {actual}")]
    Label {
        /// The number of characters that the label holds.
        actual: usize,
        /// The bound that a label stays inside.
        max: usize,
    },
}

/// One bounded strip of named surfaces.
///
/// The strip holds the order that the host opened, one active tab, and nothing
/// else. It owns no surface: a host reads [`TabStrip::active`] and draws that
/// surface itself.
///
/// # Examples
///
/// ```
/// use kvim_ui::TabStrip;
///
/// #[derive(Clone, Copy, Debug, Eq, PartialEq)]
/// enum Surface {
///     Chat,
///     Editor,
///     Review,
/// }
///
/// let mut tabs = TabStrip::default();
/// tabs.open(Surface::Chat, "Chat")?;
/// tabs.open(Surface::Editor, "Editor")?;
/// tabs.open(Surface::Review, "Review")?;
///
/// // The first tab that a strip opens owns it.
/// assert_eq!(tabs.active(), Some(&Surface::Chat));
///
/// // One key walks the surfaces instead of one mapping for each.
/// tabs.select_next();
/// assert_eq!(tabs.active(), Some(&Surface::Editor));
///
/// // The walk cycles, so the last tab reaches the first one.
/// tabs.select_previous();
/// tabs.select_previous();
/// assert_eq!(tabs.active(), Some(&Surface::Review));
///
/// // A host that closes a surface closes its tab. The strip keeps one active
/// // tab as long as it holds any, so the last tab falls back to its neighbour.
/// tabs.close(&Surface::Review);
/// assert_eq!(tabs.active(), Some(&Surface::Editor));
/// # Ok::<(), kvim_ui::TabError>(())
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TabStrip<T> {
    tabs: Vec<Entry<T>>,
    active: Option<usize>,
}

/// One stored tab.
#[derive(Clone, Debug, Eq, PartialEq)]
struct Entry<T> {
    id: T,
    label: Box<str>,
}

impl<T> Default for TabStrip<T> {
    /// Returns one strip without a tab.
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active: None,
        }
    }
}

impl<T: PartialEq> TabStrip<T> {
    /// Opens one tab, or renames the tab that already holds this identity.
    ///
    /// The first tab of one strip becomes the active tab, so a strip that holds
    /// a tab always holds an active one.
    ///
    /// # Errors
    ///
    /// Returns [`TabError::Limit`] when the strip already holds [`TABS_MAX`]
    /// tabs, and [`TabError::Label`] for a label outside its bound.
    pub fn open(&mut self, id: T, label: &str) -> Result<(), TabError> {
        let characters = label.chars().count();
        if characters == 0 || characters > TAB_LABEL_CHARS_MAX {
            return Err(TabError::Label {
                actual: characters,
                max: TAB_LABEL_CHARS_MAX,
            });
        }
        if let Some(entry) = self.tabs.iter_mut().find(|entry| entry.id == id) {
            entry.label = label.into();
            return Ok(());
        }
        if self.tabs.len() >= TABS_MAX {
            return Err(TabError::Limit { max: TABS_MAX });
        }
        self.tabs.push(Entry {
            id,
            label: label.into(),
        });
        if self.active.is_none() {
            self.active = Some(0);
        }
        Ok(())
    }

    /// Closes the tab of one identity and reports whether the strip held it.
    ///
    /// The tab that follows the closed one becomes active. The last tab of the
    /// strip has no follower, so it falls back to the tab that becomes the last
    /// one, and closing the only tab leaves the strip empty.
    pub fn close(&mut self, id: &T) -> bool {
        let Some(index) = self.tabs.iter().position(|entry| &entry.id == id) else {
            return false;
        };
        self.tabs.remove(index);
        self.active = match self.active {
            _ if self.tabs.is_empty() => None,
            Some(active) if active > index => Some(active - 1),
            Some(active) => Some(active.min(self.tabs.len() - 1)),
            None => None,
        };
        true
    }

    /// Makes the tab of one identity the active tab.
    ///
    /// Returns `false` when the strip holds no such tab, and changes nothing.
    pub fn select(&mut self, id: &T) -> bool {
        let Some(index) = self.tabs.iter().position(|entry| &entry.id == id) else {
            return false;
        };
        self.active = Some(index);
        true
    }

    /// Makes the next tab active, and cycles at the last one.
    ///
    /// Returns `false` for a strip that holds fewer than two tabs, because the
    /// walk then reaches nothing new.
    pub fn select_next(&mut self) -> bool {
        self.walk(1)
    }

    /// Makes the previous tab active, and cycles at the first one.
    pub fn select_previous(&mut self) -> bool {
        self.walk(-1)
    }

    /// Walks the active tab by one step in either direction.
    fn walk(&mut self, step: isize) -> bool {
        if self.tabs.len() < 2 {
            return false;
        }
        let count = self.tabs.len();
        let active = self.active.unwrap_or(0);
        let moved = isize::try_from(active).unwrap_or(0) + step;
        let count_step = isize::try_from(count).unwrap_or(1);
        // The walk cycles, so the value stays inside the strip in either
        // direction without a branch for each end.
        self.active = Some(usize::try_from(moved.rem_euclid(count_step)).unwrap_or(0));
        true
    }
}

impl<T> TabStrip<T> {
    /// Returns the identity of the active tab.
    #[must_use]
    pub fn active(&self) -> Option<&T> {
        self.active
            .and_then(|index| self.tabs.get(index))
            .map(|entry| &entry.id)
    }

    /// Returns every tab, in the order that the host opened them.
    pub fn tabs(&self) -> impl Iterator<Item = Tab<'_, T>> {
        self.tabs.iter().enumerate().map(|(index, entry)| Tab {
            id: &entry.id,
            label: &entry.label,
            active: self.active == Some(index),
        })
    }

    /// Returns the number of tabs that the strip holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// Reports whether the strip holds no tab.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// Returns the place of every tab that fits inside one rectangle.
    ///
    /// A tab that the rectangle cannot hold whole receives no placement, so a
    /// host never draws half of a label. The strip reports the places instead
    /// of drawing them, so a host can also answer a mouse click with them.
    #[must_use]
    pub fn placements(&self, area: Rect) -> Vec<TabPlacement<'_, T>> {
        let mut placements = Vec::with_capacity(self.tabs.len());
        let mut x = area.x;
        for tab in self.tabs() {
            let width = tab_cells(tab.label);
            if x.saturating_add(width) > area.x.saturating_add(area.width) {
                break;
            }
            placements.push(TabPlacement {
                tab,
                area: Rect::new(x, area.y, width, 1),
            });
            x = x.saturating_add(width);
        }
        placements
    }

    /// Draws every visible tab through one host callback.
    ///
    /// The callback receives one placement at a time and paints the cells of
    /// that tab. The strip names no glyph, no color, and no border, because the
    /// host owns every presentation value.
    pub fn render<F>(&self, target: &mut Buffer, area: Rect, mut draw: F)
    where
        F: FnMut(&mut Buffer, &TabPlacement<'_, T>),
    {
        if area.is_empty() {
            return;
        }
        for placement in &self.placements(area) {
            draw(target, placement);
        }
    }
}

impl<T> fmt::Display for TabStrip<T> {
    /// Writes the number of tabs and the place of the active one.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.active {
            Some(active) => write!(formatter, "tab {} of {}", active + 1, self.tabs.len()),
            None => formatter.write_str("no tab"),
        }
    }
}

/// Returns the number of cells that one tab occupies.
fn tab_cells(label: &str) -> u16 {
    let characters = u16::try_from(label.chars().count()).unwrap_or(u16::MAX);
    characters.saturating_add(TAB_PADDING_CELLS.saturating_mul(2))
}

#[cfg(test)]
#[path = "tabs_tests.rs"]
mod tests;
