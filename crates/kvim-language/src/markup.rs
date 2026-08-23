//! The markdown of one server answer, as a value that the rendering paints.
//!
//! A language server writes markdown, so an answer that reached the screen as
//! raw text showed its own source: literal asterisks, literal backticks, and an
//! unrendered fence. This module reads that text and answers one
//! [`MarkupDocument`]: a sequence of blocks, each holding styled pieces of
//! text.
//!
//! [`MarkupDocument::parse`] is pure. It reads no clock, no environment
//! variable, no file, and no socket, so one text always produces one document.
//!
//! The parse is `pulldown-cmark`, and no type of that crate leaves this module.
//! The document is a value of kvim, so the dependency stays at one boundary.
//!
//! The block model follows the markdown renderer of `keel`, the editor of the
//! same author, which solved the same problem for the answer of a model.
//!
//! # The code of one fence carries the roles of the editor
//!
//! A fence names its language in an info string, and only an adapter may select
//! by that name, so the highlight of a fence stays in this crate.
//! [`MarkupDocument::highlighted`] reads the name of each fence, selects the
//! adapter of that name, and collects the spans of the one highlighter that
//! also serves a buffer. One text therefore carries one set of roles wherever
//! it stands.
//!
//! The highlight is Tree-sitter work, which the terminal event loop must never
//! run. The caller runs [`MarkupDocument::highlighted`] where the answer of the
//! server arrives, off that loop, so the float paints a finished value. See
//! `docs/language-services.md` and `docs/responsiveness.md`.
//!
//! Two servers answer one hover on their own, and each answer therefore carries
//! its own document. [`MarkupDocument::joined`] appends the blocks of one
//! document to another, so the reader of two answers still paints one document
//! and never parses a text that a join produced.
//!
//! # The value holds no glyph, no width, and no color
//!
//! `kvim-tui` owns the palette, and it owns the terminal cell as well, because
//! `unicode-width` runs in that crate alone. This module therefore answers a
//! role and a structure, and the renderer answers every visible cell. A
//! thematic break is one block and not one row of dashes. A list item names its
//! marker and not the text of that marker, because a marker and the blanks that
//! replace it must occupy one terminal width. See `docs/language-services.md`.

use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Parser, Tag, TagEnd};
use tokio_util::sync::CancellationToken;

use crate::session::LSP_HOVER_BYTES_MAX;
use crate::{HighlightSpan, LanguageRegistry, analysis};

/// The source bytes that one parse reads.
///
/// One hover answer is the largest markup that the editor reads today, and
/// [`LSP_HOVER_BYTES_MAX`] already bounds it. One constant holds both, so the
/// two cannot drift. A longer text stops at the last character boundary below
/// the bound, and the document reports that it is clipped.
pub const MARKUP_SOURCE_BYTES_MAX: usize = LSP_HOVER_BYTES_MAX;

/// The blocks that one document holds.
///
/// One block needs at least one character of its own, so a source of
/// [`MARKUP_SOURCE_BYTES_MAX`] bytes holds far more blocks than one float
/// shows. The parse stops at this bound and reports the document as clipped.
pub const MARKUP_BLOCKS_MAX: usize = 256;

/// The styled pieces that one document holds, over every block of it.
///
/// One piece is one stretch of text that the parse appends in one role, and one
/// line of a code block counts as one piece. The walk tests the count before
/// each event, so the bound stops the parse between two events. The lines of one
/// code block join the count when that block closes, so they stop the parse
/// after it and never inside it. [`MARKUP_SOURCE_BYTES_MAX`] bounds the lines of
/// one such block. The parse degrades at this bound exactly as it does at
/// [`MARKUP_BLOCKS_MAX`].
pub const MARKUP_PIECES_MAX: usize = 2048;

/// The containers that the prefix of one block follows.
///
/// A quote inside a quote inside a list indents the text of a block, and a
/// server can nest without a limit. A container below this depth adds no
/// further prefix, so the text of the block still reaches the screen and the
/// prefix cannot consume the whole width.
pub const MARKUP_NESTING_DEPTH_MAX: usize = 8;

/// The source of one fence that the highlight reads, in bytes.
///
/// The float shows at most 16 rows of 96 terminal cells, which is about 1536
/// bytes of source, so this bound holds every fence that a reader can see more
/// than twice over. One fence of this size costs about 1.4 ms of highlight
/// work in a release build, and one real hover answer of two fences costs
/// about 68 us. A longer fence keeps every line and carries no span, so it
/// reads as plain code and costs no highlight at all.
pub const MARKUP_FENCE_SOURCE_BYTES_MAX: usize = 4 * 1024;

/// The highlight spans that one fence produces.
///
/// A dense measured fence of 4061 bytes produces 918 spans, which is one span
/// for each 4.4 bytes. This bound holds one span for each two bytes of
/// [`MARKUP_FENCE_SOURCE_BYTES_MAX`], so no real fence reaches it. One span
/// holds 16 bytes, so the bound retains 32 KiB for one fence. A fence that
/// passes the bound carries no span at all, because kvim publishes no partial
/// result.
pub const MARKUP_FENCE_SPANS_MAX: usize = 2048;

/// The fences of one document that the highlight reads.
///
/// [`MARKUP_SOURCE_BYTES_MAX`] already bounds the text of every fence
/// together, and this bound holds the setup cost of one highlight, which the
/// source bound does not hold. That cost measures about 1.4 us for a fence
/// that holds no line. A fence occupies at least one row of the float, and one
/// blank row stands above it, so a float of 16 rows shows at most 8 fences. A
/// later fence keeps every line and carries no span.
pub const MARKUP_FENCES_MAX: usize = 16;

/// What one stretch of text of a document is.
///
/// A role names the meaning of the text, never its color. `kvim-tui` maps each
/// role to one style, exactly as it maps a highlight role. See
/// `docs/windows.md`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum MarkupRole {
    /// The body text of one block.
    Text,
    /// The text of one heading.
    Heading,
    /// Text between one pair of emphasis markers.
    Emphasis,
    /// Text between one pair of strong markers.
    Strong,
    /// One code span inside a text.
    InlineCode,
    /// The text of one link. The destination is markup, so it never paints.
    Link,
    /// The text inside one block quote.
    Quote,
}

/// One stretch of text in one role, addressed by where it ends.
///
/// The runs of a [`StyledMarkup`] partition its text in order, so the run that
/// owns a byte offset is the first run that ends after it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct StyleRun {
    /// The byte offset at which the run ends, exclusive.
    end: usize,
    /// The role that paints the run.
    role: MarkupRole,
}

/// One text and the roles that name its parts.
///
/// The text is one string and the roles address it by byte range, so a wrapped
/// line is a slice of that string and a role survives a wrap point on both
/// sides of it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StyledMarkup {
    /// The whole text, without a line feed.
    text: String,
    /// The runs that partition [`StyledMarkup::text`], in order.
    runs: Vec<StyleRun>,
}

impl StyledMarkup {
    /// Returns the whole text, without a line feed.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Reports whether the text holds no character.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// Returns every run of the text as one piece, in order.
    #[must_use]
    pub fn pieces(&self) -> Vec<(&str, MarkupRole)> {
        self.pieces_in(0, self.text.len())
    }

    /// Returns the pieces of one byte range of the text, in order.
    ///
    /// Both ends must address a character boundary. A wrapped line starts and
    /// ends at a grapheme cluster boundary, and a run ends where one parser
    /// event ended, so a renderer meets both by construction.
    #[must_use]
    pub fn pieces_in(&self, start: usize, end: usize) -> Vec<(&str, MarkupRole)> {
        debug_assert!(
            self.text.is_char_boundary(start.min(self.text.len())),
            "a caller wraps between grapheme clusters, so a line starts at a character boundary"
        );
        debug_assert!(
            self.text.is_char_boundary(end.min(self.text.len())),
            "a caller wraps between grapheme clusters, so a line ends at a character boundary"
        );

        let end = end.min(self.text.len());
        let mut pieces = Vec::new();
        let mut run_start = 0;

        for run in &self.runs {
            let from = run_start.max(start);
            let to = run.end.min(end);
            if from < to {
                pieces.push((&self.text[from..to], run.role));
            }

            run_start = run.end;
            if run_start >= end {
                break;
            }
        }

        pieces
    }

    /// Appends text in one role.
    ///
    /// Text in the role of the run before it extends that run, so a paragraph
    /// that arrived as many events holds one run and not one run for each
    /// event.
    fn push(&mut self, text: &str, role: MarkupRole) {
        if text.is_empty() {
            return;
        }

        self.text.push_str(text);
        match self.runs.last_mut() {
            Some(run) if run.role == role => run.end = self.text.len(),
            _ => self.runs.push(StyleRun {
                end: self.text.len(),
                role,
            }),
        }

        debug_assert_eq!(
            self.runs.last().map(|run| run.end),
            Some(self.text.len()),
            "each push extends the last run or adds one that ends at the text"
        );
    }
}

/// The marker of one list item.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkupMarker {
    /// One item of an unordered list.
    Bullet,
    /// One item of an ordered list, and the number of that item.
    Ordered(u64),
}

/// One container that rails or indents the rows of a block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MarkupContainer {
    /// One block quote. Every row of the block carries its rail.
    Quote,
    /// One list. `marker` names the item that this block opens, and no marker
    /// stands on a block that continues an item that another block opened.
    List {
        /// The marker of the item that this block opens.
        marker: Option<MarkupMarker>,
    },
}

impl MarkupContainer {
    /// Returns the container of a block that continues the block before it.
    ///
    /// One item marker stands on one block only, so a continuation carries the
    /// indentation of the list and no marker.
    const fn continued(self) -> Self {
        match self {
            Self::Quote => Self::Quote,
            Self::List { .. } => Self::List { marker: None },
        }
    }
}

/// What one block of a document holds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MarkupBody {
    /// One paragraph or one list item, which wraps at the available width.
    Prose(StyledMarkup),
    /// One heading, and the rank of that heading.
    Heading {
        /// The rank of the heading, 1 through 6.
        level: u8,
        /// The text of the heading.
        text: StyledMarkup,
    },
    /// One fenced or indented code block, which keeps its source lines.
    Code {
        /// The info string of the fence, such as `rust`, or an empty string.
        info: String,
        /// The lines of the block, in order, without their line feeds.
        lines: Vec<String>,
        /// The syntax roles of the lines, or no span at all.
        ///
        /// One span addresses the line by its index in `lines` and the range
        /// by its bytes inside that line, exactly as one span of a buffer
        /// does. [`MarkupDocument::parse`] leaves the table empty, and
        /// [`MarkupDocument::highlighted`] fills it for a fence that names a
        /// registered language. An empty table therefore reads as plain code.
        highlights: Vec<HighlightSpan>,
    },
    /// One thematic break, which separates two parts of an answer.
    Rule,
}

/// One block of a document: the containers around it and the content inside it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkupBlock {
    /// Whether one blank row stands above the block.
    spaced: bool,
    /// The containers around the block, outermost first.
    containers: Vec<MarkupContainer>,
    /// The content of the block.
    body: MarkupBody,
}

impl MarkupBlock {
    /// Reports whether one blank row stands above the block.
    ///
    /// A paragraph, a heading, a code block, and a quote each open with one
    /// blank row, because the source separated them with a blank line. The
    /// blocks of one list follow one another without a blank row, so a list
    /// reads as one list and not as one paragraph for each item.
    #[must_use]
    pub fn is_spaced(&self) -> bool {
        self.spaced
    }

    /// Returns the containers around the block, outermost first.
    ///
    /// The list holds at most [`MARKUP_NESTING_DEPTH_MAX`] containers.
    #[must_use]
    pub fn containers(&self) -> &[MarkupContainer] {
        &self.containers
    }

    /// Returns the content of the block.
    #[must_use]
    pub fn body(&self) -> &MarkupBody {
        &self.body
    }

    /// Returns the styled pieces that the block holds.
    ///
    /// One stretch of text in one role is one piece, and one line of a code
    /// block is one piece as well, so the count follows the rule of
    /// [`MARKUP_PIECES_MAX`].
    fn pieces(&self) -> usize {
        match &self.body {
            MarkupBody::Prose(styled) | MarkupBody::Heading { text: styled, .. } => {
                styled.runs.len()
            }
            MarkupBody::Code { lines, .. } => lines.len(),
            MarkupBody::Rule => 1,
        }
    }
}

/// The markdown of one text, as blocks of styled text.
///
/// The document is derived and never stored: the session keeps the text that
/// the server wrote, and this value is a pure function of it.
///
/// # Examples
///
/// ```
/// use kvim_language::{LanguageRegistry, MarkupBody, MarkupDocument, SyntaxRole};
///
/// let document = MarkupDocument::parse("```rust\nfn main() {}\n```");
/// let MarkupBody::Code { info, lines, highlights } = document.blocks()[0].body() else {
///     panic!("the fence opens a code block");
/// };
/// assert_eq!(info, "rust");
/// assert_eq!(lines, &["fn main() {}".to_owned()]);
/// // The parse alone names no role, so the fence reads as plain code.
/// assert!(highlights.is_empty());
///
/// let document = document.highlighted(LanguageRegistry::first_release());
/// let MarkupBody::Code { highlights, .. } = document.blocks()[0].body() else {
///     panic!("the fence stays a code block");
/// };
/// assert_eq!(highlights[0].role, SyntaxRole::Keyword);
/// ```
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MarkupDocument {
    /// The blocks of the document, in order.
    blocks: Vec<MarkupBlock>,
    /// Whether one bound stopped the parse before the source ended.
    clipped: bool,
}

impl MarkupDocument {
    /// Reads the markdown of one text.
    ///
    /// The parse reads at most [`MARKUP_SOURCE_BYTES_MAX`] bytes, and it stops
    /// at [`MARKUP_BLOCKS_MAX`] blocks or [`MARKUP_PIECES_MAX`] pieces. The
    /// document reports every one of these through
    /// [`MarkupDocument::is_clipped`].
    #[must_use]
    pub fn parse(source: &str) -> Self {
        let bounded = bounded_source(source);
        let mut clipped = bounded.len() < source.len();
        let mut walk = Walk::default();

        for event in Parser::new(bounded) {
            if walk.exhausted() {
                clipped = true;
                break;
            }

            walk.handle(&event);
        }
        walk.close();

        debug_assert!(
            walk.blocks.len() <= MARKUP_BLOCKS_MAX + 1,
            "the bound check runs before each event, and the close adds at most one block"
        );

        Self {
            blocks: walk.blocks,
            clipped,
        }
    }

    /// Names the syntax roles of the code of each fence.
    ///
    /// The pass reads the language name of each info string, selects the
    /// adapter of that name, and collects the spans of the one highlighter
    /// that also serves a buffer. A fence therefore carries the roles that its
    /// code carries in a buffer of the same language.
    ///
    /// A fence that names no language, a fence that names a language of no
    /// adapter, and a fence that passes one bound all keep every line and
    /// carry no span. None of these is a failure, because a server may write
    /// any info string, and a fence without a span reads as plain code.
    ///
    /// The pass reads at most [`MARKUP_FENCES_MAX`] fences. It reads at most
    /// [`MARKUP_FENCE_SOURCE_BYTES_MAX`] bytes of one fence, and it keeps at
    /// most [`MARKUP_FENCE_SPANS_MAX`] spans of one fence.
    ///
    /// The highlight is Tree-sitter work, so the caller must run this pass off
    /// the terminal event loop. See `docs/responsiveness.md`.
    #[must_use]
    pub fn highlighted(mut self, registry: LanguageRegistry) -> Self {
        // The bounds above hold this pass below one frame, and the caller runs
        // it off the terminal event loop, so it needs no cancellation owner of
        // its own. The shared highlighter takes one token, and this token
        // stays uncancelled.
        let cancellation = CancellationToken::new();
        let mut fences = 0_usize;

        for block in &mut self.blocks {
            let MarkupBody::Code {
                info,
                lines,
                highlights,
            } = &mut block.body
            else {
                continue;
            };
            if fences >= MARKUP_FENCES_MAX {
                break;
            }
            let Some(adapter) = registry.adapter_of_language(fence_language(info)) else {
                continue;
            };
            let source = lines.join("\n");
            if source.len() > MARKUP_FENCE_SOURCE_BYTES_MAX {
                continue;
            }

            fences += 1;
            // A failed parse, a malformed span, and a fence above the span
            // bound all leave the fence plain, exactly as an unknown language
            // does. kvim publishes no partial result.
            if let Ok(spans) = analysis::collect_highlights(
                adapter.catalog(),
                &source,
                MARKUP_FENCE_SPANS_MAX,
                &cancellation,
            ) {
                *highlights = spans;
            }

            debug_assert!(
                highlights.len() <= MARKUP_FENCE_SPANS_MAX,
                "the highlighter stops at the bound that this pass gives it"
            );
        }

        self
    }

    /// Appends the blocks of one further document.
    ///
    /// Two servers answer one hover on their own, and each answer carries its
    /// own document, because the highlight of a fence runs where that answer
    /// arrives. The editor therefore joins documents and never joins two
    /// markdown texts.
    ///
    /// One blank row stands above the first block of `other`, so a reader sees
    /// where one answer ends and the next one starts. The join reports itself
    /// as clipped as soon as one of the two documents does.
    ///
    /// The bounds of one document hold over the join. It tests the two counts
    /// before each block, exactly as the parse tests them before each event, so
    /// it appends no block after the count reached [`MARKUP_BLOCKS_MAX`] or
    /// [`MARKUP_PIECES_MAX`]. It reports the join as clipped as soon as one of
    /// these bounds stops it.
    ///
    /// The join names no role and reads no grammar. It moves finished blocks,
    /// so the terminal event loop may run it. See `docs/language-services.md`.
    #[must_use]
    pub fn joined(mut self, other: &Self) -> Self {
        let held = self.blocks.len();
        let mut pieces = self.pieces();
        self.clipped |= other.clipped;

        for (index, block) in other.blocks.iter().enumerate() {
            if self.blocks.len() >= MARKUP_BLOCKS_MAX || pieces >= MARKUP_PIECES_MAX {
                self.clipped = true;
                break;
            }

            let mut block = block.clone();
            // The first block of a document opens no blank row, because it
            // stands at the top of its own answer. It follows another answer
            // here, so it opens one.
            if index == 0 && held > 0 {
                block.spaced = true;
            }
            pieces = pieces.saturating_add(block.pieces());
            self.blocks.push(block);
        }

        debug_assert!(
            self.blocks.len() <= held.max(MARKUP_BLOCKS_MAX),
            "the bound check runs before each appended block"
        );
        self
    }

    /// Returns the blocks of the document, in order.
    #[must_use]
    pub fn blocks(&self) -> &[MarkupBlock] {
        &self.blocks
    }

    /// Returns the styled pieces of every block of the document.
    fn pieces(&self) -> usize {
        self.blocks
            .iter()
            .map(MarkupBlock::pieces)
            .fold(0_usize, usize::saturating_add)
    }

    /// Reports whether the document holds no block.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Reports whether one bound stopped the parse before the source ended.
    ///
    /// The rest of the source does not reach the value. The float shows a
    /// bounded number of rows and already reports that it hides content, so a
    /// text that no row can hold would occupy memory for nothing.
    #[must_use]
    pub fn is_clipped(&self) -> bool {
        self.clipped
    }
}

/// Returns the part of one source that the parse reads.
///
/// A source above [`MARKUP_SOURCE_BYTES_MAX`] ends at the last character
/// boundary below the bound, so the parse never splits a character.
fn bounded_source(source: &str) -> &str {
    if source.len() <= MARKUP_SOURCE_BYTES_MAX {
        return source;
    }

    let mut end = MARKUP_SOURCE_BYTES_MAX;
    while end > 0 && !source.is_char_boundary(end) {
        end -= 1;
    }

    &source[..end]
}

/// Returns the language name that one info string names.
///
/// CommonMark defines the first word of an info string as the language of the
/// fence, and it leaves the rest of the string to the writer. `rust,ignore` and
/// `rust title="one"` therefore both name Rust, so the reader stops at the
/// first blank and at the first comma. An info string that names nothing
/// answers the empty text, which no adapter declares.
///
/// The reader extracts the name here, because no code above the adapter
/// boundary may match a language name. See `docs/language-services.md`.
fn fence_language(info: &str) -> &str {
    info.trim_start()
        .split(|value: char| value.is_whitespace() || value == ',')
        .next()
        .unwrap_or_default()
}

/// Returns the rank of one heading, 1 through 6.
const fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

/// One open container of the walk: a block quote or a list.
#[derive(Clone, Debug, Eq, PartialEq)]
enum OpenContainer {
    /// One block quote.
    Quote,
    /// One list, and the state of the item that it opened.
    List {
        /// Whether one block inside this list has already opened.
        produced: bool,
        /// The number of the next item of an ordered list, or `None` for an
        /// unordered one.
        number: Option<u64>,
        /// The marker of the item that opened and whose first block has not
        /// arrived yet.
        marker: Option<MarkupMarker>,
    },
}

/// The content of one block that the walk opened.
#[derive(Clone, Debug, Eq, PartialEq)]
enum OpenBody {
    /// One paragraph or one list item.
    Prose(StyledMarkup),
    /// One heading and its rank.
    Heading { level: u8, text: StyledMarkup },
    /// One code block, which keeps its text until the fence closes or the text
    /// ends.
    Code { info: String, text: String },
}

/// One block that the walk opened and did not close.
#[derive(Clone, Debug, Eq, PartialEq)]
struct OpenBlock {
    /// Whether one blank row stands above the block.
    spaced: bool,
    /// The containers around the block, outermost first.
    containers: Vec<MarkupContainer>,
    /// The role of the text of the block, which an inline role overrides.
    base: MarkupRole,
    /// The content of the block.
    body: OpenBody,
}

/// The state of one walk over the events of one text.
#[derive(Debug, Default)]
struct Walk {
    /// The blocks that the walk closed.
    blocks: Vec<MarkupBlock>,
    /// The pieces that the walk produced.
    pieces: usize,
    /// The open block quotes and lists, outermost first.
    containers: Vec<OpenContainer>,
    /// The open inline roles, outermost first.
    inline: Vec<MarkupRole>,
    /// The block that the walk opened and did not close.
    open: Option<OpenBlock>,
}

impl Walk {
    /// Reports whether the walk reached one of its bounds.
    fn exhausted(&self) -> bool {
        self.blocks.len() >= MARKUP_BLOCKS_MAX || self.pieces >= MARKUP_PIECES_MAX
    }

    /// Applies one parser event.
    fn handle(&mut self, event: &Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => self.push_inline(text, None),
            Event::Code(text) => self.push_inline(text, Some(MarkupRole::InlineCode)),
            // kvim renders no markup. An HTML block and an inline tag both
            // arrive as the text that the server wrote, so nothing is dropped.
            Event::Html(text) | Event::InlineHtml(text) => self.push_inline(text, None),
            Event::SoftBreak => self.push_inline(" ", None),
            Event::HardBreak => self.hard_break(),
            Event::Rule => self.rule(),
            // The parse enables no extension, so a table, a footnote, a task
            // list marker, and a maths span never arrive as their own event.
            // Their source text arrives as text instead.
            _ => {}
        }
    }

    /// Opens one tag.
    fn start(&mut self, tag: &Tag<'_>) {
        match tag {
            Tag::Paragraph => {
                let base = self.base_role();
                self.open_block(base, OpenBody::Prose(StyledMarkup::default()));
            }
            Tag::Heading { level, .. } => self.open_block(
                MarkupRole::Heading,
                OpenBody::Heading {
                    level: heading_level(*level),
                    text: StyledMarkup::default(),
                },
            ),
            Tag::CodeBlock(kind) => {
                let info = match kind {
                    CodeBlockKind::Fenced(info) => info.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                self.open_block(
                    MarkupRole::Text,
                    OpenBody::Code {
                        info,
                        text: String::new(),
                    },
                );
            }
            Tag::BlockQuote(_) => self.containers.push(OpenContainer::Quote),
            Tag::List(first) => self.containers.push(OpenContainer::List {
                produced: false,
                number: *first,
                marker: None,
            }),
            Tag::Item => self.open_item(),
            Tag::Emphasis => self.inline.push(MarkupRole::Emphasis),
            Tag::Strong => self.inline.push(MarkupRole::Strong),
            Tag::Link { .. } => self.inline.push(MarkupRole::Link),
            // An image has no place on a terminal screen, so its alternative
            // text takes the plain text role.
            Tag::Image { .. } => self.inline.push(MarkupRole::Text),
            _ => {}
        }
    }

    /// Closes one tag.
    fn end(&mut self, tag: &TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) | TagEnd::CodeBlock => self.close(),
            TagEnd::BlockQuote(_) | TagEnd::List(_) => {
                self.close();
                self.containers.pop();
            }
            TagEnd::Item => {
                // A tight list item holds its text without a paragraph of its
                // own, so the item itself ends the block that holds it.
                self.close();
                if let Some(OpenContainer::List { marker, .. }) = self.containers.last_mut() {
                    // An item without a block never used its marker.
                    *marker = None;
                }
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Link | TagEnd::Image => {
                self.inline.pop();
            }
            _ => {}
        }
    }

    /// Returns the role of the text of a block outside a heading.
    fn base_role(&self) -> MarkupRole {
        if self
            .containers
            .iter()
            .any(|container| matches!(container, OpenContainer::Quote))
        {
            MarkupRole::Quote
        } else {
            MarkupRole::Text
        }
    }

    /// Records the marker of one list item, for the first block inside it.
    fn open_item(&mut self) {
        self.close();

        let Some(OpenContainer::List { number, marker, .. }) = self.containers.last_mut() else {
            return;
        };

        *marker = Some(match number {
            Some(value) => {
                let item = MarkupMarker::Ordered(*value);
                *number = Some(value.saturating_add(1));
                item
            }
            None => MarkupMarker::Bullet,
        });
    }

    /// Reports whether one blank row opens the block that starts now, and
    /// records that the open lists produced a block.
    ///
    /// The first block of a document opens none. Every later block opens one,
    /// except a block of a list that already produced one, so a list reads as
    /// one list rather than as one paragraph for each of its items.
    fn spaced(&mut self) -> bool {
        let mut spaced = !self.blocks.is_empty();
        for container in &mut self.containers {
            if let OpenContainer::List { produced, .. } = container {
                if *produced {
                    spaced = false;
                }
                *produced = true;
            }
        }

        spaced
    }

    /// Returns the containers of a block that opens now, outermost first.
    ///
    /// The innermost open item hands its marker to this block, so one marker
    /// stands on one block only. A container below
    /// [`MARKUP_NESTING_DEPTH_MAX`] adds no prefix, and its marker is still
    /// taken, so no later block of that list carries it.
    fn prefix(&mut self) -> Vec<MarkupContainer> {
        let mut prefix = Vec::new();

        for (depth, container) in self.containers.iter_mut().enumerate() {
            let taken = match container {
                OpenContainer::Quote => MarkupContainer::Quote,
                OpenContainer::List { marker, .. } => MarkupContainer::List {
                    marker: marker.take(),
                },
            };
            if depth < MARKUP_NESTING_DEPTH_MAX {
                prefix.push(taken);
            }
        }

        debug_assert!(
            prefix.len() <= MARKUP_NESTING_DEPTH_MAX,
            "the loop above keeps the outermost containers of the bound only"
        );

        prefix
    }

    /// Opens one block with the containers and the spacing of this position.
    fn open_block(&mut self, base: MarkupRole, body: OpenBody) {
        self.close();

        let spaced = self.spaced();
        let containers = self.prefix();
        self.open = Some(OpenBlock {
            spaced,
            containers,
            base,
            body,
        });
    }

    /// Appends text to the open block, opening a paragraph when none is open.
    ///
    /// `forced` is the role of a code span, which overrides the inline roles
    /// around it.
    fn push_inline(&mut self, text: &str, forced: Option<MarkupRole>) {
        if self.open.is_none() {
            let base = self.base_role();
            self.open_block(base, OpenBody::Prose(StyledMarkup::default()));
        }

        let inline = self.inline.last().copied();
        let Some(open) = self.open.as_mut() else {
            debug_assert!(false, "the branch above opens a block when none is open");
            return;
        };

        match &mut open.body {
            OpenBody::Prose(styled) | OpenBody::Heading { text: styled, .. } => {
                let role = forced.or(inline).unwrap_or(open.base);
                // A code span of two source lines carries the line feed that
                // joined them. A prose block holds one paragraph and wraps as
                // one, so the feed becomes the blank that it stood for.
                if text.contains('\n') {
                    styled.push(&text.replace('\n', " "), role);
                } else {
                    styled.push(text, role);
                }
            }
            OpenBody::Code { text: code, .. } => code.push_str(text),
        }

        self.pieces = self.pieces.saturating_add(1);
    }

    /// Ends the current prose block and opens its continuation.
    ///
    /// A hard break is a line break inside one paragraph. The block that
    /// follows carries the containers of the block before it without their
    /// markers, so the two keep one left edge.
    fn hard_break(&mut self) {
        let Some(open) = self.open.as_ref() else {
            self.push_inline(" ", None);
            return;
        };
        if matches!(open.body, OpenBody::Code { .. }) {
            self.push_inline("\n", None);
            return;
        }

        let base = open.base;
        let containers = open
            .containers
            .iter()
            .map(|container| container.continued())
            .collect();
        self.close();
        self.open = Some(OpenBlock {
            // A hard break continues one paragraph, so no blank row opens it.
            spaced: false,
            containers,
            base,
            body: OpenBody::Prose(StyledMarkup::default()),
        });
    }

    /// Appends one thematic break as its own block.
    fn rule(&mut self) {
        self.close();

        let spaced = self.spaced();
        let containers = self.prefix();
        self.blocks.push(MarkupBlock {
            spaced,
            containers,
            body: MarkupBody::Rule,
        });
        self.pieces = self.pieces.saturating_add(1);
    }

    /// Closes the open block and appends it.
    ///
    /// An empty paragraph appends nothing, because it would occupy a row that
    /// the answer does not hold. An empty code block still appends one row, so
    /// a fence that holds no line yet already reads as a code block.
    fn close(&mut self) {
        let Some(open) = self.open.take() else {
            return;
        };

        let body = match open.body {
            OpenBody::Prose(styled) => {
                if styled.is_empty() {
                    return;
                }
                MarkupBody::Prose(styled)
            }
            OpenBody::Heading { level, text } => {
                if text.is_empty() {
                    return;
                }
                debug_assert!(
                    (1..=6).contains(&level),
                    "CommonMark defines six heading ranks, and the parser names one of them"
                );
                MarkupBody::Heading { level, text }
            }
            OpenBody::Code { info, text } => {
                let body = text.strip_suffix('\n').unwrap_or(&text);
                let lines: Vec<String> = body.split('\n').map(str::to_owned).collect();

                debug_assert!(
                    !lines.is_empty(),
                    "a split on the line feed answers with at least one piece"
                );
                self.pieces = self.pieces.saturating_add(lines.len());
                MarkupBody::Code {
                    info,
                    lines,
                    highlights: Vec::new(),
                }
            }
        };

        self.blocks.push(MarkupBlock {
            spaced: open.spaced,
            containers: open.containers,
            body,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use kvim_core::TextBuffer;
    use kvim_settings::FileSettings;
    use tokio_util::sync::CancellationToken;

    use super::{
        MARKUP_BLOCKS_MAX, MARKUP_FENCE_SOURCE_BYTES_MAX, MARKUP_FENCE_SPANS_MAX,
        MARKUP_FENCES_MAX, MARKUP_NESTING_DEPTH_MAX, MARKUP_PIECES_MAX, MARKUP_SOURCE_BYTES_MAX,
        MarkupBlock, MarkupBody, MarkupContainer, MarkupDocument, MarkupMarker, MarkupRole,
        fence_language,
    };
    use crate::{AnalysisInput, HighlightSpan, LanguageRegistry};

    /// The answer that rust-analyzer sends for one function of kvim.
    ///
    /// The shape is the common one: one fence that names the module path, one
    /// fence that holds the signature, one thematic break, and the first line
    /// of the document comment.
    const RUST_ANALYZER_HOVER: &str = "\n```rust\nkvim_language::session\n```\n\n```rust\nfn \
                                       hover_markup(result: &RawValue) -> Result<Option<MarkupText>, \
                                       LspError>\n```\n\n---\n\nReturns the bounded text of one \
                                       hover answer and the markup that covers it.";

    /// Returns the text of one block, without its containers.
    fn content(block: &MarkupBlock) -> String {
        match block.body() {
            MarkupBody::Prose(styled) => styled.text().to_owned(),
            MarkupBody::Heading { text, .. } => text.text().to_owned(),
            MarkupBody::Code { lines, .. } => lines.join("\n"),
            MarkupBody::Rule => String::new(),
        }
    }

    /// Returns the pieces of one prose block, with their roles.
    fn pieces(document: &MarkupDocument, index: usize) -> Vec<(String, MarkupRole)> {
        match document.blocks()[index].body() {
            MarkupBody::Prose(styled) | MarkupBody::Heading { text: styled, .. } => styled
                .pieces()
                .into_iter()
                .map(|(text, role)| (text.to_owned(), role))
                .collect(),
            MarkupBody::Code { .. } => panic!("block {index} is a code block"),
            MarkupBody::Rule => panic!("block {index} is a thematic break"),
        }
    }

    /// Returns the document of one source with the code of each fence named.
    fn highlighted(source: &str) -> MarkupDocument {
        MarkupDocument::parse(source).highlighted(LanguageRegistry::first_release())
    }

    /// Returns the spans of the code block at one index.
    fn spans(document: &MarkupDocument, index: usize) -> &[HighlightSpan] {
        match document.blocks()[index].body() {
            MarkupBody::Code { highlights, .. } => highlights,
            other => panic!("block {index} is no code block: {other:?}"),
        }
    }

    /// Returns the spans that one text carries as the buffer of a Rust file.
    ///
    /// The path selects the adapter, so this helper takes the selection that
    /// an open file takes and never the one that a fence takes.
    fn buffer_spans(source: &str) -> Vec<HighlightSpan> {
        let version = TextBuffer::from_text("", &FileSettings::default())
            .expect("the empty text is small")
            .version();
        LanguageRegistry::first_release()
            .adapter(Path::new("hover.rs"))
            .expect("the Rust adapter owns a .rs path")
            .analyze(
                &AnalysisInput::new(version, Arc::from(source)),
                &CancellationToken::new(),
            )
            .expect("the test source stays inside every bound")
            .highlights()
            .to_vec()
    }

    #[test]
    fn a_rust_fence_carries_the_roles_of_the_same_buffer() {
        let path = "kvim_language::session";
        let signature =
            "fn hover_markup(result: &RawValue) -> Result<Option<MarkupText>, LspError>";
        let document = highlighted(RUST_ANALYZER_HOVER);

        assert!(
            !spans(&document, 1).is_empty(),
            "the signature of the answer carries roles"
        );
        assert_eq!(
            spans(&document, 1),
            buffer_spans(signature),
            "one text carries one set of roles in a fence and in a buffer"
        );
        assert_eq!(
            spans(&document, 0),
            buffer_spans(path),
            "the module path of the answer carries the roles of the same buffer text"
        );
    }

    #[test]
    fn a_fence_of_many_lines_addresses_the_lines_of_its_own_source() {
        let source = "fn main() {\n    let value = 1;\n}";
        let document = highlighted(&format!("```rust\n{source}\n```"));

        assert_eq!(
            spans(&document, 0),
            buffer_spans(source),
            "a span of a fence addresses the line and the byte as a span of a buffer does"
        );
        assert_eq!(
            spans(&document, 0)
                .iter()
                .map(|span| span.line)
                .max()
                .expect("the fence carries roles"),
            2,
            "the last line of the fence is the third one"
        );
    }

    #[test]
    fn a_compound_info_string_selects_the_language_of_its_first_word() {
        // A writer may add an attribute after the name, and rust-analyzer
        // sends `rust` alone. The match folds ASCII case.
        for info in ["rust", "rust,ignore", "rust title=\"one\"", "Rust", "RS"] {
            let document = highlighted(&format!("```{info}\nfn main() {{}}\n```"));

            assert_eq!(
                spans(&document, 0),
                buffer_spans("fn main() {}"),
                "the info string {info:?} names Rust"
            );
        }

        assert_eq!(fence_language("rust,ignore"), "rust");
        assert_eq!(fence_language("rust title=\"one\""), "rust");
        assert_eq!(fence_language(""), "");
    }

    #[test]
    fn a_fence_of_an_unknown_language_carries_no_role() {
        for info in ["console", "mermaid", "haskell"] {
            let document = highlighted(&format!("```{info}\nfn main() {{}}\n```"));

            assert!(
                spans(&document, 0).is_empty(),
                "no adapter answers to {info:?}, and that is no failure"
            );
            assert_eq!(
                content(&document.blocks()[0]),
                "fn main() {}",
                "the fence keeps every line of {info:?}"
            );
        }
    }

    #[test]
    fn a_hostile_info_string_selects_no_language() {
        // The info string is server text. The reader passes one complete name,
        // and every comparison rejects a length that no declared name holds.
        let info = "r".repeat(16 * 1024);
        let document = highlighted(&format!("```{info}\nfn main() {{}}\n```"));

        assert!(spans(&document, 0).is_empty());
    }

    #[test]
    fn a_fence_that_names_no_language_carries_no_role() {
        // The first source is a fence without an info string, and the second
        // one is an indented code block, which names no language at all.
        for source in ["```\nfn main() {}\n```", "    fn main() {}"] {
            let document = highlighted(source);

            assert!(
                spans(&document, 0).is_empty(),
                "{source:?} names no language"
            );
            assert_eq!(content(&document.blocks()[0]), "fn main() {}");
        }
    }

    #[test]
    fn a_fence_above_the_source_bound_carries_no_role() {
        let line = "fn main() {}\n";
        let lines = MARKUP_FENCE_SOURCE_BYTES_MAX / line.len();
        let inside = line.repeat(lines);
        let outside = line.repeat(lines + 1);
        assert!(inside.len() <= MARKUP_FENCE_SOURCE_BYTES_MAX);
        assert!(outside.len() > MARKUP_FENCE_SOURCE_BYTES_MAX);

        let document = highlighted(&format!("```rust\n{inside}```"));
        assert!(
            !spans(&document, 0).is_empty(),
            "a fence at the bound still carries roles"
        );

        let document = highlighted(&format!("```rust\n{outside}```"));
        assert!(
            spans(&document, 0).is_empty(),
            "a fence above the bound costs no highlight"
        );
        assert_eq!(
            document.blocks()[0].body(),
            &MarkupBody::Code {
                info: "rust".to_owned(),
                lines: outside.lines().map(str::to_owned).collect(),
                highlights: Vec::new(),
            },
            "the fence above the bound keeps every line"
        );
    }

    #[test]
    fn a_fence_above_the_span_bound_carries_no_role() {
        // Each keyword, each name, and each bracket is one span, so this
        // source produces about one span for each 1.5 bytes and reaches the
        // span bound below the source bound.
        let line = "fn a(){}\n";
        let source = line.repeat(MARKUP_FENCE_SOURCE_BYTES_MAX / line.len());
        assert!(source.len() <= MARKUP_FENCE_SOURCE_BYTES_MAX);
        assert!(
            buffer_spans(&source).len() > MARKUP_FENCE_SPANS_MAX,
            "the same buffer text passes the fence span bound"
        );

        let document = highlighted(&format!("```rust\n{source}```"));

        assert!(
            spans(&document, 0).is_empty(),
            "kvim publishes no partial result, so the fence carries no span at all"
        );
    }

    #[test]
    fn the_fence_bound_holds_over_a_document_of_many_fences() {
        let document = highlighted(&"```rust\nfn main() {}\n```\n\n".repeat(MARKUP_FENCES_MAX * 2));

        let named = document
            .blocks()
            .iter()
            .filter(|block| !matches!(block.body(), MarkupBody::Code { highlights, .. } if highlights.is_empty()))
            .count();
        assert_eq!(named, MARKUP_FENCES_MAX, "the pass reads no further fence");
        assert!(
            document
                .blocks()
                .iter()
                .all(|block| content(block) == "fn main() {}"),
            "a fence above the bound keeps every line"
        );
    }

    #[test]
    fn the_parse_alone_names_no_role() {
        // The terminal event loop may run the parse, and it must run no
        // Tree-sitter work, so only the highlight pass produces one span.
        let document = MarkupDocument::parse(RUST_ANALYZER_HOVER);

        let mut fences = 0;
        for (index, block) in document.blocks().iter().enumerate() {
            let MarkupBody::Code { highlights, .. } = block.body() else {
                continue;
            };
            fences += 1;
            assert!(
                highlights.is_empty(),
                "block {index} carries a span that no highlight pass produced"
            );
        }
        assert_eq!(
            fences, 2,
            "the answer holds the two fences that this test reads"
        );
    }

    #[test]
    fn a_rust_analyzer_hover_parses_into_its_blocks() {
        let document = MarkupDocument::parse(RUST_ANALYZER_HOVER);
        let blocks = document.blocks();

        assert_eq!(blocks.len(), 4, "{blocks:?}");
        assert!(!document.is_clipped());

        let MarkupBody::Code { info, lines, .. } = blocks[0].body() else {
            panic!("the module path stands in a fence: {:?}", blocks[0]);
        };
        assert_eq!(info, "rust", "the fence keeps its info string");
        assert_eq!(lines, &["kvim_language::session".to_owned()]);

        let MarkupBody::Code { info, lines, .. } = blocks[1].body() else {
            panic!("the signature stands in a fence: {:?}", blocks[1]);
        };
        assert_eq!(info, "rust", "the second fence keeps its info string");
        assert_eq!(
            lines,
            &[
                "fn hover_markup(result: &RawValue) -> Result<Option<MarkupText>, LspError>"
                    .to_owned()
            ]
        );

        assert_eq!(
            blocks[2].body(),
            &MarkupBody::Rule,
            "the thematic break is one block of its own"
        );

        assert_eq!(
            pieces(&document, 3),
            vec![(
                "Returns the bounded text of one hover answer and the markup that covers it."
                    .to_owned(),
                MarkupRole::Text,
            )],
            "the prose keeps every character of the document comment"
        );

        // Every block but the first opens with one blank row, because the
        // source separated them with a blank line.
        assert_eq!(
            blocks
                .iter()
                .map(MarkupBlock::is_spaced)
                .collect::<Vec<_>>(),
            vec![false, true, true, true]
        );
    }

    #[test]
    fn two_answers_join_into_one_document_and_one_blank_row() {
        // Two servers of one language answer on their own, and each answer
        // carries its own document, so the editor joins documents.
        let first = highlighted("```rust\nfn first() {}\n```");
        let second = highlighted("The second server *answers* as well.");
        let joined = first.clone().joined(&second);

        assert_eq!(
            joined
                .blocks()
                .iter()
                .map(|block| (content(block), block.is_spaced()))
                .collect::<Vec<_>>(),
            vec![
                ("fn first() {}".to_owned(), false),
                (
                    "The second server answers as well.".to_owned(),
                    // One blank row stands above the first block of the second
                    // answer, and the first block of the join opens none.
                    true,
                ),
            ]
        );
        assert_eq!(
            spans(&joined, 0),
            spans(&first, 0),
            "the join moves the spans of each fence unchanged"
        );
        assert!(!joined.is_clipped());
    }

    #[test]
    fn the_first_answer_of_one_join_opens_no_blank_row() {
        // The join starts at the empty document, so the first answer keeps the
        // spacing that its own parse gave it.
        let answer = MarkupDocument::parse("one\n\ntwo");
        let joined = MarkupDocument::default().joined(&answer);

        assert_eq!(joined, answer, "the empty document adds nothing");
    }

    #[test]
    fn one_clipped_answer_reports_the_join_as_clipped() {
        let clipped = MarkupDocument::parse(&"word ".repeat(MARKUP_SOURCE_BYTES_MAX));
        assert!(clipped.is_clipped());
        let complete = MarkupDocument::parse("a short answer");
        assert!(!complete.is_clipped());

        assert!(complete.clone().joined(&clipped).is_clipped());
        assert!(clipped.joined(&complete).is_clipped());
    }

    #[test]
    fn the_block_bound_holds_over_one_join() {
        // Neither answer reaches the bound alone, and the two reach it
        // together, so the join is the step that stops.
        let answer = MarkupDocument::parse(&"a\n\n".repeat(MARKUP_BLOCKS_MAX * 3 / 4));
        assert!(!answer.is_clipped(), "one answer stays under the bound");

        let joined = answer.clone().joined(&answer);

        assert_eq!(
            joined.blocks().len(),
            MARKUP_BLOCKS_MAX,
            "the join appends no block above the bound"
        );
        assert!(joined.is_clipped(), "the join stopped at the block bound");
    }

    #[test]
    fn the_piece_bound_holds_over_one_join() {
        // One line of a code block is one piece, and the lines of that block
        // join the count when the block closes, so this answer holds the
        // complete bound in one block.
        let answer =
            MarkupDocument::parse(&format!("```\n{}```", "line\n".repeat(MARKUP_PIECES_MAX)));
        assert!(!answer.is_clipped(), "the parse reached no bound");
        assert_eq!(
            answer
                .blocks()
                .iter()
                .map(MarkupBlock::pieces)
                .sum::<usize>(),
            MARKUP_PIECES_MAX
        );

        let joined = answer.joined(&MarkupDocument::parse("a second answer"));

        assert_eq!(
            joined.blocks().len(),
            1,
            "the join appends no block after the count reached the bound"
        );
        assert!(joined.is_clipped(), "the join stopped at the piece bound");
    }

    #[test]
    fn an_empty_source_holds_no_block() {
        for source in ["", "   ", "\n\n"] {
            let document = MarkupDocument::parse(source);

            assert!(document.is_empty(), "{source:?} holds no block");
            assert!(!document.is_clipped(), "{source:?} reached no bound");
        }
    }

    #[test]
    fn a_text_without_markup_stays_one_paragraph() {
        let message = "expected type usize, found type u32";
        let document = MarkupDocument::parse(message);

        assert_eq!(document.blocks().len(), 1);
        assert_eq!(
            pieces(&document, 0),
            vec![(message.to_owned(), MarkupRole::Text)],
            "a text that holds no markup keeps every character in one role"
        );
    }

    #[test]
    fn a_source_above_the_bound_reports_the_bound() {
        let source = "word ".repeat(MARKUP_SOURCE_BYTES_MAX);
        let document = MarkupDocument::parse(&source);

        assert!(document.is_clipped(), "the source stands above the bound");
        assert!(document.blocks().len() <= MARKUP_BLOCKS_MAX + 1);

        let kept: usize = document
            .blocks()
            .iter()
            .map(|block| content(block).len())
            .sum();
        assert!(
            kept <= MARKUP_SOURCE_BYTES_MAX,
            "the value never grows past the source bound: {kept}"
        );
    }

    #[test]
    fn a_source_above_the_bound_splits_no_character() {
        // Each character occupies four bytes, so the bound falls inside one of
        // them and the parse must step back to its start.
        let source = "𝄞".repeat(MARKUP_SOURCE_BYTES_MAX);
        let document = MarkupDocument::parse(&source);

        assert!(document.is_clipped());
        assert!(
            content(&document.blocks()[0])
                .chars()
                .all(|value| value == '𝄞'),
            "the parse cut the source at a character boundary"
        );
    }

    #[test]
    fn the_block_bound_holds_over_a_source_of_many_blocks() {
        let source = "a\n\n".repeat(MARKUP_BLOCKS_MAX * 2);
        let document = MarkupDocument::parse(&source);

        assert!(document.blocks().len() <= MARKUP_BLOCKS_MAX + 1);
        assert!(document.is_clipped(), "the parse stopped at the bound");
    }

    #[test]
    fn the_piece_bound_holds_over_one_text_of_many_pieces() {
        // Each emphasis and each blank between two of them is one piece. The
        // source stays far below the source bound, so the piece bound is the
        // one that stops this parse.
        let source = "*a* ".repeat(MARKUP_PIECES_MAX);
        assert!(source.len() < MARKUP_SOURCE_BYTES_MAX);

        let document = MarkupDocument::parse(&source);

        assert!(
            document.is_clipped(),
            "the parse stopped at the piece bound"
        );
        let pieces: usize = document
            .blocks()
            .iter()
            .map(|block| match block.body() {
                MarkupBody::Prose(styled) | MarkupBody::Heading { text: styled, .. } => {
                    styled.pieces().len()
                }
                MarkupBody::Code { lines, .. } => lines.len(),
                MarkupBody::Rule => 1,
            })
            .sum();
        assert!(
            pieces <= MARKUP_PIECES_MAX,
            "the value holds no more pieces than the bound: {pieces}"
        );
    }

    #[test]
    fn each_inline_marker_takes_its_own_role() {
        let document =
            MarkupDocument::parse("plain *soft* **hard** `code` and [the manual](https://kvim)");

        assert_eq!(
            pieces(&document, 0),
            vec![
                ("plain ".to_owned(), MarkupRole::Text),
                ("soft".to_owned(), MarkupRole::Emphasis),
                (" ".to_owned(), MarkupRole::Text),
                ("hard".to_owned(), MarkupRole::Strong),
                (" ".to_owned(), MarkupRole::Text),
                ("code".to_owned(), MarkupRole::InlineCode),
                (" and ".to_owned(), MarkupRole::Text),
                ("the manual".to_owned(), MarkupRole::Link),
            ],
            "no marker of the source reaches the text, and the destination never paints"
        );
    }

    #[test]
    fn a_heading_keeps_its_rank_and_drops_its_marker() {
        let document = MarkupDocument::parse("### The report");

        let MarkupBody::Heading { level, text } = document.blocks()[0].body() else {
            panic!("the hashes open a heading");
        };
        assert_eq!(*level, 3);
        assert_eq!(text.text(), "The report");
        assert_eq!(text.pieces()[0].1, MarkupRole::Heading);
    }

    #[test]
    fn a_list_names_the_marker_of_each_item_once() {
        let document = MarkupDocument::parse("1. one\n2. two\n   - inner\n3. three");
        let blocks = document.blocks();

        assert_eq!(
            blocks
                .iter()
                .map(|block| (block.containers().to_owned(), content(block)))
                .collect::<Vec<_>>(),
            vec![
                (
                    vec![MarkupContainer::List {
                        marker: Some(MarkupMarker::Ordered(1))
                    }],
                    "one".to_owned()
                ),
                (
                    vec![MarkupContainer::List {
                        marker: Some(MarkupMarker::Ordered(2))
                    }],
                    "two".to_owned()
                ),
                (
                    vec![
                        MarkupContainer::List { marker: None },
                        MarkupContainer::List {
                            marker: Some(MarkupMarker::Bullet)
                        },
                    ],
                    "inner".to_owned()
                ),
                (
                    vec![MarkupContainer::List {
                        marker: Some(MarkupMarker::Ordered(3))
                    }],
                    "three".to_owned()
                ),
            ],
            "the nested item indents under the item that holds it"
        );

        // The items of one list follow one another without a blank row.
        assert!(blocks.iter().all(|block| !block.is_spaced()));
    }

    #[test]
    fn a_hard_break_continues_one_item_without_its_marker() {
        // Two blanks at the end of a line are one hard break of CommonMark.
        let document = MarkupDocument::parse("- first line  \n  second line");
        let blocks = document.blocks();

        assert_eq!(blocks.len(), 2, "{blocks:?}");
        assert_eq!(
            blocks[0].containers(),
            &[MarkupContainer::List {
                marker: Some(MarkupMarker::Bullet)
            }]
        );
        assert_eq!(content(&blocks[0]), "first line");
        assert_eq!(
            blocks[1].containers(),
            &[MarkupContainer::List { marker: None }],
            "one marker stands on one block only"
        );
        assert_eq!(content(&blocks[1]), "second line");
        assert!(!blocks[1].is_spaced(), "a hard break continues one item");
    }

    #[test]
    fn a_block_quote_rails_its_block_and_names_its_text() {
        let document = MarkupDocument::parse("> a remark");

        assert_eq!(
            document.blocks()[0].containers(),
            &[MarkupContainer::Quote],
            "the quote rails every row of the block"
        );
        assert_eq!(
            pieces(&document, 0),
            vec![("a remark".to_owned(), MarkupRole::Quote)]
        );
    }

    #[test]
    fn the_container_bound_holds_over_a_deep_nesting() {
        let source = format!("{}deep", "> ".repeat(MARKUP_NESTING_DEPTH_MAX * 2));
        let document = MarkupDocument::parse(&source);

        assert_eq!(
            document.blocks()[0].containers().len(),
            MARKUP_NESTING_DEPTH_MAX,
            "a container below the bound adds no prefix"
        );
        assert_eq!(content(&document.blocks()[0]), "deep");
    }

    #[test]
    fn an_open_fence_already_reads_as_a_code_block() {
        // A server writes a fence and no closing fence when its answer ends
        // with one. CommonMark closes the fence at the end of the text.
        for source in ["```rust", "```rust\n", "```rust\nfn main() {"] {
            let document = MarkupDocument::parse(source);

            assert!(
                matches!(document.blocks()[0].body(), MarkupBody::Code { .. }),
                "{source:?} must already be a code block"
            );
        }
    }

    #[test]
    fn a_table_arrives_as_the_text_that_the_server_wrote() {
        // The parse enables no extension, so no cell of the table disappears.
        let document = MarkupDocument::parse("| Tool | Use |\n| --- | --- |\n| read | a file |");

        let text: String = document
            .blocks()
            .iter()
            .map(content)
            .collect::<Vec<_>>()
            .join(" ");
        for cell in ["Tool", "Use", "read", "a file"] {
            assert!(text.contains(cell), "the text keeps {cell:?}: {text:?}");
        }
    }
}
