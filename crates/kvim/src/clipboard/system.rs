//! The system clipboard trait and one implementation for each platform.
//!
//! [`SystemClipboard`] carries text only, because a system clipboard carries no
//! shape. The rules above it, in [`Clipboard`](super::Clipboard), own the shape,
//! the transfer bound, and the failure reports.
//!
//! The platform branch lives in [`detect_system_clipboard`]. No other module
//! names a clipboard command, and no other module reads the target platform.

use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use crate::runtime::{PROCESS_DEADLINE_DEFAULT, ProcessOutput, ProcessRequest};

use super::{CLIPBOARD_BYTES_MAX, ClipboardFailure};

/// The system clipboard of one host.
///
/// One implementation exists for each supported platform, plus a no-operation
/// implementation for a host without any clipboard command and an in-memory
/// implementation for tests. A later implementation, for example an OSC 52
/// clipboard for a remote terminal, needs no change above this trait.
pub trait SystemClipboard: fmt::Debug {
    /// Writes one text to the system clipboard.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardFailure`] when no clipboard exists, when the command
    /// did not start, or when the command reported a failure.
    fn write(&mut self, text: &str) -> Result<(), ClipboardFailure>;

    /// Reads the current system clipboard text.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardFailure`] when no clipboard exists, when the command
    /// did not start, when the command reported a failure, or when the
    /// clipboard holds bytes that are not UTF-8 text.
    fn read(&mut self) -> Result<String, ClipboardFailure>;
}

/// Runs one bounded external command.
///
/// The composition root implements the trait with the bounded process service
/// of [`crate::runtime`], which owns the concurrency limit, the output limit,
/// and the deadline. See `docs/responsiveness.md`.
pub trait ProcessExecutor: fmt::Debug {
    /// Runs one command and returns its captured output.
    ///
    /// # Errors
    ///
    /// Returns [`ClipboardFailure`] when the command did not start, passed its
    /// deadline, was cancelled, or was refused.
    fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, ClipboardFailure>;
}

/// The clipboard of a host that provides no clipboard command.
///
/// A remote terminal without a clipboard tool is a supported environment. Every
/// operation reports [`ClipboardFailure::Unavailable`], and the editor stays
/// fully usable.
///
/// # Examples
///
/// ```
/// use kvim::clipboard::{ClipboardFailure, NoClipboard, SystemClipboard};
///
/// let mut clipboard = NoClipboard;
/// assert_eq!(clipboard.write("alpha"), Err(ClipboardFailure::Unavailable));
/// assert_eq!(clipboard.read(), Err(ClipboardFailure::Unavailable));
/// ```
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct NoClipboard;

impl SystemClipboard for NoClipboard {
    fn write(&mut self, _text: &str) -> Result<(), ClipboardFailure> {
        Err(ClipboardFailure::Unavailable)
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        Err(ClipboardFailure::Unavailable)
    }
}

/// A clipboard that keeps its text in memory.
///
/// Tests use this implementation, so no test starts a real clipboard command.
///
/// # Examples
///
/// ```
/// use kvim::clipboard::{MemoryClipboard, SystemClipboard};
///
/// let mut clipboard = MemoryClipboard::default();
/// clipboard.write("alpha").expect("an in-memory write always succeeds");
/// assert_eq!(clipboard.read(), Ok("alpha".to_owned()));
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MemoryClipboard {
    text: String,
}

impl MemoryClipboard {
    /// Creates a clipboard that already holds text, as an external copy does.
    #[must_use]
    pub fn with_text(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }

    /// Returns the text that the clipboard holds.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl SystemClipboard for MemoryClipboard {
    fn write(&mut self, text: &str) -> Result<(), ClipboardFailure> {
        self.text = text.to_owned();
        Ok(())
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        Ok(self.text.clone())
    }
}

/// The macOS clipboard, which uses `pbcopy` and `pbpaste`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MacOsClipboard<E> {
    executor: E,
}

impl<E: ProcessExecutor> MacOsClipboard<E> {
    /// Creates the macOS clipboard over one bounded process executor.
    pub const fn new(executor: E) -> Self {
        Self { executor }
    }
}

impl<E: ProcessExecutor> SystemClipboard for MacOsClipboard<E> {
    fn write(&mut self, text: &str) -> Result<(), ClipboardFailure> {
        run_write(&self.executor, "pbcopy", &[], text)
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        run_read(&self.executor, "pbpaste", &[])
    }
}

/// The clipboard tools of a Linux session, in preference order.
///
/// `docs/clipboard.md` binds this table.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinuxTool {
    /// `wl-copy` and `wl-paste --no-newline` on a Wayland session.
    Wayland,
    /// `xclip -selection clipboard` on an X11 session.
    XClip,
    /// `xsel --clipboard`, the X11 fallback.
    XSel,
}

impl LinuxTool {
    /// Returns the program that writes the clipboard.
    #[must_use]
    pub const fn write_program(self) -> &'static str {
        match self {
            Self::Wayland => "wl-copy",
            Self::XClip => "xclip",
            Self::XSel => "xsel",
        }
    }

    /// Returns the program that reads the clipboard.
    #[must_use]
    pub const fn read_program(self) -> &'static str {
        match self {
            Self::Wayland => "wl-paste",
            Self::XClip => "xclip",
            Self::XSel => "xsel",
        }
    }

    const fn write_args(self) -> &'static [&'static str] {
        match self {
            Self::Wayland => &[],
            Self::XClip => &["-selection", "clipboard"],
            Self::XSel => &["--clipboard", "--input"],
        }
    }

    const fn read_args(self) -> &'static [&'static str] {
        match self {
            Self::Wayland => &["--no-newline"],
            Self::XClip => &["-selection", "clipboard", "-o"],
            Self::XSel => &["--clipboard", "--output"],
        }
    }
}

/// The Linux clipboard, which uses the tool that the session selected.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LinuxClipboard<E> {
    tool: LinuxTool,
    executor: E,
}

impl<E: ProcessExecutor> LinuxClipboard<E> {
    /// Creates the Linux clipboard over one tool and one process executor.
    pub const fn new(tool: LinuxTool, executor: E) -> Self {
        Self { tool, executor }
    }

    /// Returns the tool that this session selected.
    #[must_use]
    pub const fn tool(&self) -> LinuxTool {
        self.tool
    }
}

impl<E: ProcessExecutor> SystemClipboard for LinuxClipboard<E> {
    fn write(&mut self, text: &str) -> Result<(), ClipboardFailure> {
        run_write(
            &self.executor,
            self.tool.write_program(),
            self.tool.write_args(),
            text,
        )
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        run_read(
            &self.executor,
            self.tool.read_program(),
            self.tool.read_args(),
        )
    }
}

/// The display session that selects the Linux tool order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplaySession {
    /// A Wayland session. Kvim prefers the Wayland tool.
    Wayland,
    /// An X11 session, or an unknown session.
    X11,
}

/// Selects the Linux clipboard tool of one session.
///
/// The function is pure: the caller reports the session and which programs
/// exist. Kvim selects the tool once at startup and never guesses per
/// operation.
///
/// # Examples
///
/// ```
/// use kvim::clipboard::{DisplaySession, LinuxTool, select_linux_tool};
///
/// let tool = select_linux_tool(DisplaySession::Wayland, |_| true);
/// assert_eq!(tool, Some(LinuxTool::Wayland));
///
/// // The selection falls back through the X11 tools in order.
/// let tool = select_linux_tool(DisplaySession::Wayland, |name| name == "xsel");
/// assert_eq!(tool, Some(LinuxTool::XSel));
///
/// // A host without any clipboard tool is a supported environment.
/// assert_eq!(select_linux_tool(DisplaySession::X11, |_| false), None);
/// ```
#[must_use]
pub fn select_linux_tool(
    session: DisplaySession,
    available: impl Fn(&str) -> bool,
) -> Option<LinuxTool> {
    let candidates: &[LinuxTool] = match session {
        DisplaySession::Wayland => &[LinuxTool::Wayland, LinuxTool::XClip, LinuxTool::XSel],
        DisplaySession::X11 => &[LinuxTool::XClip, LinuxTool::XSel],
    };
    candidates
        .iter()
        .find(|tool| available(tool.write_program()) && available(tool.read_program()))
        .copied()
}

/// Selects the clipboard implementation of this host.
///
/// The function reads the target platform, the presence of a Wayland session,
/// and the executable search path. It runs once at startup, and the composition
/// root injects the result. A host without any clipboard command receives
/// [`NoClipboard`].
///
/// # Examples
///
/// ```
/// use kvim::clipboard::{ClipboardFailure, ProcessExecutor, detect_system_clipboard};
/// use kvim::runtime::{ProcessOutput, ProcessRequest};
///
/// #[derive(Debug)]
/// struct Refuse;
/// impl ProcessExecutor for Refuse {
///     fn run(&self, _request: ProcessRequest) -> Result<ProcessOutput, ClipboardFailure> {
///         Err(ClipboardFailure::Refused)
///     }
/// }
///
/// // The selection always returns a usable clipboard, even without a command.
/// // A refused command stays an expected runtime state on every host.
/// let mut clipboard = detect_system_clipboard(Refuse);
/// assert!(clipboard.read().is_err());
/// ```
#[must_use]
pub fn detect_system_clipboard<E>(executor: E) -> Box<dyn SystemClipboard>
where
    E: ProcessExecutor + 'static,
{
    if cfg!(target_os = "macos") {
        if program_on_path("pbcopy") && program_on_path("pbpaste") {
            return Box::new(MacOsClipboard::new(executor));
        }
        return Box::new(NoClipboard);
    }
    if cfg!(target_os = "linux") {
        let session = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
            DisplaySession::Wayland
        } else {
            DisplaySession::X11
        };
        if let Some(tool) = select_linux_tool(session, program_on_path) {
            return Box::new(LinuxClipboard::new(tool, executor));
        }
    }
    Box::new(NoClipboard)
}

/// Reports whether one program exists on the executable search path.
///
/// The function reads the search path of the process. It reads no other host
/// state, and it reports presence only.
#[must_use]
pub fn program_on_path(name: &str) -> bool {
    let Some(path) = std::env::var_os("PATH") else {
        return false;
    };
    std::env::split_paths(&path).any(|directory| is_executable(&directory.join(name)))
}

fn run_write(
    executor: &impl ProcessExecutor,
    program: &str,
    args: &[&str],
    text: &str,
) -> Result<(), ClipboardFailure> {
    let mut request = request(program, args);
    request.stdin = text.as_bytes().to_vec();
    let output = executor.run(request)?;
    if output.status_code == Some(0) {
        return Ok(());
    }
    Err(ClipboardFailure::Failed)
}

fn run_read(
    executor: &impl ProcessExecutor,
    program: &str,
    args: &[&str],
) -> Result<String, ClipboardFailure> {
    let output = executor.run(request(program, args))?;
    if output.status_code != Some(0) {
        return Err(ClipboardFailure::Failed);
    }
    String::from_utf8(output.stdout).map_err(|_| ClipboardFailure::NotText)
}

fn request(program: &str, args: &[&str]) -> ProcessRequest {
    let mut request = ProcessRequest::new(program);
    request.args = args.iter().map(OsString::from).collect();
    request.output_bytes_max = CLIPBOARD_BYTES_MAX;
    request.deadline = PROCESS_DEADLINE_DEFAULT;
    request
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    std::fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}
