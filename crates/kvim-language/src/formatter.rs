//! The external formatter declaration of one adapter, and one bounded run of it.
//!
//! A declaration is data: the program and its arguments in command order. The
//! editor runs what the declaration names and knows no formatter product.
//! Adding an external formatter therefore means adding one declaration to one
//! adapter, and nothing above the adapter boundary changes.
//!
//! An external formatter takes precedence over a formatting language server.
//! kvim sends a document-formatting request only while its adapter declares no
//! program. See `docs/language-services.md`.
//!
//! The editor never runs the program itself. It builds one [`ProcessRequest`],
//! the bounded process service of `kvim-runtime` runs it, and
//! [`FormatterRequest::publish`] turns the captured output into one formatted
//! document. See `docs/responsiveness.md`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use kvim_core::{BufferVersion, CharPosition, CharRange, EditTransaction, TextBuffer, TextChange};
use kvim_runtime::{ProcessOutput, ProcessRequest, RuntimeError};
use kvim_settings::FILE_BYTES_MAX;

use super::server::LanguageServerDeclaration;

/// The largest number of arguments that one formatter declaration names.
///
/// One formatter names a subcommand, a standard-input flag, and the document
/// path. Eight covers that practice and still bounds the command of one buffer.
pub const FORMATTER_ARGS_MAX: usize = 8;

/// The captured output of one formatter run, in bytes.
///
/// The limit counts standard output and standard error together. `text-model.md`
/// bounds one file at 4 MiB, so 8 MiB holds the formatted document beside the
/// warnings of the program.
pub const FORMATTER_OUTPUT_BYTES_MAX: usize = 8 * 1024 * 1024;

/// The deadline of one formatter run.
///
/// A cold formatter reads its configuration before it formats. The value
/// matches [`crate::LSP_FORMAT_DEADLINE`] and the process deadline default of
/// `docs/responsiveness.md`.
pub const FORMATTER_DEADLINE: Duration = Duration::from_secs(10);

/// One argument of an external formatter command.
///
/// A formatter that reads its rules from the file name needs the document path
/// although the document arrives on standard input. The declaration names the
/// place of that path, and the caller substitutes the path of its buffer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatterArgument {
    /// The exact text of the argument.
    Literal(&'static str),
    /// The path of the document that the formatter reads on standard input.
    DocumentPath,
}

/// The external formatter of one language adapter.
///
/// # Examples
///
/// ```
/// use kvim_language::{FormatterArgument, LanguageAdapter, MarkdownAdapter};
///
/// let declaration = MarkdownAdapter::new()
///     .external_formatter()
///     .expect("the Markdown adapter declares a formatter");
/// assert_eq!(declaration.program, "prettier");
/// // `prettier` selects its parser from the file name of the document.
/// assert_eq!(declaration.args[1], FormatterArgument::DocumentPath);
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FormatterDeclaration {
    /// The executable that formats one document.
    pub program: &'static str,
    /// The arguments of that executable, in command order.
    pub args: &'static [FormatterArgument],
}

/// Reports whether one adapter declares a valid external formatter.
///
/// The declaration names a program, and it holds at most
/// [`FORMATTER_ARGS_MAX`] arguments. The rules belong to
/// `docs/language-services.md`, and a debug assertion of the request checks
/// them once for each declaration.
#[must_use]
pub(super) const fn declaration_is_valid(declaration: &FormatterDeclaration) -> bool {
    !declaration.program.is_empty() && declaration.args.len() <= FORMATTER_ARGS_MAX
}

/// Why one formatter changed no buffer content.
///
/// None of these states is a failure of the editor. The buffer stays exactly as
/// the user typed it, and the save that waited for the formatter still writes
/// that content. See `docs/language-services.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FormatterFailure {
    /// The host holds no such program.
    ///
    /// The editor names this state once for each session, because it never
    /// changes while the editor runs.
    NotInstalled,
    /// The formatter produced no usable document.
    ///
    /// The program reported a non-zero exit code, wrote no text, wrote bytes
    /// that are not UTF-8, wrote more than its output bound, or passed its
    /// deadline. The request may also have been cancelled or refused.
    Unavailable,
    /// The buffer changed after the request.
    ///
    /// The answer describes content that the buffer no longer holds, so kvim
    /// discards it and keeps the content that the user typed.
    Obsolete,
}

impl FormatterFailure {
    /// Returns the formatter state of one runtime failure.
    ///
    /// A program that cannot start is a normal state: the editor names it once
    /// and stays usable without the formatter.
    #[must_use]
    pub const fn of(error: &RuntimeError) -> Self {
        match error {
            RuntimeError::ProcessSpawn(_) => Self::NotInstalled,
            RuntimeError::Cancelled
            | RuntimeError::Timeout
            | RuntimeError::WorkerFailure(_)
            | RuntimeError::ProcessRead(_)
            | RuntimeError::ProcessWrite(_)
            | RuntimeError::OutputLimit { .. } => Self::Unavailable,
        }
    }
}

/// One bounded run of the external formatter of one buffer version.
///
/// # Examples
///
/// ```
/// use std::path::PathBuf;
///
/// use kvim_core::TextBuffer;
/// use kvim_language::{FormatterRequest, LanguageAdapter, NixAdapter};
/// use kvim_settings::FileSettings;
///
/// let declaration = NixAdapter::new()
///     .external_formatter()
///     .expect("the Nix adapter declares a formatter");
/// let buffer = TextBuffer::from_text("{  }\n", kvim_core::BufferBytesMax::default())
///     .expect("the text is small");
/// let request = FormatterRequest::new(
///     declaration,
///     PathBuf::from("/work/flake.nix"),
///     buffer.version(),
///     buffer.to_string(),
/// );
/// let command = request.command();
/// assert_eq!(command.program, "nixfmt");
/// assert_eq!(command.stdin, b"{  }\n");
/// ```
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormatterRequest {
    /// The formatter that the adapter of the document declares.
    declaration: &'static FormatterDeclaration,
    /// The document that the formatter formats.
    path: PathBuf,
    /// The buffer version that produced the content below.
    version: BufferVersion,
    /// The exact buffer text of that version.
    content: String,
}

impl FormatterRequest {
    /// Creates one run of the declared formatter over one buffer version.
    ///
    /// `content` is the exact text of `version`, because the answer replaces
    /// that text. The caller reads both from the same buffer.
    #[must_use]
    pub fn new(
        declaration: &'static FormatterDeclaration,
        path: PathBuf,
        version: BufferVersion,
        content: String,
    ) -> Self {
        debug_assert!(
            declaration_is_valid(declaration),
            "an adapter declares a program and at most FORMATTER_ARGS_MAX arguments"
        );
        Self {
            declaration,
            path,
            version,
            content,
        }
    }

    /// Returns the document that this run formats.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the buffer version that this run formats.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns the bounded command of one formatter run.
    ///
    /// The command carries the exact buffer text on standard input, and it
    /// substitutes the document path for every path argument of the
    /// declaration.
    #[must_use]
    pub fn command(&self) -> ProcessRequest {
        let mut request = ProcessRequest::new(self.declaration.program);
        request.args = self
            .declaration
            .args
            .iter()
            .map(|argument| match argument {
                FormatterArgument::Literal(text) => OsString::from(*text),
                FormatterArgument::DocumentPath => self.path.clone().into_os_string(),
            })
            .collect();
        request.stdin = self.content.clone().into_bytes();
        request.output_bytes_max = FORMATTER_OUTPUT_BYTES_MAX;
        request.deadline = FORMATTER_DEADLINE;
        request
    }

    /// Turns the captured output of one formatter run into one document.
    ///
    /// `None` reports that the buffer already matches its formatter, so the
    /// answer changes nothing and records no undo step.
    ///
    /// # Errors
    ///
    /// Returns [`FormatterFailure::Unavailable`] when the program reported a
    /// non-zero exit code, wrote bytes that are not UTF-8, wrote no text
    /// although the buffer holds text, or wrote a document above the maximum
    /// file size of `text-model.md`.
    pub fn publish(
        &self,
        output: &ProcessOutput,
    ) -> Result<Option<FormattedDocument>, FormatterFailure> {
        // Every formatter reports a refusal through its exit code. No branch
        // reads the message text of its standard error.
        if output.status_code != Some(0) {
            return Err(FormatterFailure::Unavailable);
        }
        let formatted =
            std::str::from_utf8(&output.stdout).map_err(|_| FormatterFailure::Unavailable)?;
        // A program that writes nothing formatted nothing, so its answer would
        // empty a buffer that the user still holds.
        if formatted.is_empty() && !self.content.is_empty() {
            return Err(FormatterFailure::Unavailable);
        }
        // A document above the file bound would build a buffer that kvim
        // refuses to load. See `docs/text-model.md`.
        if formatted.len() as u64 > FILE_BYTES_MAX {
            return Err(FormatterFailure::Unavailable);
        }
        if formatted == self.content {
            return Ok(None);
        }
        Ok(Some(FormattedDocument {
            version: self.version,
            text: formatted.to_owned(),
        }))
    }
}

/// The document that one external formatter produced for one buffer version.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FormattedDocument {
    /// The buffer version that produced the text below.
    version: BufferVersion,
    /// The formatted document.
    text: String,
}

impl FormattedDocument {
    /// Returns the buffer version that produced this document.
    #[must_use]
    pub const fn version(&self) -> BufferVersion {
        self.version
    }

    /// Returns the formatted document.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Builds one undoable transaction for the current buffer.
    ///
    /// The answer replaces the complete document, so one undo reverses a
    /// complete format.
    ///
    /// # Errors
    ///
    /// Returns [`FormatterFailure::Obsolete`] when the buffer changed after the
    /// request, because the answer then describes content that the buffer no
    /// longer holds.
    pub fn transaction(
        &self,
        buffer: &TextBuffer,
        cursor: CharPosition,
    ) -> Result<EditTransaction, FormatterFailure> {
        if buffer.version().get() != self.version.get() {
            return Err(FormatterFailure::Obsolete);
        }
        let (Ok(start), Ok(end)) = (
            buffer.char_position(0),
            buffer.char_position(buffer.len_chars()),
        ) else {
            debug_assert!(
                false,
                "zero and the buffer length are always valid character positions"
            );
            return Err(FormatterFailure::Obsolete);
        };
        let Ok(range) = CharRange::new(start, end) else {
            debug_assert!(false, "zero never follows the length of the same buffer");
            return Err(FormatterFailure::Obsolete);
        };
        Ok(EditTransaction::single(
            cursor,
            TextChange::replace(range, self.text.clone()),
        ))
    }
}

/// The formatter that formats the buffers of one language adapter.
///
/// An external formatter takes precedence over a formatting server, so this
/// value names the one path that a format-on-save runs. See
/// `docs/language-services.md`.
#[derive(Clone, Copy)]
pub enum LanguageFormatter {
    /// An external program formats the buffer.
    External(&'static FormatterDeclaration),
    /// The declared language server formats the buffer.
    Server(&'static LanguageServerDeclaration),
}
