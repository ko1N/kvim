//! Behavior tests for the clipboard boundary, the shape rule, and every failure path.
//!
//! No test starts a real clipboard command. Every test substitutes one
//! implementation of [`SystemClipboard`] or of [`ProcessExecutor`].

use std::cell::RefCell;
use std::rc::Rc;

use super::{
    CLIPBOARD_BYTES_MAX, Clipboard, ClipboardEvidence, ClipboardFailure, ClipboardNotice,
    ClipboardPlatform, ClipboardRead, ClipboardSelection, ClipboardShape, ClipboardValue,
    DisplaySession, LinuxClipboard, LinuxTool, MacOsClipboard, MemoryClipboard, NoClipboard,
    OwnedClipboardValue, ProcessExecutor, SystemClipboard, select_linux_tool,
};
use kvim_runtime::{ProcessOutput, ProcessRequest};

/// A clipboard whose text another test line can replace, as an external
/// application replaces the system clipboard.
#[derive(Clone, Debug, Default)]
struct SharedClipboard {
    text: Rc<RefCell<String>>,
}

impl SharedClipboard {
    fn set(&self, text: &str) {
        *self.text.borrow_mut() = text.to_owned();
    }
}

impl SystemClipboard for SharedClipboard {
    fn write(&mut self, text: &str) -> Result<(), ClipboardFailure> {
        self.set(text);
        Ok(())
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        Ok(self.text.borrow().clone())
    }
}

/// A clipboard whose commands always end with one chosen failure.
#[derive(Debug)]
struct FailingClipboard(ClipboardFailure);

impl SystemClipboard for FailingClipboard {
    fn write(&mut self, _text: &str) -> Result<(), ClipboardFailure> {
        Err(self.0)
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        Err(self.0)
    }
}

/// A clipboard that returns more text than the transfer bound allows.
#[derive(Debug)]
struct OversizedClipboard;

impl SystemClipboard for OversizedClipboard {
    fn write(&mut self, _text: &str) -> Result<(), ClipboardFailure> {
        Ok(())
    }

    fn read(&mut self) -> Result<String, ClipboardFailure> {
        Ok("x".repeat(CLIPBOARD_BYTES_MAX + 1))
    }
}

/// A process executor that records every request and returns a fixed result.
#[derive(Clone, Debug)]
struct RecordingExecutor {
    requests: Rc<RefCell<Vec<ProcessRequest>>>,
    stdout: Vec<u8>,
    status_code: Option<i32>,
}

impl RecordingExecutor {
    fn new(stdout: &str) -> Self {
        Self {
            requests: Rc::new(RefCell::new(Vec::new())),
            stdout: stdout.as_bytes().to_vec(),
            status_code: Some(0),
        }
    }

    fn failing() -> Self {
        Self {
            requests: Rc::new(RefCell::new(Vec::new())),
            stdout: Vec::new(),
            status_code: Some(1),
        }
    }

    /// Returns an executor whose command a signal ended before it exited.
    fn signalled() -> Self {
        Self {
            requests: Rc::new(RefCell::new(Vec::new())),
            stdout: Vec::new(),
            status_code: None,
        }
    }

    fn recorded(&self) -> Vec<(String, Vec<String>)> {
        self.requests
            .borrow()
            .iter()
            .map(|request| {
                (
                    request.program.to_string_lossy().into_owned(),
                    request
                        .args
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect(),
                )
            })
            .collect()
    }
}

impl ProcessExecutor for RecordingExecutor {
    fn run(&self, request: ProcessRequest) -> Result<ProcessOutput, ClipboardFailure> {
        self.requests.borrow_mut().push(request);
        Ok(ProcessOutput {
            status_code: self.status_code,
            stdout: self.stdout.clone(),
            stderr: Vec::new(),
        })
    }
}

fn characterwise(text: &str) -> ClipboardValue<'_> {
    ClipboardValue {
        text,
        shape: ClipboardShape::Characterwise,
    }
}

fn linewise(text: &str) -> ClipboardValue<'_> {
    ClipboardValue {
        text,
        shape: ClipboardShape::Linewise,
    }
}

#[test]
fn a_missing_command_is_reported_once_for_each_session() {
    let mut clipboard = Clipboard::new(Box::new(NoClipboard));
    assert_eq!(
        clipboard.copy(characterwise("alpha")),
        Some(ClipboardNotice::NoCommand)
    );
    assert_eq!(clipboard.copy(characterwise("beta")), None);
    assert_eq!(clipboard.paste(), ClipboardRead::Fallback(None));
}

#[test]
fn a_missing_command_reported_by_a_read_stays_reported_for_a_write() {
    let mut clipboard = Clipboard::new(Box::new(NoClipboard));
    assert_eq!(
        clipboard.paste(),
        ClipboardRead::Fallback(Some(ClipboardNotice::NoCommand))
    );
    assert_eq!(clipboard.copy(characterwise("alpha")), None);
}

#[test]
fn a_failed_command_is_reported_for_each_operation() {
    let mut clipboard = Clipboard::new(Box::new(FailingClipboard(ClipboardFailure::Failed)));
    for _ in 0..3 {
        assert_eq!(
            clipboard.copy(characterwise("alpha")),
            Some(ClipboardNotice::CommandFailed)
        );
        assert_eq!(
            clipboard.paste(),
            ClipboardRead::Fallback(Some(ClipboardNotice::CommandFailed))
        );
    }
}

#[test]
fn an_oversized_value_never_reaches_the_clipboard() {
    let system = SharedClipboard::default();
    let mut clipboard = Clipboard::new(Box::new(system.clone()));
    let text = "x".repeat(CLIPBOARD_BYTES_MAX + 1);
    assert_eq!(
        clipboard.copy(characterwise(&text)),
        Some(ClipboardNotice::TooLarge {
            bytes: CLIPBOARD_BYTES_MAX + 1
        })
    );
    assert!(
        system.text.borrow().is_empty(),
        "an oversized value stays inside the editor"
    );
}

#[test]
fn an_oversized_read_falls_back_instead_of_returning_the_text() {
    let mut clipboard = Clipboard::new(Box::new(OversizedClipboard));
    assert_eq!(
        clipboard.paste(),
        ClipboardRead::Fallback(Some(ClipboardNotice::TooLarge {
            bytes: CLIPBOARD_BYTES_MAX + 1
        }))
    );
}

#[test]
fn a_kvim_write_keeps_its_shape_across_the_boundary() {
    let mut clipboard = Clipboard::new(Box::new(MemoryClipboard::default()));
    for shape in [
        ClipboardShape::Characterwise,
        ClipboardShape::Linewise,
        ClipboardShape::Blockwise,
    ] {
        let value = ClipboardValue {
            text: "one\n",
            shape,
        };
        assert_eq!(clipboard.copy(value), None);
        assert_eq!(
            clipboard.paste(),
            ClipboardRead::Value(OwnedClipboardValue {
                text: "one\n".to_owned(),
                shape,
            }),
            "equal text keeps the recorded shape"
        );
    }
}

#[test]
fn an_external_copy_decides_its_shape_from_the_last_character() {
    for (text, shape) in [
        ("external", ClipboardShape::Characterwise),
        ("external\n", ClipboardShape::Linewise),
    ] {
        let mut clipboard = Clipboard::new(Box::new(MemoryClipboard::with_text(text)));
        assert_eq!(
            clipboard.paste(),
            ClipboardRead::Value(OwnedClipboardValue {
                text: text.to_owned(),
                shape,
            })
        );
    }
}

#[test]
fn an_external_copy_after_a_kvim_write_loses_the_recorded_shape() {
    let system = SharedClipboard::default();
    let mut clipboard = Clipboard::new(Box::new(system.clone()));
    assert_eq!(clipboard.copy(linewise("one\n")), None);

    // Another application replaced the text, so the recorded shape no longer
    // describes the clipboard.
    system.set("other");
    assert_eq!(
        clipboard.paste(),
        ClipboardRead::Value(OwnedClipboardValue {
            text: "other".to_owned(),
            shape: ClipboardShape::Characterwise,
        })
    );
}

#[test]
fn a_failed_write_forgets_the_recorded_shape() {
    /// A clipboard that accepts one write and refuses the next one.
    #[derive(Debug, Default)]
    struct FlakyClipboard {
        writes: usize,
        text: String,
    }

    impl SystemClipboard for FlakyClipboard {
        fn write(&mut self, text: &str) -> Result<(), ClipboardFailure> {
            self.writes += 1;
            if self.writes > 1 {
                return Err(ClipboardFailure::Failed);
            }
            self.text = text.to_owned();
            Ok(())
        }

        fn read(&mut self) -> Result<String, ClipboardFailure> {
            Ok(self.text.clone())
        }
    }

    let mut clipboard = Clipboard::new(Box::new(FlakyClipboard::default()));
    let block = ClipboardValue {
        text: "ab",
        shape: ClipboardShape::Blockwise,
    };
    assert_eq!(clipboard.copy(block), None);
    assert_eq!(
        clipboard.copy(characterwise("cd")),
        Some(ClipboardNotice::CommandFailed)
    );
    // The clipboard still holds "ab", but Kvim no longer claims it, so the text
    // decides the shape.
    assert_eq!(
        clipboard.paste(),
        ClipboardRead::Value(OwnedClipboardValue {
            text: "ab".to_owned(),
            shape: ClipboardShape::Characterwise,
        })
    );
}

#[test]
fn a_clipboard_that_returns_other_bytes_reports_a_text_failure() {
    /// A clipboard that returns bytes which are not UTF-8 text.
    #[derive(Debug)]
    struct BinaryClipboard;

    impl SystemClipboard for BinaryClipboard {
        fn write(&mut self, _text: &str) -> Result<(), ClipboardFailure> {
            Ok(())
        }

        fn read(&mut self) -> Result<String, ClipboardFailure> {
            Err(ClipboardFailure::NotText)
        }
    }

    let mut clipboard = Clipboard::new(Box::new(BinaryClipboard));
    assert_eq!(
        clipboard.paste(),
        ClipboardRead::Fallback(Some(ClipboardNotice::NotText))
    );
}

#[test]
fn the_macos_clipboard_runs_the_documented_commands() {
    let executor = RecordingExecutor::new("alpha");
    let mut clipboard = MacOsClipboard::new(executor.clone());
    clipboard.write("alpha").expect("the executor succeeds");
    assert_eq!(clipboard.read(), Ok("alpha".to_owned()));
    assert_eq!(
        executor.recorded(),
        vec![
            ("pbcopy".to_owned(), Vec::new()),
            ("pbpaste".to_owned(), Vec::new()),
        ]
    );
}

/// One expected command, as `docs/clipboard.md` records it.
struct ExpectedCommand {
    program: &'static str,
    args: &'static [&'static str],
}

impl ExpectedCommand {
    fn recorded(&self) -> (String, Vec<String>) {
        (
            self.program.to_owned(),
            self.args.iter().map(|arg| (*arg).to_owned()).collect(),
        )
    }
}

#[test]
fn the_linux_clipboard_runs_the_documented_commands_of_its_tool() {
    let expected = [
        (
            LinuxTool::Wayland,
            ExpectedCommand {
                program: "wl-copy",
                args: &[],
            },
            ExpectedCommand {
                program: "wl-paste",
                args: &["--no-newline"],
            },
        ),
        (
            LinuxTool::XClip,
            ExpectedCommand {
                program: "xclip",
                args: &["-selection", "clipboard"],
            },
            ExpectedCommand {
                program: "xclip",
                args: &["-selection", "clipboard", "-o"],
            },
        ),
        (
            LinuxTool::XSel,
            ExpectedCommand {
                program: "xsel",
                args: &["--clipboard", "--input"],
            },
            ExpectedCommand {
                program: "xsel",
                args: &["--clipboard", "--output"],
            },
        ),
    ];

    for (tool, write, read) in expected {
        let executor = RecordingExecutor::new("alpha");
        let mut clipboard = LinuxClipboard::new(tool, executor.clone());
        clipboard.write("alpha").expect("the executor succeeds");
        assert_eq!(clipboard.read(), Ok("alpha".to_owned()));
        assert_eq!(clipboard.tool(), tool);
        assert_eq!(executor.recorded(), vec![write.recorded(), read.recorded()]);
    }
}

#[test]
fn every_clipboard_request_carries_the_transfer_bound_and_the_text() {
    let executor = RecordingExecutor::new("");
    let mut clipboard = LinuxClipboard::new(LinuxTool::XClip, executor.clone());
    clipboard.write("alpha").expect("the executor succeeds");

    let requests = executor.requests.borrow();
    let request = requests.first().expect("the write ran one command");
    assert_eq!(request.stdin, b"alpha");
    assert_eq!(request.output_bytes_max, CLIPBOARD_BYTES_MAX);
}

#[test]
fn a_non_zero_exit_status_is_a_command_failure() {
    let mut clipboard = MacOsClipboard::new(RecordingExecutor::failing());
    assert_eq!(clipboard.write("alpha"), Err(ClipboardFailure::Failed));
    assert_eq!(clipboard.read(), Err(ClipboardFailure::Failed));
}

#[test]
fn a_signal_that_ended_the_command_is_a_command_failure() {
    // A signal leaves no exit status, so the command reported no success and
    // the failure is proven.
    let mut clipboard = MacOsClipboard::new(RecordingExecutor::signalled());
    assert_eq!(clipboard.write("alpha"), Err(ClipboardFailure::Failed));
    assert_eq!(clipboard.read(), Err(ClipboardFailure::Failed));
    assert_eq!(
        ClipboardFailure::Failed.evidence(),
        ClipboardEvidence::Failure
    );

    let mut boundary = Clipboard::new(Box::new(
        MacOsClipboard::new(RecordingExecutor::signalled()),
    ));
    assert_eq!(
        boundary.copy(characterwise("alpha")),
        Some(ClipboardNotice::CommandFailed),
        "a command that a signal ended still reaches the message line"
    );
}

#[test]
fn a_command_that_reported_no_outcome_reports_nothing() {
    // `wl-copy` and `xclip` own the selection through a background process that
    // holds the captured output streams open, so a write that succeeded reaches
    // its deadline. Kvim never learned that a transfer failed, so it must show
    // no failure. See `docs/clipboard.md`.
    for failure in [ClipboardFailure::Timeout, ClipboardFailure::Cancelled] {
        assert_eq!(
            failure.evidence(),
            ClipboardEvidence::Unknown,
            "{failure} proves nothing about the transfer"
        );
        let mut clipboard = Clipboard::new(Box::new(FailingClipboard(failure)));
        assert_eq!(
            clipboard.copy(linewise("one\n")),
            None,
            "{failure} must not report a clipboard failure"
        );
        assert_eq!(
            clipboard.paste(),
            ClipboardRead::Fallback(None),
            "{failure} falls back to the editor register without a report"
        );
    }
}

#[test]
fn a_refused_command_never_loses_the_editor_value() {
    /// An executor that refuses every command, as a saturated service does.
    #[derive(Debug)]
    struct RefusingExecutor;

    impl ProcessExecutor for RefusingExecutor {
        fn run(&self, _request: ProcessRequest) -> Result<ProcessOutput, ClipboardFailure> {
            Err(ClipboardFailure::Refused)
        }
    }

    let mut clipboard = Clipboard::new(Box::new(MacOsClipboard::new(RefusingExecutor)));
    assert_eq!(
        clipboard.copy(linewise("one\n")),
        Some(ClipboardNotice::CommandFailed)
    );
    assert_eq!(
        clipboard.paste(),
        ClipboardRead::Fallback(Some(ClipboardNotice::CommandFailed))
    );
}

#[test]
fn a_wayland_session_prefers_wayland_and_falls_back_through_x11() {
    assert_eq!(
        select_linux_tool(DisplaySession::Wayland, |_| true),
        Some(LinuxTool::Wayland)
    );
    assert_eq!(
        select_linux_tool(DisplaySession::Wayland, |name| !name.starts_with("wl-")),
        Some(LinuxTool::XClip)
    );
    assert_eq!(
        select_linux_tool(DisplaySession::Wayland, |name| name == "xsel"),
        Some(LinuxTool::XSel)
    );
    assert_eq!(
        select_linux_tool(DisplaySession::X11, |_| true),
        Some(LinuxTool::XClip),
        "an X11 session never selects the Wayland tool"
    );
    assert_eq!(select_linux_tool(DisplaySession::X11, |_| false), None);
}

#[test]
fn a_mac_os_host_needs_both_commands_to_select_the_mac_os_clipboard() {
    assert_eq!(
        ClipboardSelection::select(ClipboardPlatform::MacOs, DisplaySession::X11, |_| true),
        ClipboardSelection::MacOs
    );
    assert_eq!(
        ClipboardSelection::select(ClipboardPlatform::MacOs, DisplaySession::X11, |name| name
            == "pbcopy"),
        ClipboardSelection::Absent,
        "a host that reads no clipboard has no usable clipboard"
    );
}

#[test]
fn a_host_without_a_tool_selects_no_clipboard() {
    assert_eq!(
        ClipboardSelection::select(ClipboardPlatform::Linux, DisplaySession::Wayland, |_| false),
        ClipboardSelection::Absent
    );
    assert_eq!(
        ClipboardSelection::select(ClipboardPlatform::Other, DisplaySession::X11, |_| true),
        ClipboardSelection::Absent
    );
    assert_eq!(
        ClipboardSelection::Absent.commands(),
        None,
        "a host without a command names no command"
    );
}

#[test]
fn every_selection_names_the_documented_commands() {
    let macos = ClipboardSelection::MacOs
        .commands()
        .expect("the macOS selection names its commands");
    assert_eq!(macos.write.to_string(), "pbcopy");
    assert_eq!(macos.read.to_string(), "pbpaste");

    let expected = [
        (LinuxTool::Wayland, "wl-copy", "wl-paste --no-newline"),
        (
            LinuxTool::XClip,
            "xclip -selection clipboard",
            "xclip -selection clipboard -o",
        ),
        (
            LinuxTool::XSel,
            "xsel --clipboard --input",
            "xsel --clipboard --output",
        ),
    ];
    for (tool, write, read) in expected {
        let selection = ClipboardSelection::Linux {
            session: DisplaySession::X11,
            tool,
        };
        let commands = selection
            .commands()
            .expect("a Linux selection names its commands");
        assert_eq!(commands.write.to_string(), write);
        assert_eq!(commands.read.to_string(), read);
    }
}
