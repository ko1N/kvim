//! The system clipboard of one editor session.
//!
//! The `kvim-clipboard` crate owns the platform boundary, the shape rule, the
//! transfer bound, and the one-time report for a missing command. It runs its
//! platform command through one synchronous
//! [`ProcessExecutor`](kvim_clipboard::ProcessExecutor), and the terminal event
//! loop must never block on an external command.
//!
//! [`SessionClipboard`] closes that gap. It supplies an executor that runs no
//! command: the executor records the request that the clipboard asked for, and
//! the operation returns [`ClipboardStep::Waiting`]. The event loop hands the
//! recorded command to the bounded process service and returns the output
//! through [`SessionClipboard::finish_copy`] or
//! [`SessionClipboard::finish_read`], which repeat the same operation. The
//! repeated operation reads the delivered output instead of recording a second
//! request, so it returns [`ClipboardStep::Done`].
//!
//! An implementation that needs no external command, such as
//! [`NoClipboard`](kvim_clipboard::NoClipboard) or
//! [`MemoryClipboard`](kvim_clipboard::MemoryClipboard), never reaches the
//! executor and therefore always finishes on the event loop.
//!
//! See `docs/clipboard.md` and `docs/responsiveness.md`.

use std::cell::RefCell;
use std::rc::Rc;

use kvim_clipboard::{
    Clipboard, ClipboardFailure, ClipboardNotice, ClipboardRead, ClipboardShape, ClipboardValue,
    NoClipboard, OwnedClipboardValue, ProcessExecutor, SystemClipboard,
};
use kvim_core::LineEnding;
use kvim_editor::{RegisterShape, RegisterValue};
use kvim_runtime::{ProcessOutput, ProcessRequest, RuntimeError, SubmitError};

/// One step of a clipboard operation.
///
/// The clipboard of a host without an external command finishes at once, so a
/// test double and a host without a clipboard tool both produce
/// [`ClipboardStep::Done`] on the first call.
#[derive(Clone, Debug)]
pub(super) enum ClipboardStep<T> {
    /// The operation finished without any external command.
    Done(T),
    /// The operation needs this command before it can finish.
    Waiting(ProcessRequest),
}

/// The recorded command and the delivered output of one deferred operation.
#[derive(Debug, Default)]
struct DeferredState {
    /// The command that the clipboard asked for and no one has run yet.
    recorded: Option<ProcessRequest>,
    /// The output that the bounded process service produced for that command.
    ready: Option<Result<ProcessOutput, ClipboardFailure>>,
}

/// The executor that records one clipboard command instead of running it.
///
/// The value is shared with the [`Clipboard`] that owns it, so both sides see
/// the same recorded command and the same delivered output. One editor session
/// runs on one thread, so the sharing needs no lock.
#[derive(Clone, Debug, Default)]
struct DeferredExecutor(Rc<RefCell<DeferredState>>);

impl ProcessExecutor for DeferredExecutor {
    fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, ClipboardFailure> {
        let mut state = self.0.borrow_mut();
        if let Some(ready) = state.ready.take() {
            return ready;
        }
        state.recorded = Some(request);
        // The operation has not finished. The caller finds the recorded command
        // and repeats the operation, so it never reads this failure.
        Err(ClipboardFailure::Cancelled)
    }
}

/// The system clipboard boundary of one editor session.
///
/// The value holds the platform implementation, the text that Kvim wrote last,
/// and the one-time report for a missing command. It holds no register value,
/// so no clipboard failure removes editor data.
#[derive(Debug)]
pub(super) struct SessionClipboard {
    clipboard: Clipboard,
    deferred: DeferredExecutor,
}

impl Default for SessionClipboard {
    /// Creates a session clipboard for a host that provides no command.
    ///
    /// The composition root replaces it with [`SessionClipboard::detect`] at
    /// startup, so no test reaches a real clipboard command.
    fn default() -> Self {
        Self::over(Box::new(NoClipboard))
    }
}

impl SessionClipboard {
    /// Selects the clipboard implementation of this host.
    ///
    /// The selection reads the target platform and the executable search path,
    /// so it runs once at startup and never per operation.
    #[must_use]
    pub(super) fn detect() -> Self {
        let deferred = DeferredExecutor::default();
        Self {
            clipboard: Clipboard::detect(deferred.clone()),
            deferred,
        }
    }

    /// Creates a session clipboard over one explicit implementation.
    #[must_use]
    pub(super) fn over(system: Box<dyn SystemClipboard>) -> Self {
        Self {
            clipboard: Clipboard::new(system),
            deferred: DeferredExecutor::default(),
        }
    }

    /// Writes one unnamed-register value to the system clipboard.
    ///
    /// The caller keeps its register value in every case, so a failed write
    /// loses nothing.
    pub(super) fn copy(&mut self, value: &RegisterValue) -> ClipboardStep<Option<ClipboardNotice>> {
        let notice = self.clipboard.copy(ClipboardValue {
            text: value.text(),
            shape: clipboard_shape(value.shape()),
        });
        self.step(notice)
    }

    /// Reads the system clipboard and decides the shape of its text.
    pub(super) fn read(&mut self) -> ClipboardStep<ClipboardRead> {
        let read = self.clipboard.paste();
        self.step(read)
    }

    /// Repeats one write with the output that its recorded command produced.
    pub(super) fn finish_copy(
        &mut self,
        value: &RegisterValue,
        output: Result<ProcessOutput, ClipboardFailure>,
    ) -> Option<ClipboardNotice> {
        self.deliver(output);
        let step = self.copy(value);
        self.discard();
        match step {
            ClipboardStep::Done(notice) => notice,
            // One write runs one command, so the delivered output finishes it.
            // The bound keeps a defect of that shape from restarting the
            // command forever.
            ClipboardStep::Waiting(_) => {
                debug_assert!(false, "one clipboard write runs one command");
                Some(ClipboardNotice::CommandFailed)
            }
        }
    }

    /// Repeats one read with the output that its recorded command produced.
    pub(super) fn finish_read(
        &mut self,
        output: Result<ProcessOutput, ClipboardFailure>,
    ) -> ClipboardRead {
        self.deliver(output);
        let step = self.read();
        self.discard();
        match step {
            ClipboardStep::Done(read) => read,
            // See [`SessionClipboard::finish_copy`] for the same bound.
            ClipboardStep::Waiting(_) => {
                debug_assert!(false, "one clipboard read runs one command");
                ClipboardRead::Fallback(Some(ClipboardNotice::CommandFailed))
            }
        }
    }

    /// Hands the output of one recorded command back to the clipboard.
    fn deliver(&mut self, output: Result<ProcessOutput, ClipboardFailure>) {
        let mut state = self.deferred.0.borrow_mut();
        state.recorded = None;
        state.ready = Some(output);
    }

    /// Drops a command that no operation runs and an output that none reads.
    ///
    /// One finished operation leaves neither behind, so the next operation
    /// always starts from an empty state.
    fn discard(&mut self) {
        let mut state = self.deferred.0.borrow_mut();
        state.recorded = None;
        state.ready = None;
    }

    /// Returns the recorded command, or the finished value of the operation.
    fn step<T>(&self, done: T) -> ClipboardStep<T> {
        match self.deferred.0.borrow_mut().recorded.take() {
            Some(request) => ClipboardStep::Waiting(request),
            None => ClipboardStep::Done(done),
        }
    }
}

/// A clipboard that reaches its value through one external command.
///
/// The double stands for every platform implementation, so a test can drive the
/// deferred path without naming a platform and without running a command. The
/// event loop of a test returns the output through
/// [`Session::apply_clipboard_result`](super::session::Session::apply_clipboard_result).
#[cfg(test)]
#[derive(Debug)]
struct DeferredClipboard(DeferredExecutor);

#[cfg(test)]
impl SystemClipboard for DeferredClipboard {
    fn write(&mut self, text: &str) -> Result<(), ClipboardFailure> {
        let mut request = ProcessRequest::new("write");
        request.stdin = text.as_bytes().to_vec();
        let output = self.0.run(request)?;
        if output.status_code == Some(0) {
            return Ok(());
        }
        Err(ClipboardFailure::Failed)
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        let output = self.0.run(ProcessRequest::new("read"))?;
        if output.status_code != Some(0) {
            return Err(ClipboardFailure::Failed);
        }
        String::from_utf8(output.stdout).map_err(|_| ClipboardFailure::NotText)
    }
}

#[cfg(test)]
impl SessionClipboard {
    /// Creates a session clipboard whose commands the event loop must run.
    #[must_use]
    pub(super) fn deferred() -> Self {
        let deferred = DeferredExecutor::default();
        Self {
            clipboard: Clipboard::new(Box::new(DeferredClipboard(deferred.clone()))),
            deferred,
        }
    }
}

/// Returns the clipboard shape of one register shape.
///
/// The two modules keep their own shape types, because the dependency
/// direction lets no boundary module reach into the editor. This composition
/// layer converts between them. See `docs/clipboard.md`.
const fn clipboard_shape(shape: RegisterShape) -> ClipboardShape {
    match shape {
        RegisterShape::Characterwise => ClipboardShape::Characterwise,
        RegisterShape::Linewise => ClipboardShape::Linewise,
        RegisterShape::Blockwise => ClipboardShape::Blockwise,
    }
}

/// Returns the register value of one clipboard value.
///
/// A linewise value must end with the line ending of the buffer that receives
/// it, so the conversion appends a missing one.
pub(super) fn register_value(value: OwnedClipboardValue, ending: LineEnding) -> RegisterValue {
    match value.shape {
        ClipboardShape::Characterwise => RegisterValue::characterwise(value.text),
        ClipboardShape::Linewise => RegisterValue::linewise(value.text, ending),
        ClipboardShape::Blockwise => RegisterValue::new(value.text, RegisterShape::Blockwise),
    }
}

/// Returns the clipboard failure of one background failure.
pub(super) const fn command_failure(error: &RuntimeError) -> ClipboardFailure {
    match error {
        RuntimeError::Timeout => ClipboardFailure::Timeout,
        RuntimeError::Cancelled => ClipboardFailure::Cancelled,
        RuntimeError::ProcessSpawn(_) => ClipboardFailure::NotStarted,
        RuntimeError::WorkerFailure(_)
        | RuntimeError::ProcessRead(_)
        | RuntimeError::ProcessWrite(_)
        | RuntimeError::OutputLimit { .. } => ClipboardFailure::Failed,
    }
}

/// Returns the clipboard failure of one refused submission.
pub(super) const fn refused_submission(error: SubmitError) -> ClipboardFailure {
    match error {
        SubmitError::ShuttingDown => ClipboardFailure::Cancelled,
        SubmitError::InvalidLimits | SubmitError::ProcessBounds | SubmitError::Saturated(_) => {
            ClipboardFailure::Refused
        }
    }
}
