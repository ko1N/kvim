//! Terminal-neutral pointer input values.

use thiserror::Error;

/// The maximum number of pointer events that an adapter may coalesce.
pub const POINTER_EVENTS_COALESCE_MAX: u8 = 32;

/// A terminal or rendered-surface cell position.
///
/// This position is distinct from a source-text character or byte position.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CellPosition {
    column: u16,
    row: u16,
}

impl CellPosition {
    /// Creates a position from zero-based cell coordinates.
    #[must_use]
    pub const fn new(column: u16, row: u16) -> Self {
        Self { column, row }
    }

    /// Returns the zero-based cell column.
    #[must_use]
    pub const fn column(self) -> u16 {
        self.column
    }

    /// Returns the zero-based cell row.
    #[must_use]
    pub const fn row(self) -> u16 {
        self.row
    }
}

/// Non-Shift modifiers reported with a pointer event.
///
/// Terminal adapters omit Shift-modified pointer input so the terminal can own
/// native text selection.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointerModifiers {
    control: bool,
    alt: bool,
    super_key: bool,
}

impl PointerModifiers {
    /// Creates modifiers from terminal-neutral modifier states.
    #[must_use]
    pub const fn new(control: bool, alt: bool, super_key: bool) -> Self {
        Self {
            control,
            alt,
            super_key,
        }
    }

    /// Returns whether Control was held.
    #[must_use]
    pub const fn control(self) -> bool {
        self.control
    }

    /// Returns whether Alt was held.
    #[must_use]
    pub const fn alt(self) -> bool {
        self.alt
    }

    /// Returns whether Super was held.
    #[must_use]
    pub const fn super_key(self) -> bool {
        self.super_key
    }
}

/// A pointer button with a supported identity.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PointerButton {
    /// The primary button.
    Left,
    /// The secondary button.
    Right,
    /// The middle button.
    Middle,
}

/// A wheel direction in cell coordinates.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PointerWheelDirection {
    /// Scroll toward smaller row values.
    Up,
    /// Scroll toward larger row values.
    Down,
    /// Scroll toward smaller column values.
    Left,
    /// Scroll toward larger column values.
    Right,
}

/// The reason a wheel tick count is invalid.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PointerWheelError {
    /// The wheel action carries no raw tick.
    #[error("the pointer wheel action must contain at least one tick")]
    ZeroTicks,
    /// The wheel action exceeds the published coalescing bound.
    #[error(
        "the pointer wheel action has {ticks} ticks, above the maximum of {POINTER_EVENTS_COALESCE_MAX}"
    )]
    TooManyTicks {
        /// The rejected tick count.
        ticks: u8,
    },
}

/// A bounded coalesced wheel action.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointerWheel {
    direction: PointerWheelDirection,
    ticks: u8,
}

impl PointerWheel {
    /// Creates a wheel action with a bounded nonzero tick count.
    ///
    /// # Errors
    ///
    /// Returns [`PointerWheelError::ZeroTicks`] when `ticks` is zero. Returns
    /// [`PointerWheelError::TooManyTicks`] when `ticks` exceeds
    /// [`POINTER_EVENTS_COALESCE_MAX`].
    pub const fn new(
        direction: PointerWheelDirection,
        ticks: u8,
    ) -> Result<Self, PointerWheelError> {
        if ticks == 0 {
            return Err(PointerWheelError::ZeroTicks);
        }
        if ticks > POINTER_EVENTS_COALESCE_MAX {
            return Err(PointerWheelError::TooManyTicks { ticks });
        }
        Ok(Self { direction, ticks })
    }

    /// Returns the wheel direction.
    #[must_use]
    pub const fn direction(self) -> PointerWheelDirection {
        self.direction
    }

    /// Returns the number of raw wheel ticks.
    #[must_use]
    pub const fn ticks(self) -> u8 {
        self.ticks
    }
}

/// One terminal-neutral pointer action.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PointerAction {
    /// A button press.
    Press(PointerButton),
    /// A button release.
    Release(PointerButton),
    /// Movement while a button is held.
    Drag(PointerButton),
    /// Movement without a reported button.
    Motion,
    /// One or more bounded wheel ticks.
    Wheel(PointerWheel),
}

/// One terminal-neutral pointer event.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PointerEvent {
    position: CellPosition,
    modifiers: PointerModifiers,
    action: PointerAction,
}

impl PointerEvent {
    /// Creates a terminal-neutral pointer event.
    #[must_use]
    pub const fn new(
        position: CellPosition,
        modifiers: PointerModifiers,
        action: PointerAction,
    ) -> Self {
        Self {
            position,
            modifiers,
            action,
        }
    }

    /// Returns the reported cell position.
    #[must_use]
    pub const fn position(self) -> CellPosition {
        self.position
    }

    /// Returns the non-Shift modifiers.
    #[must_use]
    pub const fn modifiers(self) -> PointerModifiers {
        self.modifiers
    }

    /// Returns the pointer action.
    #[must_use]
    pub const fn action(self) -> PointerAction {
        self.action
    }
}
