//! Generic source-presentation state and painting.
//!
//! The model contains only validated visible values. Painting performs no I/O.

use kvim_path::WorktreeRelativePath;
use ratatui::{buffer::Buffer, layout::Rect};

use super::theme::{Theme, ThemeRole};

/// Why a generic source-presentation operation changed no presentation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourcePresentationRefusal {
    /// The editor already closed.
    NoEditor,
    /// Another active file holds unsaved text.
    DifferentDirtyBuffer,
    /// Another file operation already uses the bounded file lane.
    Busy,
    /// A range leaves the current buffer.
    RangeOutsideBuffer,
    /// Selection already names the first annotation.
    AtFirst,
    /// Selection already names the last annotation.
    AtLast,
    /// No presentation exists.
    NoPresentation,
    /// The requested file could not be opened.
    OpenFailed,
}

/// One private validated annotation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceAnnotation {
    first_line: usize,
    last_line: usize,
    message: String,
}

impl SourceAnnotation {
    /// Creates a zero-based inclusive annotation from facade-validated values.
    #[must_use]
    pub fn new(first_line: usize, last_line: usize, message: String) -> Self {
        debug_assert!(
            first_line <= last_line,
            "the facade validates ordered ranges"
        );
        Self {
            first_line,
            last_line,
            message,
        }
    }

    /// Returns the first zero-based line.
    #[must_use]
    pub const fn first_line(&self) -> usize {
        self.first_line
    }

    /// Returns the last zero-based line.
    #[must_use]
    pub const fn last_line(&self) -> usize {
        self.last_line
    }

    /// Returns the annotation message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One private validated source presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourcePresentation {
    path: WorktreeRelativePath,
    annotations: Vec<SourceAnnotation>,
    selected: usize,
}

impl SourcePresentation {
    /// Creates a presentation from facade-validated values.
    #[must_use]
    pub fn new(path: WorktreeRelativePath, annotations: Vec<SourceAnnotation>) -> Self {
        debug_assert!(
            !annotations.is_empty(),
            "the facade requires one annotation"
        );
        Self {
            path,
            annotations,
            selected: 0,
        }
    }

    /// Returns the contained path.
    #[must_use]
    pub const fn path(&self) -> &WorktreeRelativePath {
        &self.path
    }

    /// Returns the selected annotation.
    #[must_use]
    pub fn selected(&self) -> &SourceAnnotation {
        self.annotations
            .get(self.selected)
            .expect("construction and navigation keep selection in bounds")
    }

    /// Returns the zero-based selected index.
    #[must_use]
    pub const fn selected_index(&self) -> usize {
        self.selected
    }

    /// Returns the annotation count.
    #[must_use]
    pub fn annotation_count(&self) -> usize {
        self.annotations.len()
    }

    /// Reports whether every annotation fits the given line count.
    #[must_use]
    pub fn fits(&self, line_count: usize) -> bool {
        self.annotations
            .iter()
            .all(|annotation| annotation.last_line < line_count)
    }

    /// Selects the next annotation without wrapping.
    pub fn select_next(&mut self) -> Result<(), SourcePresentationRefusal> {
        if self.selected + 1 >= self.annotations.len() {
            return Err(SourcePresentationRefusal::AtLast);
        }
        self.selected += 1;
        Ok(())
    }

    /// Selects the previous annotation without wrapping.
    pub fn select_previous(&mut self) -> Result<(), SourcePresentationRefusal> {
        if self.selected == 0 {
            return Err(SourcePresentationRefusal::AtFirst);
        }
        self.selected -= 1;
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(crate) struct SourcePresentationView<'a> {
    pub(crate) message: &'a str,
    pub(crate) current: usize,
    pub(crate) total: usize,
}

pub(crate) fn source_area(area: Rect, has_presentation: bool) -> (Rect, Option<Rect>) {
    if !has_presentation || area.height < 2 {
        return (area, None);
    }
    let source = Rect::new(area.x, area.y, area.width, area.height - 1);
    let panel = Rect::new(area.x, source.bottom(), area.width, 1);
    (source, Some(panel))
}

pub(crate) fn render_panel(
    target: &mut Buffer,
    panel: Rect,
    theme: Theme,
    view: SourcePresentationView<'_>,
) {
    if panel.is_empty() {
        return;
    }
    let style = theme.style(ThemeRole::SourcePresentationPanel);
    target.set_style(panel, style);
    let counter = format!("{}/{}", view.current, view.total);
    let counter_width = u16::try_from(counter.chars().count()).unwrap_or(u16::MAX);
    if counter_width >= panel.width {
        target.set_stringn(panel.x, panel.y, &counter, usize::from(panel.width), style);
        return;
    }
    let counter_x = panel.right().saturating_sub(counter_width);
    let message_width = usize::from(counter_x.saturating_sub(panel.x).saturating_sub(1));
    target.set_stringn(panel.x, panel.y, view.message, message_width, style);
    target.set_stringn(
        counter_x,
        panel.y,
        &counter,
        usize::from(counter_width),
        style,
    );
}

#[cfg(test)]
#[path = "source_presentation_tests.rs"]
mod tests;
