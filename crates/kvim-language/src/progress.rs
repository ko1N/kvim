//! The work-done progress of one language server.
//!
//! A server reports the state of a long operation, for example an index run,
//! through the `$/progress` notification. The session parses that notification
//! here and publishes one [`ProgressReport`]. Nothing above this module parses
//! protocol text. See `docs/language-services.md`.

use serde::Deserialize;
use serde_json::value::RawValue;

/// The largest number of characters that one progress string may hold.
///
/// A token, a title, and a message all pass this bound. A longer value is a
/// report that the overlay cannot show, so the session drops it instead of
/// keeping an unbounded string.
pub const LSP_PROGRESS_CHARS_MAX: usize = 128;

/// The attempt of one session that produced one report.
///
/// A session restarts after a server failure, and the new server assigns its
/// own tokens. The generation therefore separates the reports of two attempts,
/// so a report of the attempt that failed can never change visible state.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SessionGeneration(u64);

impl SessionGeneration {
    /// The generation of the first attempt of one session.
    pub const FIRST: Self = Self(0);

    /// Returns the generation of the attempt that follows this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// Returns the underlying value for logs and comparisons.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// The identity that one server assigns to one progress item.
///
/// The protocol allows a string token and an integer token. The boundary
/// normalizes both into one bounded string, so one comparison covers both
/// forms. A value that no `begin` created addresses no item.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProgressToken(String);

impl ProgressToken {
    /// Creates a token from one server value.
    ///
    /// Returns `None` for an empty token and for a token above
    /// [`LSP_PROGRESS_CHARS_MAX`] characters, because neither addresses an item
    /// that the overlay can show.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_language::ProgressToken;
    ///
    /// assert!(ProgressToken::new("rustAnalyzer/Indexing".to_owned()).is_some());
    /// assert!(ProgressToken::new(String::new()).is_none());
    /// ```
    #[must_use]
    pub fn new(value: String) -> Option<Self> {
        if value.is_empty() || value.chars().count() > LSP_PROGRESS_CHARS_MAX {
            return None;
        }
        Some(Self(value))
    }

    /// Returns the token text.
    #[must_use]
    pub fn get(&self) -> &str {
        &self.0
    }
}

/// The completion of one progress item, in percent.
///
/// The protocol bounds the value at 100. A server that reports more describes
/// no state that the overlay can show, so the boundary rejects it.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ProgressPercentage(u8);

impl ProgressPercentage {
    /// Creates a percentage from one reported value.
    ///
    /// Returns `None` above 100.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_language::ProgressPercentage;
    ///
    /// assert_eq!(ProgressPercentage::new(42).map(ProgressPercentage::get), Some(42));
    /// assert!(ProgressPercentage::new(101).is_none());
    /// ```
    #[must_use]
    pub const fn new(value: u8) -> Option<Self> {
        if value > 100 { None } else { Some(Self(value)) }
    }

    /// Returns the percentage value.
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// The stage that one progress notification reports.
///
/// Each variant carries exactly the data that the protocol defines for it, so
/// no reader has to decide which fields of one report are meaningful.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProgressStage {
    /// The operation started. Only this stage creates an item.
    Begin {
        /// The short title of the operation.
        title: String,
        /// The detail of the operation, when the server reports one.
        message: Option<String>,
        /// The completion of the operation, when the server reports one.
        percentage: Option<ProgressPercentage>,
    },
    /// The operation progressed.
    Report {
        /// The new detail, when the server reports one.
        message: Option<String>,
        /// The new completion, when the server reports one.
        percentage: Option<ProgressPercentage>,
    },
    /// The operation finished.
    End {
        /// The closing detail, when the server reports one.
        message: Option<String>,
    },
}

/// One work-done progress report of one server attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProgressReport {
    /// The attempt that produced the report.
    pub generation: SessionGeneration,
    /// The program that runs the server, which titles the overlay group.
    pub server: &'static str,
    /// The item that the report addresses.
    pub token: ProgressToken,
    /// The stage of the report.
    pub stage: ProgressStage,
}

/// The wire shape of one `$/progress` notification.
#[derive(Deserialize)]
struct RawProgress {
    token: RawToken,
    value: RawStage,
}

/// The wire shape of one progress token.
#[derive(Deserialize)]
#[serde(untagged)]
enum RawToken {
    /// A token that the server sent as a string.
    Text(String),
    /// A token that the server sent as an integer.
    Number(i64),
}

/// The wire shape of one progress value.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
enum RawStage {
    Begin {
        title: String,
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        percentage: Option<u32>,
    },
    Report {
        #[serde(default)]
        message: Option<String>,
        #[serde(default)]
        percentage: Option<u32>,
    },
    End {
        #[serde(default)]
        message: Option<String>,
    },
}

/// Parses one `$/progress` notification.
///
/// Returns `None` for a notification that reports no work-done stage that the
/// overlay can show, and for a token that addresses no item. The same method
/// also carries the partial results of a request, whose value holds no stage at
/// all. Progress is decoration, so every unreadable report is dropped and none
/// of them fails the session.
pub(super) fn parse(
    params: Option<&RawValue>,
    generation: SessionGeneration,
    server: &'static str,
) -> Option<ProgressReport> {
    let raw: RawProgress = serde_json::from_str(params?.get()).ok()?;
    let token = match raw.token {
        RawToken::Text(text) => text,
        RawToken::Number(number) => number.to_string(),
    };
    Some(ProgressReport {
        generation,
        server,
        token: ProgressToken::new(token)?,
        stage: stage(raw.value),
    })
}

/// Converts one wire value into one stage.
///
/// A string above [`LSP_PROGRESS_CHARS_MAX`] is clipped, and a percentage
/// outside the protocol range is dropped, because progress is decoration and
/// must never fail the session.
fn stage(value: RawStage) -> ProgressStage {
    match value {
        RawStage::Begin {
            title,
            message,
            percentage,
        } => ProgressStage::Begin {
            title: clipped(title),
            message: message.map(clipped),
            percentage: completion(percentage),
        },
        RawStage::Report {
            message,
            percentage,
        } => ProgressStage::Report {
            message: message.map(clipped),
            percentage: completion(percentage),
        },
        RawStage::End { message } => ProgressStage::End {
            message: message.map(clipped),
        },
    }
}

/// Clips one string to [`LSP_PROGRESS_CHARS_MAX`] characters.
fn clipped(text: String) -> String {
    if text.chars().count() <= LSP_PROGRESS_CHARS_MAX {
        return text;
    }
    text.chars().take(LSP_PROGRESS_CHARS_MAX).collect()
}

/// Returns the reported completion, or `None` outside the protocol range.
fn completion(percentage: Option<u32>) -> Option<ProgressPercentage> {
    percentage
        .and_then(|value| u8::try_from(value).ok())
        .and_then(ProgressPercentage::new)
}
