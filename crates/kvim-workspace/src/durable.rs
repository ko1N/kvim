//! Durable filesystem operation outcomes and failure reports.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(test)]
use std::cell::Cell;

/// The maximum number of restoration or cleanup failures in one report.
pub const RECOVERY_FAILURES_MAX: usize = 128;

/// The maximum number of paths that one indeterminate report can name.
pub const INDETERMINATE_PATHS_MAX: usize = 128;

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FailurePoint {
    SaveAfterRename,
    SaveDirectPartial,
    SaveDirectSync,
    MutationAfterRename,
    MutationAfterRenameAndRestore,
    MutationRestore,
    MutationCleanup,
}

#[cfg(test)]
thread_local! {
    static FAILURE_POINT: Cell<Option<FailurePoint>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn inject_failure(point: FailurePoint) {
    FAILURE_POINT.with(|current| current.set(Some(point)));
}

#[cfg(test)]
pub(crate) fn fail_at(point: FailurePoint) -> io::Result<()> {
    FAILURE_POINT.with(|current| {
        if current.get() == Some(point)
            || current.get() == Some(FailurePoint::MutationAfterRenameAndRestore)
                && matches!(
                    point,
                    FailurePoint::MutationAfterRename | FailurePoint::MutationRestore
                )
        {
            if current.get() != Some(FailurePoint::MutationAfterRenameAndRestore)
                || point == FailurePoint::MutationRestore
            {
                current.set(None);
            }
            Err(io::Error::other(format!("injected failure at {point:?}")))
        } else {
            Ok(())
        }
    })
}

#[cfg(not(test))]
#[inline]
pub(crate) fn fail_at(_point: FailurePoint) -> io::Result<()> {
    Ok(())
}

/// One failed attempt to restore or clean up a filesystem path.
#[derive(Debug)]
pub struct RecoveryFailure {
    path: PathBuf,
    action: RecoveryAction,
    source: io::Error,
}

impl RecoveryFailure {
    /// Creates one recovery failure with its preserved source.
    pub fn new(path: PathBuf, action: RecoveryAction, source: io::Error) -> Self {
        Self {
            path,
            action,
            source,
        }
    }

    /// Returns the path that could not be restored or cleaned up.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the failed recovery action.
    #[must_use]
    pub const fn action(&self) -> RecoveryAction {
        self.action
    }
}

impl fmt::Display for RecoveryFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "could not {} {}",
            self.action,
            self.path.display()
        )
    }
}

impl Error for RecoveryFailure {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.source)
    }
}

/// The restoration or cleanup action that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAction {
    /// Remove an entry created only for staging.
    RemoveTemporary,
    /// Restore an entry to its original path.
    RestoreOriginal,
    /// Restore an entry that an overwrite displaced.
    RestoreDestination,
    /// Remove an entry after its durable commit.
    RemoveParked,
}

impl fmt::Display for RecoveryAction {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::RemoveTemporary => "remove temporary entry",
            Self::RestoreOriginal => "restore original entry",
            Self::RestoreDestination => "restore replaced entry",
            Self::RemoveParked => "remove parked entry",
        })
    }
}

/// A public indeterminate report exceeded one of its collection bounds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndeterminateLimitError {
    recovery_failures: usize,
    affected_paths: usize,
}

impl IndeterminateLimitError {
    /// Returns the supplied number of recovery failures.
    #[must_use]
    pub const fn recovery_failures(self) -> usize {
        self.recovery_failures
    }

    /// Returns the supplied number of affected paths.
    #[must_use]
    pub const fn affected_paths(self) -> usize {
        self.affected_paths
    }
}

impl fmt::Display for IndeterminateLimitError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "indeterminate report has {} recovery failures and {} affected paths; each limit is {}",
            self.recovery_failures, self.affected_paths, INDETERMINATE_PATHS_MAX
        )
    }
}

impl Error for IndeterminateLimitError {}

/// A filesystem failure after the operation could no longer prove no change.
#[derive(Debug)]
pub struct Indeterminate<E> {
    primary: E,
    recovery: Vec<RecoveryFailure>,
    affected: Vec<PathBuf>,
}

impl<E> Indeterminate<E> {
    /// Creates an indeterminate report after validating both collection bounds.
    ///
    /// # Errors
    ///
    /// Returns [`IndeterminateLimitError`] when either supplied collection is
    /// larger than its published bound.
    pub fn new(
        primary: E,
        recovery: Vec<RecoveryFailure>,
        affected: Vec<PathBuf>,
    ) -> Result<Self, IndeterminateLimitError> {
        if recovery.len() > RECOVERY_FAILURES_MAX || affected.len() > INDETERMINATE_PATHS_MAX {
            return Err(IndeterminateLimitError {
                recovery_failures: recovery.len(),
                affected_paths: affected.len(),
            });
        }
        Ok(Self::from_operation(primary, recovery, affected))
    }

    pub(crate) fn from_operation(
        primary: E,
        recovery: Vec<RecoveryFailure>,
        mut affected: Vec<PathBuf>,
    ) -> Self {
        assert!(
            recovery.len() <= RECOVERY_FAILURES_MAX,
            "an internal durable operation must preserve recovery failures within its owning bound"
        );
        assert!(
            affected.len() <= INDETERMINATE_PATHS_MAX,
            "an internal durable operation must name affected paths within its owning bound"
        );
        affected.sort();
        affected.dedup();
        Self {
            primary,
            recovery,
            affected,
        }
    }

    /// Returns the first operation failure.
    #[must_use]
    pub const fn primary(&self) -> &E {
        &self.primary
    }

    /// Returns every restoration or cleanup failure, in operation order.
    #[must_use]
    pub fn recovery_failures(&self) -> &[RecoveryFailure] {
        &self.recovery
    }

    /// Returns every path that reconciliation must check.
    #[must_use]
    pub fn affected_paths(&self) -> &[PathBuf] {
        &self.affected
    }
}

impl<E: fmt::Display> fmt::Display for Indeterminate<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.primary)?;
        if !self.recovery.is_empty() {
            write!(
                formatter,
                "; {} restoration or cleanup step(s) also failed",
                self.recovery.len()
            )?;
        }
        Ok(())
    }
}

impl<E: Error + 'static> Error for Indeterminate<E> {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        Some(&self.primary)
    }
}

/// The durable result of one filesystem operation.
#[derive(Debug)]
pub enum DurableOutcome<T, E> {
    /// The operation proved that no durable target changed.
    Unchanged(E),
    /// The operation proved the new durable target state.
    Committed(T),
    /// The operation cannot prove that every affected path is unchanged.
    Indeterminate(Indeterminate<E>),
}

impl<T, E> DurableOutcome<T, E> {
    /// Returns the committed value or panics with the supplied message.
    #[cfg(test)]
    pub fn expect(self, message: &str) -> T
    where
        E: fmt::Debug,
    {
        match self {
            Self::Committed(value) => value,
            Self::Unchanged(error) => panic!("{message}: {error:?}"),
            Self::Indeterminate(report) => panic!("{message}: {report:?}"),
        }
    }

    /// Returns the primary failure or panics with the supplied message.
    #[cfg(test)]
    pub fn expect_err(self, message: &str) -> E
    where
        T: fmt::Debug,
        E: fmt::Debug,
    {
        match self {
            Self::Unchanged(error) => error,
            Self::Indeterminate(report) => report.primary,
            Self::Committed(value) => panic!("{message}: {value:?}"),
        }
    }
}
