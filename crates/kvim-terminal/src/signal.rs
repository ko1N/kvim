//! The process termination signal source.
//!
//! A terminated editor must never leave the terminal in raw mode. The default
//! action of `SIGTERM`, `SIGINT`, and `SIGHUP` ends the process at once, while
//! the editor still holds raw mode, the alternate screen, and the enhanced
//! keyboard flags. No restore step runs, and the user must repair the shell
//! blind.
//!
//! [`TerminationSource`] turns the first of those signals into one value that
//! the event loop reads beside its terminal events. The loop then leaves
//! exactly as it leaves after the last window closes, so the ordinary restore
//! runs and writes the same steps as the panic hook. See
//! `docs/responsiveness.md`.

use std::future;

use tokio::sync::mpsc;

/// The number of termination requests that one source holds.
///
/// The editor leaves its event loop on the first request, so the listener
/// reports one request and needs no queue behind it.
const TERMINATION_QUEUE_CAPACITY: usize = 1;

/// One operating system signal that asks the editor to end.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TerminationSignal {
    /// The user interrupted the process (`SIGINT`).
    Interrupt,
    /// The terminal of the process closed (`SIGHUP`).
    Hangup,
    /// Another process asked this process to end (`SIGTERM`).
    Terminate,
}

/// The source of the first termination request of the process.
///
/// The source reads from a queue instead of the signals themselves, so a test
/// drives the termination path without sending a real signal to the test
/// process. [`TerminationSource::from_process`] supplies the process signals.
///
/// ```
/// use kvim_terminal::{TerminationSignal, TerminationSource};
///
/// # tokio::runtime::Builder::new_current_thread().build()?.block_on(async {
/// let (requests, mut terminations) = TerminationSource::channel();
/// requests
///     .send(TerminationSignal::Terminate)
///     .await
///     .expect("the source holds its receiver");
///
/// assert_eq!(terminations.recv().await, TerminationSignal::Terminate);
/// # });
/// # Ok::<(), std::io::Error>(())
/// ```
pub struct TerminationSource {
    requests: mpsc::Receiver<TerminationSignal>,
}

impl TerminationSource {
    /// Listens for the termination signals of this process.
    ///
    /// The listener reports the first signal and then ends, because the editor
    /// leaves its event loop on that request and shuts its background services
    /// down under their own deadlines. One process creates one source, so the
    /// listener is one task for the life of the editor.
    ///
    /// The call spawns that task, so it must run inside a Tokio runtime
    /// context.
    ///
    /// ```no_run
    /// use kvim_terminal::TerminationSource;
    ///
    /// # async fn wait() {
    /// let mut terminations = TerminationSource::from_process();
    /// let signal = terminations.recv().await;
    /// # let _ = signal;
    /// # }
    /// ```
    #[must_use]
    pub fn from_process() -> Self {
        let (requests, source) = Self::channel();
        spawn_listener(requests);
        source
    }

    /// Creates a source that the caller drives, and the sender of its requests.
    ///
    /// A real signal reaches the whole process, so a test that raises one could
    /// end the test runner. A test drives this pair instead.
    #[must_use]
    pub fn channel() -> (mpsc::Sender<TerminationSignal>, Self) {
        let (sender, requests) = mpsc::channel(TERMINATION_QUEUE_CAPACITY);
        (sender, Self { requests })
    }

    /// Waits for the first termination request of the process.
    ///
    /// The future never completes after the listener ends, so the event loop
    /// waits for its other sources instead of spinning on a ready arm. The
    /// operation is cancel safe, so the event loop may drop it inside a
    /// `select`: it holds no request that the drop could lose.
    pub async fn recv(&mut self) -> TerminationSignal {
        match self.requests.recv().await {
            Some(signal) => signal,
            None => future::pending().await,
        }
    }
}

/// Reports the first termination signal of the process to `requests`.
///
/// A registration that fails leaves the default action of every signal in
/// place, so the process still ends. The listener reports nothing then, which
/// is the behavior of an editor without this source.
#[cfg(unix)]
fn spawn_listener(requests: mpsc::Sender<TerminationSignal>) {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let (Ok(mut interrupt), Ok(mut hangup), Ok(mut terminate)) = (
            signal(SignalKind::interrupt()),
            signal(SignalKind::hangup()),
            signal(SignalKind::terminate()),
        ) else {
            return;
        };
        let received = tokio::select! {
            _ = interrupt.recv() => TerminationSignal::Interrupt,
            _ = hangup.recv() => TerminationSignal::Hangup,
            _ = terminate.recv() => TerminationSignal::Terminate,
        };
        // The queue holds one request and the editor leaves on it, so this send
        // never waits. The task ends here.
        let _ = requests.send(received).await;
    });
}

/// Reports no termination request.
///
/// Only Unix defines the signals that end the editor, and macOS and Linux are
/// the platforms that Kvim supports.
#[cfg(not(unix))]
fn spawn_listener(_requests: mpsc::Sender<TerminationSignal>) {}

#[cfg(test)]
mod tests {
    use futures_util::FutureExt;

    use super::*;

    #[tokio::test]
    async fn a_source_without_a_listener_never_reports_a_request() {
        let (requests, mut terminations) = TerminationSource::channel();
        drop(requests);

        assert!(
            terminations.recv().now_or_never().is_none(),
            "a ready arm without a request would spin the event loop"
        );
    }
}
