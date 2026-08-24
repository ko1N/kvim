//! Buffer identity and the loaded buffer list.
//!
//! A buffer keeps its identity while it stays loaded. Windows, registers, and
//! later language sessions refer to a buffer by identity, never by path. See
//! `docs/files.md`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use kvim_core::{BufferVersion, TextBuffer};
use kvim_path::WorktreeRoot;
use kvim_settings::FileSettings;

use super::file::{FileIdentity, FileTarget};
use super::mutation::{BufferPathUpdate, OpenBuffer};

/// The largest number of buffers that one editor keeps loaded.
///
/// The bound protects the editor against an unbounded buffer list. One daily
/// Rust session opens far fewer files than this value.
pub const BUFFERS_MAX: usize = 128;

/// The name of a buffer that holds no file.
pub const SCRATCH_BUFFER_NAME: &str = "[Scratch]";

/// The stable identity of one loaded buffer.
///
/// The identity never changes while the buffer stays loaded. A rename changes
/// the path of the buffer and keeps the identity.
///
/// # Examples
///
/// ```
/// use kvim_workspace::BufferId;
///
/// assert_ne!(BufferId::new(1), BufferId::new(2));
/// assert_eq!(BufferId::new(7).get(), 7);
/// ```
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BufferId(u32);

impl BufferId {
    /// Creates a buffer identity from its value.
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the identity value.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// What another program did to the file of one buffer.
///
/// The marker exists only while kvim cannot make the buffer current again: the
/// buffer holds unsaved changes that no reload may replace, or the file is
/// gone. The buffer stays fully editable in both states. See `docs/files.md`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternalChange {
    /// The file changed while the buffer held unsaved changes.
    Changed,
    /// The file no longer lies at the path of the buffer.
    Missing,
}

/// Whether a successful file write still describes the live buffer text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveApplyOutcome {
    /// The live buffer still holds the saved version and is now clean.
    Current,
    /// The live buffer has newer edits and remains dirty.
    Stale,
}

/// Whether the text history's saved position still describes the file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryBaseline {
    /// The text history and file agree about the saved position.
    Current,
    /// A stale save wrote another text version, so no history position is clean.
    Invalidated,
}

/// One loaded buffer with its path, its file identity, and its text.
///
/// The file identity is the state that kvim observed at load time or after the
/// last successful save. The save path compares it with the current file before
/// it replaces that file, and the reload path compares it before it replaces
/// the buffer text.
#[derive(Debug)]
pub struct FileBuffer {
    text: TextBuffer,
    path: Option<PathBuf>,
    target: Option<FileTarget>,
    name: String,
    identity: Option<FileIdentity>,
    history_baseline: HistoryBaseline,
    /// What another program did to the file that kvim could not follow.
    external: Option<ExternalChange>,
}

impl FileBuffer {
    /// Creates an empty buffer that holds no file.
    #[must_use]
    pub fn scratch(files: &FileSettings) -> Self {
        let text = TextBuffer::from_text("", files)
            .expect("an empty text never passes the file size limit");
        Self::generated(SCRATCH_BUFFER_NAME, text)
    }

    /// Creates a buffer over generated text that holds no file.
    ///
    /// The `name` names the buffer in the winbar, exactly as
    /// [`FileBuffer::scratch`] names its own. The buffer holds no path, so it
    /// stays an ordinary scratch buffer: the user edits it and closes it, and a
    /// save asks for a file name first.
    ///
    /// # Examples
    ///
    /// ```
    /// use kvim_core::TextBuffer;
    /// use kvim_settings::FileSettings;
    /// use kvim_workspace::FileBuffer;
    ///
    /// let files = FileSettings::default();
    /// let text = TextBuffer::from_text("one\n", &files).expect("the text is short");
    /// let buffer = FileBuffer::generated("[Logs]", text);
    /// assert_eq!(buffer.name(), "[Logs]");
    /// assert_eq!(buffer.path(), None);
    /// assert!(!buffer.is_modified());
    /// ```
    #[must_use]
    pub fn generated(name: impl Into<String>, text: TextBuffer) -> Self {
        Self {
            text,
            path: None,
            target: None,
            name: name.into(),
            identity: None,
            history_baseline: HistoryBaseline::Current,
            external: None,
        }
    }

    /// Creates a buffer over one loaded file.
    ///
    /// The identity is `None` while the path holds no file yet, so the first
    /// save writes a new file.
    #[must_use]
    pub fn loaded(text: TextBuffer, target: FileTarget, identity: Option<FileIdentity>) -> Self {
        let path = target.as_path().to_path_buf();
        let name = display_name(&path);
        Self {
            text,
            path: Some(path),
            target: Some(target),
            name,
            identity,
            history_baseline: HistoryBaseline::Current,
            external: None,
        }
    }

    /// Returns the text of the buffer.
    #[must_use]
    pub const fn text(&self) -> &TextBuffer {
        &self.text
    }

    /// Returns the text of the buffer for one edit transaction.
    pub fn text_mut(&mut self) -> &mut TextBuffer {
        &mut self.text
    }

    /// Returns the file path, or `None` for a scratch buffer.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Returns the validated file target, or `None` for a scratch buffer.
    #[must_use]
    pub const fn target(&self) -> Option<&FileTarget> {
        self.target.as_ref()
    }

    /// Returns the short name that the winbar and the messages show.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the file identity of the last load or save.
    #[must_use]
    pub const fn identity(&self) -> Option<FileIdentity> {
        self.identity
    }

    /// Reports whether the buffer differs from the last saved state.
    #[must_use]
    pub fn is_modified(&self) -> bool {
        self.history_baseline == HistoryBaseline::Invalidated || self.text.is_modified()
    }

    /// Returns what another program did to the file that kvim could not follow.
    ///
    /// The value is `None` while no unresolved external change is recorded.
    /// A successful reload or save clears the marker.
    #[must_use]
    pub const fn external_change(&self) -> Option<ExternalChange> {
        self.external
    }

    /// Records what another program did to the file of this buffer.
    ///
    /// The buffer keeps its text and stays editable, because that text is the
    /// only copy that kvim can still write.
    pub const fn mark_external_change(&mut self, change: ExternalChange) {
        self.external = Some(change);
    }

    /// Gives the buffer the path that a workspace mutation created.
    ///
    /// The identity of the buffer never changes, and the file identity stays
    /// valid, because a rename or a move keeps the content of the file.
    pub fn set_path(&mut self, path: PathBuf) {
        self.name = display_name(&path);
        self.target = self
            .target
            .as_ref()
            .and_then(|target| target.retarget(&path));
        self.path = Some(path);
    }

    /// Applies the file state from one successful save.
    ///
    /// The target and file identity always advance to the written file. The
    /// dirty state clears only while the live text still has `saved_version`.
    pub fn apply_save(
        &mut self,
        target: FileTarget,
        identity: FileIdentity,
        saved_version: BufferVersion,
    ) -> SaveApplyOutcome {
        self.name = display_name(target.as_path());
        self.path = Some(target.as_path().to_path_buf());
        self.target = Some(target);
        self.identity = Some(identity);
        self.external = None;
        if self.text.version() != saved_version {
            self.history_baseline = HistoryBaseline::Invalidated;
            return SaveApplyOutcome::Stale;
        }
        self.history_baseline = HistoryBaseline::Current;
        self.text.mark_saved();
        SaveApplyOutcome::Current
    }

    /// Replaces the buffer with the text that its file holds now.
    ///
    /// The buffer identity, the path, and the name stay, because the reload
    /// reads the same file. The text of the file becomes the saved state, so
    /// the reloaded buffer holds no unsaved change and no external change. The
    /// caller reloads a buffer with unsaved changes only after the user asked
    /// to discard them. See `docs/files.md`.
    pub fn reload(&mut self, text: TextBuffer, identity: Option<FileIdentity>) {
        self.text = text;
        self.identity = identity;
        self.history_baseline = HistoryBaseline::Current;
        self.external = None;
    }
}

/// Returns the short name of one path.
fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// The loaded buffers of one editor, keyed by identity.
///
/// The list always holds at least one buffer, so the editor always shows text.
///
/// # Examples
///
/// ```
/// use kvim_settings::FileSettings;
/// use kvim_workspace::{Buffers, FileBuffer};
///
/// let files = FileSettings::default();
/// let (mut buffers, scratch) = Buffers::new(FileBuffer::scratch(&files));
/// assert_eq!(buffers.len(), 1);
/// assert_eq!(buffers.get(scratch).map(FileBuffer::name), Some("[Scratch]"));
/// ```
#[derive(Debug)]
pub struct Buffers {
    entries: BTreeMap<BufferId, FileBuffer>,
    next: u32,
}

impl Buffers {
    /// Creates a list that holds one buffer.
    #[must_use]
    pub fn new(first: FileBuffer) -> (Self, BufferId) {
        let id = BufferId(1);
        let mut entries = BTreeMap::new();
        entries.insert(id, first);
        (Self { entries, next: 2 }, id)
    }

    /// Adds one buffer and returns its new identity.
    ///
    /// Returns `None` when the list already holds [`BUFFERS_MAX`] buffers.
    pub fn insert(&mut self, buffer: FileBuffer) -> Option<BufferId> {
        if self.entries.len() >= BUFFERS_MAX {
            return None;
        }
        let id = BufferId(self.next);
        self.next = self
            .next
            .checked_add(1)
            .expect("a u32 counts more buffers than one session opens");
        self.entries.insert(id, buffer);
        Some(id)
    }

    /// Removes one buffer and returns it.
    pub fn remove(&mut self, id: BufferId) -> Option<FileBuffer> {
        self.entries.remove(&id)
    }

    /// Returns the named buffer.
    #[must_use]
    pub fn get(&self, id: BufferId) -> Option<&FileBuffer> {
        self.entries.get(&id)
    }

    /// Returns the named buffer for one change.
    pub fn get_mut(&mut self, id: BufferId) -> Option<&mut FileBuffer> {
        self.entries.get_mut(&id)
    }

    /// Returns the buffer that already owns one path.
    #[must_use]
    pub fn find_target(&self, target: &FileTarget) -> Option<BufferId> {
        self.entries
            .iter()
            .find(|(_, buffer)| buffer.target() == Some(target))
            .map(|(id, _)| *id)
    }

    /// Returns the buffer whose canonical display path matches `path`.
    #[must_use]
    pub fn find_path(&self, path: &Path) -> Option<BufferId> {
        self.entries
            .iter()
            .find(|(_, buffer)| buffer.path() == Some(path))
            .map(|(id, _)| *id)
    }

    /// Retargets every buffer that one workspace mutation moved.
    ///
    /// The call applies the complete list of one mutation, so the paths of the
    /// buffers and the workspace change together.
    pub fn apply_path_updates(&mut self, updates: &[BufferPathUpdate]) {
        for update in updates {
            if let Some(buffer) = self.entries.get_mut(&update.buffer) {
                buffer.set_path(update.path.clone());
            }
        }
    }

    /// Returns the mutation view of every loaded buffer of one root.
    ///
    /// The mutation request holds this list, so the worker validates against
    /// the buffers without reading editor state. A scratch buffer and a buffer
    /// of another worktree root name no contained path of this root, so neither
    /// can block or follow a mutation of it.
    #[must_use]
    pub fn open_buffers(&self, root: &WorktreeRoot) -> Vec<OpenBuffer> {
        self.entries
            .iter()
            .filter_map(|(id, buffer)| {
                let target = buffer.target().filter(|target| target.root() == root)?;
                Some(OpenBuffer {
                    id: *id,
                    path: target.relative_path().clone(),
                    is_modified: buffer.is_modified(),
                })
            })
            .collect()
    }

    /// Returns every identity in ascending order.
    #[must_use]
    pub fn ids(&self) -> Vec<BufferId> {
        self.entries.keys().copied().collect()
    }

    /// Returns the number of loaded buffers.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the list holds no buffer.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
