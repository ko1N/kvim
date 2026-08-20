//! The system clipboard trait and one implementation for each platform.
//!
//! [`SystemClipboard`] carries text only, because a system clipboard carries no
//! shape. The rules above it, in [`Clipboard`](super::Clipboard), own the shape,
//! the transfer bound, and the failure reports.
//!
//! The platform branch lives in [`ClipboardSelection`], and
//! [`detect_system_clipboard`] builds its implementation from that value. No
//! other module names a clipboard command, and no other module reads the target
//! platform. A caller that must report the selected commands, for example the
//! diagnostics report of the executable, reads [`ClipboardSelection::commands`].

use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use kvim_runtime::{PROCESS_DEADLINE_DEFAULT, ProcessOutput, ProcessRequest};

use super::{CLIPBOARD_BYTES_MAX, ClipboardFailure};

/// The environment variable that marks a Wayland session.
///
/// The detection reads whether the variable exists. It never reads and never
/// reports the value.
const WAYLAND_VARIABLE: &str = "WAYLAND_DISPLAY";

/// The macOS clipboard commands. `docs/clipboard.md` binds the table.
const MACOS_COMMANDS: ClipboardCommands = ClipboardCommands {
    write: ClipboardCommand {
        program: "pbcopy",
        args: &[],
    },
    read: ClipboardCommand {
        program: "pbpaste",
        args: &[],
    },
};

/// One clipboard command of one platform.
///
/// The value is the canonical form of the command. No module above this crate
/// names a clipboard program or a clipboard argument.
///
/// # Examples
///
/// ```
/// use kvim_clipboard::LinuxTool;
///
/// let commands = LinuxTool::Wayland.commands();
/// assert_eq!(commands.read.program, "wl-paste");
/// assert_eq!(commands.read.to_string(), "wl-paste --no-newline");
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardCommand {
    /// The program that the command runs.
    pub program: &'static str,
    /// The arguments of that program.
    pub args: &'static [&'static str],
}

impl fmt::Display for ClipboardCommand {
    /// Writes the command as one shell-like line, so a report can print it.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.program)?;
        for arg in self.args {
            write!(formatter, " {arg}")?;
        }
        Ok(())
    }
}

/// The write command and the read command of one clipboard.
///
/// The two commands always exist together, so no selection can offer one
/// direction alone.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClipboardCommands {
    /// The command that writes the clipboard.
    pub write: ClipboardCommand,
    /// The command that reads the clipboard.
    pub read: ClipboardCommand,
}

/// The platform that decides which clipboard commands exist.
///
/// # Examples
///
/// ```
/// use kvim_clipboard::{ClipboardPlatform, ClipboardSelection, DisplaySession};
///
/// // The composition root reports the platform of this build, then selects the
/// // commands that the host provides. A host that provides none is supported.
/// let selection = ClipboardSelection::select(
///     ClipboardPlatform::current(),
///     DisplaySession::detect(),
///     |_| false,
/// );
/// assert_eq!(selection.commands(), None);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardPlatform {
    /// A macOS host, which provides `pbcopy` and `pbpaste`.
    MacOs,
    /// A Linux host, which provides one of the tools of [`LinuxTool`].
    Linux,
    /// Any other host. kvim serves it without a clipboard command.
    Other,
}

impl ClipboardPlatform {
    /// Returns the platform of this build.
    #[must_use]
    pub const fn current() -> Self {
        if cfg!(target_os = "macos") {
            Self::MacOs
        } else if cfg!(target_os = "linux") {
            Self::Linux
        } else {
            Self::Other
        }
    }
}

/// The clipboard commands that one host selects.
///
/// [`detect_system_clipboard`] builds its implementation from this value, so a
/// caller can ask which commands a session uses without creating a clipboard.
///
/// # Examples
///
/// ```
/// use kvim_clipboard::{ClipboardPlatform, ClipboardSelection, DisplaySession, LinuxTool};
///
/// let selection = ClipboardSelection::select(
///     ClipboardPlatform::Linux,
///     DisplaySession::Wayland,
///     |_| true,
/// );
/// assert_eq!(
///     selection,
///     ClipboardSelection::Linux {
///         session: DisplaySession::Wayland,
///         tool: LinuxTool::Wayland,
///     }
/// );
/// let commands = selection.commands().expect("the tool exists");
/// assert_eq!(commands.write.program, "wl-copy");
///
/// // A host without any clipboard command is a supported environment.
/// let absent = ClipboardSelection::select(ClipboardPlatform::Other, DisplaySession::X11, |_| true);
/// assert_eq!(absent, ClipboardSelection::Absent);
/// assert_eq!(absent.commands(), None);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClipboardSelection {
    /// The macOS commands.
    MacOs,
    /// One Linux tool of one display session.
    Linux {
        /// The session that ordered the candidates.
        session: DisplaySession,
        /// The selected tool.
        tool: LinuxTool,
    },
    /// The host provides no clipboard command.
    Absent,
}

impl ClipboardSelection {
    /// Selects the clipboard of this host.
    ///
    /// The function reads the target platform, the presence of a Wayland
    /// session, and the executable search path. It runs once at startup.
    #[must_use]
    pub fn detect() -> Self {
        Self::select(
            ClipboardPlatform::current(),
            DisplaySession::detect(),
            program_on_path,
        )
    }

    /// Selects the clipboard of one host without reading that host.
    ///
    /// The function is pure: the caller reports the platform, the session, and
    /// which programs exist.
    #[must_use]
    pub fn select(
        platform: ClipboardPlatform,
        session: DisplaySession,
        available: impl Fn(&str) -> bool,
    ) -> Self {
        match platform {
            ClipboardPlatform::MacOs => {
                if available(MACOS_COMMANDS.write.program) && available(MACOS_COMMANDS.read.program)
                {
                    Self::MacOs
                } else {
                    Self::Absent
                }
            }
            ClipboardPlatform::Linux => match select_linux_tool(session, available) {
                Some(tool) => Self::Linux { session, tool },
                None => Self::Absent,
            },
            ClipboardPlatform::Other => Self::Absent,
        }
    }

    /// Returns the commands of this selection, or `None` without a clipboard.
    #[must_use]
    pub const fn commands(self) -> Option<ClipboardCommands> {
        match self {
            Self::MacOs => Some(MACOS_COMMANDS),
            Self::Linux { tool, .. } => Some(tool.commands()),
            Self::Absent => None,
        }
    }
}

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
/// of [`kvim_runtime`], which owns the concurrency limit, the output limit,
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
/// use kvim_clipboard::{ClipboardFailure, NoClipboard, SystemClipboard};
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
/// use kvim_clipboard::{MemoryClipboard, SystemClipboard};
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
        run_write(&self.executor, MACOS_COMMANDS.write, text)
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        run_read(&self.executor, MACOS_COMMANDS.read)
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
    /// Returns the two commands of this tool.
    ///
    /// This table is the one place that names a Linux clipboard command.
    /// `docs/clipboard.md` binds it.
    #[must_use]
    pub const fn commands(self) -> ClipboardCommands {
        match self {
            Self::Wayland => ClipboardCommands {
                write: ClipboardCommand {
                    program: "wl-copy",
                    args: &[],
                },
                read: ClipboardCommand {
                    program: "wl-paste",
                    args: &["--no-newline"],
                },
            },
            Self::XClip => ClipboardCommands {
                write: ClipboardCommand {
                    program: "xclip",
                    args: &["-selection", "clipboard"],
                },
                read: ClipboardCommand {
                    program: "xclip",
                    args: &["-selection", "clipboard", "-o"],
                },
            },
            Self::XSel => ClipboardCommands {
                write: ClipboardCommand {
                    program: "xsel",
                    args: &["--clipboard", "--input"],
                },
                read: ClipboardCommand {
                    program: "xsel",
                    args: &["--clipboard", "--output"],
                },
            },
        }
    }

    /// Returns the program that writes the clipboard.
    const fn write_program(self) -> &'static str {
        self.commands().write.program
    }

    /// Returns the program that reads the clipboard.
    const fn read_program(self) -> &'static str {
        self.commands().read.program
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
        run_write(&self.executor, self.tool.commands().write, text)
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        run_read(&self.executor, self.tool.commands().read)
    }
}

/// The display session that selects the Linux tool order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplaySession {
    /// A Wayland session. kvim prefers the Wayland tool.
    Wayland,
    /// An X11 session, or an unknown session.
    X11,
}

impl DisplaySession {
    /// Returns the display session of this host.
    ///
    /// The function reads whether the Wayland variable exists. It never reads
    /// and never reports the value.
    #[must_use]
    pub fn detect() -> Self {
        if std::env::var_os(WAYLAND_VARIABLE).is_some() {
            Self::Wayland
        } else {
            Self::X11
        }
    }
}

/// Selects the Linux clipboard tool of one session.
///
/// The function is pure: the caller reports the session and which programs
/// exist. kvim selects the tool once at startup and never guesses per
/// operation.
///
/// # Examples
///
/// ```
/// use kvim_clipboard::{DisplaySession, LinuxTool, select_linux_tool};
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
/// The function builds the implementation of [`ClipboardSelection::detect`], so
/// the selection rules live in one place. It runs once at startup, and the
/// composition root injects the result. A host without any clipboard command
/// receives [`NoClipboard`].
///
/// # Examples
///
/// ```
/// use kvim_clipboard::{ClipboardFailure, ProcessExecutor, detect_system_clipboard};
/// use kvim_runtime::{ProcessOutput, ProcessRequest};
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
    match ClipboardSelection::detect() {
        ClipboardSelection::MacOs => Box::new(MacOsClipboard::new(executor)),
        ClipboardSelection::Linux { tool, .. } => Box::new(LinuxClipboard::new(tool, executor)),
        ClipboardSelection::Absent => Box::new(NoClipboard),
    }
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
    command: ClipboardCommand,
    text: &str,
) -> Result<(), ClipboardFailure> {
    let mut request = request(command);
    request.stdin = text.as_bytes().to_vec();
    let output = executor.run(request)?;
    if output.status_code == Some(0) {
        return Ok(());
    }
    Err(ClipboardFailure::Failed)
}

fn run_read(
    executor: &impl ProcessExecutor,
    command: ClipboardCommand,
) -> Result<String, ClipboardFailure> {
    let output = executor.run(request(command))?;
    if output.status_code != Some(0) {
        return Err(ClipboardFailure::Failed);
    }
    String::from_utf8(output.stdout).map_err(|_| ClipboardFailure::NotText)
}

fn request(command: ClipboardCommand) -> ProcessRequest {
    let mut request = ProcessRequest::new(command.program);
    request.args = command.args.iter().map(OsString::from).collect();
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
