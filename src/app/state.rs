use anyhow::Result;
use std::collections::HashSet;
use time::{format_description::well_known::Rfc3339, Duration, OffsetDateTime};
use unicode_segmentation::UnicodeSegmentation;

use crate::journaling::{AutoSaveStatus, RecoverySnapshot};
use crate::search::{parse_query, regex_pattern_from_input, RangeFilter, SearchQuery};
use crate::storage::{NoteRecord, StorageHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    List,
    Reader,
}

#[derive(Debug, Clone)]
pub struct NoteSummary {
    pub id: i64,
    pub title: String,
    pub updated_at: String,
    pub preview: String,
    pub body: String,
    pub pinned: bool,
    pub archived: bool,
    pub tags: Vec<String>,
    pub deleted_at: Option<i64>,
    pub deleted_label: Option<String>,
    pub trash_status: Option<TrashStatus>,
}

#[derive(Debug, Clone)]
pub struct TrashStatus {
    pub label: String,
    pub expired: bool,
    pub indefinite: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SearchState {
    pub active: bool,
    pub query: String,
    pub last_error: Option<String>,
    pub terms: Vec<String>,
    pub tags: Vec<String>,
    pub filter_chips: Vec<String>,
    pub regex_enabled: bool,
    pub regex_pattern: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewNoteOverlay {
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct RenameNoteOverlay {
    pub note_id: i64,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct DeleteNoteOverlay {
    pub note_id: i64,
    pub title: String,
}

#[derive(Debug, Clone)]
pub struct TagEditorItem {
    pub name: String,
    pub selected: bool,
    pub original: bool,
    pub bulk_selected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagEditorMode {
    Browse,
    Input(TagInputKind),
    ConfirmDelete { tag: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagInputKind {
    Add,
    Rename { original: String },
    Merge { sources: Vec<String> },
}

impl Default for TagEditorMode {
    fn default() -> Self {
        TagEditorMode::Browse
    }
}

#[derive(Debug, Clone, Default)]
pub struct TagEditorOverlay {
    pub note_id: i64,
    pub items: Vec<TagEditorItem>,
    pub selected_index: usize,
    pub mode: TagEditorMode,
    pub input: String,
    pub status: Option<String>,
    pub suggestions: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkTrashAction {
    RestoreAll,
    PurgeAll,
}

#[derive(Debug, Clone)]
pub struct BulkTrashOverlay {
    pub action: BulkTrashAction,
}

#[derive(Debug, Clone)]
pub struct RecoveryEntry {
    pub note_id: i64,
    pub title: String,
    pub saved_at: String,
    pub saved_relative: String,
    pub body: String,
    pub preview: Vec<String>,
    pub missing: bool,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryOverlay {
    pub entries: Vec<RecoveryEntry>,
    pub selected: usize,
}

#[derive(Debug, Clone)]
pub enum OverlayState {
    NewNote(NewNoteOverlay),
    RenameNote(RenameNoteOverlay),
    DeleteNote(DeleteNoteOverlay),
    TagEditor(TagEditorOverlay),
    BulkTrash(BulkTrashOverlay),
    Recovery(RecoveryOverlay),
}

#[derive(Debug, Clone)]
pub struct EditorState {
    pub note_id: i64,
    pub buffer: String,
    pub cursor: usize,
    pub dirty: bool,
    preferred_column: Option<usize>,
    history: Vec<String>,
    history_index: usize,
}

impl EditorState {
    fn new(note_id: i64, buffer: String) -> Self {
        let cursor = buffer.len();
        let mut history = Vec::with_capacity(128);
        history.push(buffer.clone());
        Self {
            note_id,
            buffer,
            cursor,
            dirty: false,
            preferred_column: None,
            history,
            history_index: 0,
        }
    }

    pub fn note_id(&self) -> i64 {
        self.note_id
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    pub fn buffer(&self) -> &str {
        &self.buffer
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn mark_clean(&mut self) {
        self.dirty = false;
        self.history.clear();
        self.history.push(self.buffer.clone());
        self.history_index = 0;
    }

    pub fn insert_char(&mut self, ch: char) -> bool {
        let mut scratch = [0u8; 4];
        let encoded = ch.encode_utf8(&mut scratch);
        self.buffer.insert_str(self.cursor, encoded);
        self.cursor += encoded.len();
        self.preferred_column = None;
        self.after_edit();
        true
    }

    pub fn insert_newline(&mut self) -> bool {
        self.buffer.insert(self.cursor, '\n');
        self.cursor += 1;
        self.preferred_column = Some(0);
        self.after_edit();
        true
    }

    pub fn backspace(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let prev = prev_grapheme_boundary(&self.buffer, self.cursor);
        self.buffer.drain(prev..self.cursor);
        self.cursor = prev;
        self.preferred_column = None;
        self.after_edit();
        true
    }

    pub fn delete(&mut self) -> bool {
        if self.cursor >= self.buffer.len() {
            return false;
        }
        let next = next_grapheme_boundary(&self.buffer, self.cursor);
        if next == self.cursor {
            return false;
        }
        self.buffer.drain(self.cursor..next);
        self.preferred_column = None;
        self.after_edit();
        true
    }

    pub fn move_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let prev = prev_grapheme_boundary(&self.buffer, self.cursor);
        self.cursor = prev;
        self.preferred_column = None;
        true
    }

    pub fn move_right(&mut self) -> bool {
        if self.cursor >= self.buffer.len() {
            return false;
        }
        let next = next_grapheme_boundary(&self.buffer, self.cursor);
        if next == self.cursor {
            return false;
        }
        self.cursor = next;
        self.preferred_column = None;
        true
    }

    pub fn move_home(&mut self) -> bool {
        let line_start = line_start(&self.buffer, self.cursor);
        if self.cursor == line_start {
            return false;
        }
        self.cursor = line_start;
        self.preferred_column = Some(0);
        true
    }

    pub fn move_end(&mut self) -> bool {
        let line_end = line_end(&self.buffer, self.cursor);
        if self.cursor == line_end {
            return false;
        }
        self.cursor = line_end;
        self.preferred_column = Some(column_at(
            &self.buffer,
            line_start(&self.buffer, self.cursor),
            self.cursor,
        ));
        true
    }

    pub fn move_up(&mut self) -> bool {
        let current_line_start = line_start(&self.buffer, self.cursor);
        let current_column = self
            .preferred_column
            .unwrap_or_else(|| column_at(&self.buffer, current_line_start, self.cursor));
        if current_line_start == 0 {
            if self.cursor == 0 {
                return false;
            }
            self.cursor = 0;
            self.preferred_column = Some(current_column);
            return true;
        }
        let prev_line_end = current_line_start.saturating_sub(1);
        let prev_line_start = line_start(&self.buffer, prev_line_end);
        let target = position_for_column(&self.buffer, prev_line_start, current_column);
        if self.cursor == target {
            return false;
        }
        self.cursor = target;
        self.preferred_column = Some(current_column);
        true
    }

    pub fn move_down(&mut self) -> bool {
        let current_line_start = line_start(&self.buffer, self.cursor);
        let current_column = self
            .preferred_column
            .unwrap_or_else(|| column_at(&self.buffer, current_line_start, self.cursor));
        let current_line_end = line_end(&self.buffer, self.cursor);
        if current_line_end == self.buffer.len() {
            if self.cursor == self.buffer.len() {
                return false;
            }
            self.cursor = self.buffer.len();
            self.preferred_column = Some(current_column);
            return true;
        }
        let next_line_start = current_line_end + 1;
        let target = position_for_column(&self.buffer, next_line_start, current_column);
        if self.cursor == target {
            return false;
        }
        self.cursor = target;
        self.preferred_column = Some(current_column);
        true
    }

    pub fn move_word_left(&mut self) -> bool {
        if self.cursor == 0 {
            return false;
        }
        let mut idx = self.cursor;
        while idx > 0 {
            let prev = prev_grapheme_boundary(&self.buffer, idx);
            if self.buffer[prev..idx].trim().is_empty() {
                idx = prev;
            } else {
                break;
            }
        }
        while idx > 0 {
            let prev = prev_grapheme_boundary(&self.buffer, idx);
            if self.buffer[prev..idx].trim().is_empty() {
                break;
            }
            idx = prev;
        }
        self.cursor = idx;
        self.preferred_column = None;
        true
    }

    pub fn move_word_right(&mut self) -> bool {
        if self.cursor >= self.buffer.len() {
            return false;
        }
        let mut idx = self.cursor;
        let len = self.buffer.len();

        while idx < len {
            let next = next_grapheme_boundary(&self.buffer, idx);
            if self.buffer[idx..next].trim().is_empty() {
                idx = next;
            } else {
                break;
            }
        }

        while idx < len {
            let next = next_grapheme_boundary(&self.buffer, idx);
            if self.buffer[idx..next].trim().is_empty() {
                break;
            }
            idx = next;
        }

        while idx < len {
            let next = next_grapheme_boundary(&self.buffer, idx);
            if self.buffer[idx..next].trim().is_empty() {
                idx = next;
            } else {
                break;
            }
        }

        if idx == self.cursor {
            return false;
        }
        self.cursor = idx.min(len);
        self.preferred_column = None;
        true
    }

    pub fn undo(&mut self) -> bool {
        if self.history_index == 0 {
            return false;
        }
        self.history_index -= 1;
        self.restore_history_snapshot();
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.history_index + 1 >= self.history.len() {
            return false;
        }
        self.history_index += 1;
        self.restore_history_snapshot();
        true
    }

    fn after_edit(&mut self) {
        self.dirty = true;
        self.record_history();
    }

    fn record_history(&mut self) {
        const MAX_HISTORY: usize = 200;
        if let Some(current) = self.history.get(self.history_index) {
            if current.as_str() == self.buffer {
                return;
            }
        }
        self.history.truncate(self.history_index + 1);
        self.history.push(self.buffer.clone());
        if self.history.len() > MAX_HISTORY {
            let overflow = self.history.len() - MAX_HISTORY;
            self.history.drain(0..overflow);
            self.history_index = self.history.len().saturating_sub(1);
        } else {
            self.history_index = self.history.len() - 1;
        }
    }

    fn restore_history_snapshot(&mut self) {
        if let Some(snapshot) = self.history.get(self.history_index).cloned() {
            self.buffer = snapshot;
            if self.cursor > self.buffer.len() {
                self.cursor = self.buffer.len();
            }
            self.dirty = self.history_index != 0;
            self.preferred_column = None;
        }
    }
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub focus: FocusPane,
    pub show_trash: bool,
    pub selected: usize,
    pub preview_lines: usize,
    pub retention_days: u32,
    pub notes: Vec<NoteSummary>,
    pub search: SearchState,
    pub status_message: Option<String>,
    pub overlay: Option<OverlayState>,
    pub editor: Option<EditorState>,
    pub autosave_status: AutoSaveStatus,
    pub wrap_enabled: bool,
}

impl AppState {
    pub fn load(
        storage: &StorageHandle,
        preview_lines: usize,
        retention_days: u32,
    ) -> Result<Self> {
        let records = storage.fetch_recent_notes(50)?;
        let notes = records
            .into_iter()
            .map(|record| summarize_record(record, preview_lines, retention_days))
            .collect::<Vec<_>>();

        Ok(Self {
            focus: FocusPane::List,
            show_trash: false,
            selected: 0,
            preview_lines,
            retention_days,
            notes,
            search: SearchState::default(),
            status_message: None,
            overlay: None,
            editor: None,
            autosave_status: AutoSaveStatus::Inactive,
            wrap_enabled: true,
        })
    }

    pub fn len(&self) -> usize {
        self.notes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.notes.is_empty()
    }

    pub fn selected(&self) -> Option<&NoteSummary> {
        self.notes.get(self.selected)
    }

    pub fn selected_mut(&mut self) -> Option<&mut NoteSummary> {
        self.notes.get_mut(self.selected)
    }

    pub fn selected_note_id(&self) -> Option<i64> {
        self.selected().map(|note| note.id)
    }

    pub fn editor(&self) -> Option<&EditorState> {
        self.editor.as_ref()
    }

    pub fn editor_mut(&mut self) -> Option<&mut EditorState> {
        self.editor.as_mut()
    }

    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    pub fn begin_editor(&mut self, note_id: i64, body: String) {
        self.editor = Some(EditorState::new(note_id, body.clone()));
        self.update_note_buffer(note_id, &body);
    }

    pub fn close_editor(&mut self) {
        self.editor = None;
    }

    pub fn editor_buffer(&self) -> Option<&str> {
        self.editor.as_ref().map(|editor| editor.buffer())
    }

    pub fn editor_dirty(&self) -> bool {
        self.editor
            .as_ref()
            .map(|editor| editor.is_dirty())
            .unwrap_or(false)
    }

    pub fn mark_editor_saved(&mut self) {
        if let Some(editor) = self.editor.as_mut() {
            editor.mark_clean();
        }
    }

    pub fn apply_editor_preview(&mut self) {
        let Some(editor) = self.editor.as_ref() else {
            return;
        };
        let note_id = editor.note_id;
        let buffer = editor.buffer.clone();
        self.update_note_buffer(note_id, &buffer);
    }

    pub fn autosave_status(&self) -> &AutoSaveStatus {
        &self.autosave_status
    }

    pub fn set_autosave_status(&mut self, status: AutoSaveStatus) {
        self.autosave_status = status;
    }

    pub fn wrap_enabled(&self) -> bool {
        self.wrap_enabled
    }

    pub fn toggle_wrap(&mut self) -> bool {
        self.wrap_enabled = !self.wrap_enabled;
        self.wrap_enabled
    }

    pub fn on_autosave_saved(&mut self, note_id: i64, timestamp: OffsetDateTime) {
        if let Some(editor) = self.editor.as_mut() {
            if editor.note_id == note_id {
                editor.mark_clean();
            }
        }
        if let Some(note) = self.notes.iter_mut().find(|note| note.id == note_id) {
            note.updated_at = format_timestamp(timestamp.unix_timestamp());
            note.preview = build_preview(&note.body, self.preview_lines);
        }
    }

    fn update_note_buffer(&mut self, note_id: i64, buffer: &str) {
        if let Some(note) = self.notes.iter_mut().find(|note| note.id == note_id) {
            note.body = buffer.to_string();
            note.preview = build_preview(buffer, self.preview_lines);
        }
    }

    pub fn select_note_by_id(&mut self, note_id: i64) {
        if let Some(idx) = self.notes.iter().position(|note| note.id == note_id) {
            self.selected = idx;
        } else {
            self.normalize_selection();
        }
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.notes.is_empty() {
            return;
        }
        let len = self.notes.len() as isize;
        let current = self.selected as isize;
        let mut next = current + delta;
        if next < 0 {
            next = 0;
        } else if next >= len {
            next = len - 1;
        }
        self.selected = next as usize;
    }

    pub fn refresh(&mut self, storage: &StorageHandle) -> Result<()> {
        if !self.search.query.is_empty() {
            return self.apply_search(storage);
        }

        let records = if self.show_trash {
            storage.fetch_trashed_notes(50)?
        } else {
            storage.fetch_recent_notes(50)?
        };
        self.notes = records
            .into_iter()
            .map(|record| summarize_record(record, self.preview_lines, self.retention_days))
            .collect();
        self.search.terms.clear();
        self.search.tags.clear();
        self.search.filter_chips.clear();
        self.search.regex_pattern = None;
        self.normalize_selection();
        Ok(())
    }

    pub fn set_trash_view(&mut self, enabled: bool, storage: &StorageHandle) -> Result<()> {
        if self.show_trash == enabled {
            return Ok(());
        }
        self.show_trash = enabled;
        self.refresh(storage)
    }

    pub fn restore_selected_note(&mut self, storage: &StorageHandle) -> Result<()> {
        let Some(note) = self.selected() else {
            return Ok(());
        };
        storage.restore_note(note.id)?;
        self.refresh(storage)
    }

    pub fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPane::List => FocusPane::Reader,
            FocusPane::Reader => FocusPane::List,
        };
    }

    pub fn begin_search(&mut self) {
        self.search.active = true;
        self.search.last_error = None;
        self.focus = FocusPane::List;
    }

    pub fn cancel_search(&mut self, storage: &StorageHandle) -> Result<()> {
        if !self.search.active {
            return Ok(());
        }
        self.search.active = false;
        self.search.query.clear();
        self.search.last_error = None;
        self.search.terms.clear();
        self.search.tags.clear();
        self.search.filter_chips.clear();
        self.search.regex_pattern = None;
        self.refresh(storage)
    }

    pub fn finish_search(&mut self) {
        self.search.active = false;
    }

    pub fn push_search_char(&mut self, storage: &StorageHandle, ch: char) -> Result<()> {
        self.search.query.push(ch);
        self.apply_search(storage)
    }

    pub fn pop_search_char(&mut self, storage: &StorageHandle) -> Result<()> {
        if self.search.query.pop().is_some() {
            if self.search.query.is_empty() {
                self.search.last_error = None;
                self.refresh(storage)?;
            } else {
                self.apply_search(storage)?;
            }
        }
        Ok(())
    }

    pub fn search_query(&self) -> &str {
        &self.search.query
    }

    pub fn search_error(&self) -> Option<&str> {
        self.search.last_error.as_deref()
    }

    pub fn is_search_active(&self) -> bool {
        self.search.active
    }

    pub fn is_regex_enabled(&self) -> bool {
        self.search.regex_enabled
    }

    pub fn toggle_regex_mode(&mut self, storage: &StorageHandle) -> Result<bool> {
        let previous = self.search.regex_enabled;
        self.search.regex_enabled = !previous;
        if self.search.query.trim().is_empty() {
            self.search.regex_pattern = None;
            self.search.last_error = None;
            return Ok(self.search.regex_enabled);
        }

        if let Err(err) = self.apply_search(storage) {
            self.search.regex_enabled = previous;
            return Err(err);
        }
        Ok(self.search.regex_enabled)
    }

    fn apply_search(&mut self, storage: &StorageHandle) -> Result<()> {
        let trimmed = self.search.query.trim();
        if trimmed.is_empty() {
            self.search.query.clear();
            self.search.last_error = None;
            self.search.terms.clear();
            self.search.tags.clear();
            self.search.filter_chips.clear();
            self.search.regex_pattern = None;
            return self.refresh(storage);
        }

        let mut query = parse_query(trimmed);
        if self.search.regex_enabled {
            query.regex_pattern = regex_pattern_from_input(trimmed);
        }
        if !query.has_terms() && !query.has_filters() {
            self.search.terms.clear();
            self.search.tags.clear();
            self.search.filter_chips.clear();
            self.search.regex_pattern = None;
            return self.refresh(storage);
        }

        self.search.terms = query.highlight_terms();
        self.search.tags = query.tags.clone();
        self.search.filter_chips = build_filter_chips(&query);
        self.search.regex_pattern = query.regex_pattern.clone();

        let mut storage_query = query.clone();
        if self.search.regex_enabled && storage_query.regex_pattern.is_some() {
            storage_query.terms.clear();
            storage_query.title_terms.clear();
        }

        match storage.search_notes(&storage_query, 200) {
            Ok(records) => {
                self.notes = records
                    .into_iter()
                    .map(|record| summarize_record(record, self.preview_lines, self.retention_days))
                    .collect();
                self.selected = 0;
                self.normalize_selection();
                self.search.last_error = None;
                Ok(())
            }
            Err(err) => {
                self.search.last_error = Some(err.to_string());
                Err(err)
            }
        }
    }

    pub fn search_tokens(&self) -> Vec<String> {
        let mut tokens = self.search.terms.clone();
        tokens.extend(self.search.tags.iter().cloned());
        tokens
    }

    pub fn search_tags(&self) -> &[String] {
        &self.search.tags
    }

    pub fn search_filter_chips(&self) -> &[String] {
        &self.search.filter_chips
    }

    pub fn toggle_archive(&mut self, note_id: i64, archive: bool) {
        if let Some(note) = self.notes.iter_mut().find(|note| note.id == note_id) {
            note.archived = archive;
        }
    }

    pub fn selected_tags(&self) -> &[String] {
        self.selected()
            .map(|note| note.tags.as_slice())
            .unwrap_or(&[])
    }

    pub fn set_status_message<S: Into<String>>(&mut self, message: Option<S>) {
        self.status_message = message.map(Into::into);
    }

    pub fn clear_status_message(&mut self) {
        self.status_message = None;
    }

    pub fn overlay(&self) -> Option<&OverlayState> {
        self.overlay.as_ref()
    }

    pub fn overlay_mut(&mut self) -> Option<&mut OverlayState> {
        self.overlay.as_mut()
    }

    pub fn open_new_note(&mut self) {
        self.overlay = Some(OverlayState::NewNote(NewNoteOverlay {
            title: String::new(),
        }));
    }

    pub fn open_rename_note(&mut self) {
        if let Some(note) = self.selected() {
            self.overlay = Some(OverlayState::RenameNote(RenameNoteOverlay {
                note_id: note.id,
                title: note.title.clone(),
            }));
        }
    }

    pub fn open_delete_note(&mut self) {
        if let Some(note) = self.selected() {
            self.overlay = Some(OverlayState::DeleteNote(DeleteNoteOverlay {
                note_id: note.id,
                title: note.title.clone(),
            }));
        }
    }

    pub fn open_tag_editor(&mut self, storage: &StorageHandle) -> Result<()> {
        let note = match self.selected() {
            Some(note) => note,
            None => return Ok(()),
        };
        let mut tags = storage.list_all_tags()?;
        tags.sort_by(|a, b| a.to_lowercase().cmp(&b.to_lowercase()));
        let note_tags: HashSet<String> = note.tags.iter().cloned().collect();
        let mut items = Vec::new();
        let mut seen = HashSet::new();
        for tag in &tags {
            let selected = note_tags.contains(tag);
            items.push(TagEditorItem {
                name: tag.clone(),
                selected,
                original: selected,
                bulk_selected: false,
            });
            seen.insert(tag.clone());
        }
        for tag in &note.tags {
            if !seen.contains(tag) {
                items.push(TagEditorItem {
                    name: tag.clone(),
                    selected: true,
                    original: true,
                    bulk_selected: false,
                });
            }
        }
        items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        let suggestions = tags
            .iter()
            .filter(|tag| !note_tags.contains(*tag))
            .take(5)
            .cloned()
            .collect();

        let overlay = TagEditorOverlay {
            note_id: note.id,
            items,
            selected_index: 0,
            mode: TagEditorMode::default(),
            input: String::new(),
            status: None,
            suggestions,
        };
        self.overlay = Some(OverlayState::TagEditor(overlay));
        Ok(())
    }

    pub fn open_recovery_overlay(
        &mut self,
        storage: &StorageHandle,
        snapshots: Vec<RecoverySnapshot>,
    ) -> Result<()> {
        if snapshots.is_empty() {
            return Ok(());
        }
        let mut entries = Vec::with_capacity(snapshots.len());
        for snapshot in snapshots {
            let note_id = snapshot.note_id;
            let saved_at = format_datetime(snapshot.saved_at);
            let saved_relative = format_relative_time(snapshot.saved_at);
            let body = snapshot.body.clone();
            let record = storage.fetch_note_by_id(note_id)?;
            let (title, missing) = match record {
                Some(note) => (note.title, false),
                None => (format!("Recovered note #{} (missing)", note_id), true),
            };
            entries.push(RecoveryEntry {
                note_id,
                title,
                saved_at,
                saved_relative,
                body,
                preview: build_recovery_preview(&snapshot.body),
                missing,
            });
        }
        self.overlay = Some(OverlayState::Recovery(RecoveryOverlay {
            entries,
            selected: 0,
        }));
        Ok(())
    }

    pub fn close_overlay(&mut self) {
        self.overlay = None;
    }

    pub fn new_note_overlay(&self) -> Option<&NewNoteOverlay> {
        match self.overlay() {
            Some(OverlayState::NewNote(ref overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn new_note_overlay_mut(&mut self) -> Option<&mut NewNoteOverlay> {
        match self.overlay_mut() {
            Some(OverlayState::NewNote(ref mut overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn rename_note_overlay(&self) -> Option<&RenameNoteOverlay> {
        match self.overlay() {
            Some(OverlayState::RenameNote(ref overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn rename_note_overlay_mut(&mut self) -> Option<&mut RenameNoteOverlay> {
        match self.overlay_mut() {
            Some(OverlayState::RenameNote(ref mut overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn delete_note_overlay(&self) -> Option<&DeleteNoteOverlay> {
        match self.overlay() {
            Some(OverlayState::DeleteNote(ref overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn delete_note_overlay_mut(&mut self) -> Option<&mut DeleteNoteOverlay> {
        match self.overlay_mut() {
            Some(OverlayState::DeleteNote(ref mut overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn open_bulk_trash_overlay(&mut self, action: BulkTrashAction) {
        self.overlay = Some(OverlayState::BulkTrash(BulkTrashOverlay { action }));
    }

    pub fn bulk_trash_overlay(&self) -> Option<&BulkTrashOverlay> {
        match self.overlay() {
            Some(OverlayState::BulkTrash(ref overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn bulk_trash_overlay_mut(&mut self) -> Option<&mut BulkTrashOverlay> {
        match self.overlay_mut() {
            Some(OverlayState::BulkTrash(ref mut overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn bulk_trash_action(&self) -> Option<BulkTrashAction> {
        self.bulk_trash_overlay().map(|overlay| overlay.action)
    }

    pub fn tag_editor_overlay(&self) -> Option<&TagEditorOverlay> {
        match self.overlay() {
            Some(OverlayState::TagEditor(ref overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn tag_editor_overlay_mut(&mut self) -> Option<&mut TagEditorOverlay> {
        match self.overlay_mut() {
            Some(OverlayState::TagEditor(ref mut overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn recovery_overlay(&self) -> Option<&RecoveryOverlay> {
        match self.overlay() {
            Some(OverlayState::Recovery(ref overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn recovery_overlay_mut(&mut self) -> Option<&mut RecoveryOverlay> {
        match self.overlay_mut() {
            Some(OverlayState::Recovery(ref mut overlay)) => Some(overlay),
            _ => None,
        }
    }

    pub fn recovery_move_selection(&mut self, delta: isize) {
        if let Some(overlay) = self.recovery_overlay_mut() {
            if overlay.entries.is_empty() {
                overlay.selected = 0;
                return;
            }
            let len = overlay.entries.len() as isize;
            let current = overlay.selected as isize;
            let mut next = current + delta;
            if next < 0 {
                next = 0;
            } else if next >= len {
                next = len - 1;
            }
            overlay.selected = next as usize;
        }
    }

    pub fn recovery_selected_entry(&self) -> Option<&RecoveryEntry> {
        self.recovery_overlay()
            .and_then(|overlay| overlay.entries.get(overlay.selected))
    }

    pub fn recovery_entries(&self) -> &[RecoveryEntry] {
        self.recovery_overlay()
            .map(|overlay| overlay.entries.as_slice())
            .unwrap_or(&[])
    }

    pub fn recovery_remove_selected(&mut self) -> Option<RecoveryEntry> {
        let mut should_clear = false;
        let removed = match self.overlay_mut() {
            Some(OverlayState::Recovery(ref mut overlay)) => {
                if overlay.entries.is_empty() {
                    return None;
                }
                let removed = overlay.entries.remove(overlay.selected);
                if overlay.entries.is_empty() {
                    should_clear = true;
                } else if overlay.selected >= overlay.entries.len() {
                    overlay.selected = overlay.entries.len() - 1;
                }
                removed
            }
            _ => return None,
        };
        if should_clear {
            self.overlay = None;
        }
        Some(removed)
    }

    pub fn recovery_remove_for_note(&mut self, note_id: i64) {
        let mut should_clear = false;
        if let Some(OverlayState::Recovery(ref mut overlay)) = self.overlay_mut() {
            let original_len = overlay.entries.len();
            overlay.entries.retain(|entry| entry.note_id != note_id);
            if overlay.entries.is_empty() && original_len > 0 {
                should_clear = true;
            } else if overlay.selected >= overlay.entries.len() && !overlay.entries.is_empty() {
                overlay.selected = overlay.entries.len() - 1;
            }
        }
        if should_clear {
            self.overlay = None;
        }
    }

    pub fn tag_editor_mode(&self) -> TagEditorMode {
        self.tag_editor_overlay()
            .map(|overlay| overlay.mode.clone())
            .unwrap_or(TagEditorMode::Browse)
    }

    pub fn tag_editor_move_selection(&mut self, delta: isize) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if editor.items.is_empty() {
                editor.selected_index = 0;
                editor.status = None;
                editor.input.clear();
                editor.mode = TagEditorMode::Browse;
                return;
            }
            let len = editor.items.len() as isize;
            let current = editor.selected_index as isize;
            let mut next = current + delta;
            if next < 0 {
                next = 0;
            } else if next >= len {
                next = len - 1;
            }
            editor.selected_index = next as usize;
            editor.status = None;
            editor.mode = TagEditorMode::Browse;
            editor.input.clear();
        }
    }

    pub fn tag_editor_toggle_selection(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if let Some(item) = editor.items.get_mut(editor.selected_index) {
                item.selected = !item.selected;
            }
            editor.status = None;
            editor.mode = TagEditorMode::Browse;
            editor.input.clear();
        }
    }

    pub fn tag_editor_begin_add(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            editor.mode = TagEditorMode::Input(TagInputKind::Add);
            editor.input.clear();
            editor.status = Some("New tag: type name, Enter to add, Esc to cancel".into());
        }
    }

    pub fn tag_editor_begin_rename(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if let Some(item) = editor.items.get(editor.selected_index) {
                editor.mode = TagEditorMode::Input(TagInputKind::Rename {
                    original: item.name.clone(),
                });
                editor.input = item.name.clone();
                editor.status = Some(format!(
                    "Renaming '{}': edit and press Enter, Esc to cancel",
                    item.name
                ));
            }
        }
    }

    pub fn tag_editor_begin_merge(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if let Some(item) = editor.items.get(editor.selected_index) {
                editor.mode = TagEditorMode::Input(TagInputKind::Merge {
                    sources: vec![item.name.clone()],
                });
                editor.input.clear();
                editor.status = Some(format!(
                    "Merge '{}' into existing tag: type target name",
                    item.name
                ));
            }
        }
    }

    pub fn tag_editor_begin_marked_merge(&mut self) -> bool {
        let Some(editor) = self.tag_editor_overlay_mut() else {
            return false;
        };
        let sources: Vec<String> = editor
            .items
            .iter()
            .filter(|item| item.bulk_selected)
            .map(|item| item.name.clone())
            .collect();
        if sources.len() < 2 {
            editor.status = Some("Mark at least two tags with 'v' before using bulk merge".into());
            return false;
        }
        editor.mode = TagEditorMode::Input(TagInputKind::Merge { sources });
        editor.input.clear();
        editor.status = Some("Merge marked tags into existing tag: type target name".into());
        true
    }

    pub fn tag_editor_toggle_bulk_mark(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if let Some(item) = editor.items.get_mut(editor.selected_index) {
                item.bulk_selected = !item.bulk_selected;
                if item.bulk_selected {
                    editor
                        .status
                        .replace(format!("Marked '{}' for bulk actions", item.name));
                } else {
                    editor.status.replace(format!("Unmarked '{}'", item.name));
                }
            }
        }
    }

    pub fn tag_editor_clear_bulk_marks(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            let mut cleared = 0;
            for item in &mut editor.items {
                if item.bulk_selected {
                    item.bulk_selected = false;
                    cleared += 1;
                }
            }
            if cleared > 0 {
                editor.status.replace(format!(
                    "Cleared {cleared} bulk mark{}",
                    if cleared == 1 { "" } else { "s" }
                ));
            } else {
                editor.status.replace("No bulk marks to clear".into());
            }
        }
    }

    pub fn tag_editor_apply_suggestion(&mut self, index: usize) -> Option<String> {
        let editor = self.tag_editor_overlay_mut()?;
        if index >= editor.suggestions.len() {
            editor.status.replace("No suggestion in that slot".into());
            return None;
        }
        let tag = editor.suggestions.remove(index);
        let mut selected_index = None;
        if let Some((idx, item)) = editor
            .items
            .iter_mut()
            .enumerate()
            .find(|(_, item)| item.name.eq_ignore_ascii_case(&tag))
        {
            item.selected = true;
            item.bulk_selected = false;
            selected_index = Some(idx);
        } else {
            editor.items.push(TagEditorItem {
                name: tag.clone(),
                selected: true,
                original: false,
                bulk_selected: false,
            });
            editor
                .items
                .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            if let Some(idx) = editor
                .items
                .iter()
                .position(|item| item.name.eq_ignore_ascii_case(&tag))
            {
                selected_index = Some(idx);
            }
        }
        if let Some(idx) = selected_index {
            editor.selected_index = idx;
        }
        editor.status = Some(format!("Queued tag '{tag}' (press Enter to save)"));
        Some(tag)
    }

    pub fn tag_editor_begin_delete(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if let Some(item) = editor.items.get(editor.selected_index) {
                editor.mode = TagEditorMode::ConfirmDelete {
                    tag: item.name.clone(),
                };
                editor.status = Some(format!(
                    "Delete '{}'? Press y to confirm or n / Esc to cancel",
                    item.name
                ));
            }
        }
    }

    pub fn tag_editor_push_char(&mut self, ch: char) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if matches!(editor.mode, TagEditorMode::Input(_)) && editor.input.len() < 64 {
                editor.input.push(ch);
                editor.status = None;
            }
        }
    }

    pub fn tag_editor_pop_char(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if matches!(editor.mode, TagEditorMode::Input(_)) {
                editor.input.pop();
                editor.status = None;
            }
        }
    }

    pub fn tag_editor_commit_input(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            if !matches!(editor.mode, TagEditorMode::Input(TagInputKind::Add)) {
                return;
            }
            let name = editor.input.trim();
            if name.is_empty() {
                editor.status = Some("Tag cannot be empty".into());
                return;
            }
            let normalized = name.to_string();
            let mut message = String::from("Tag added");
            if let Some(existing) = editor
                .items
                .iter_mut()
                .find(|item| item.name.eq_ignore_ascii_case(name))
            {
                existing.selected = true;
                message = String::from("Tag selected");
            } else {
                editor.items.push(TagEditorItem {
                    name: normalized.clone(),
                    selected: true,
                    original: false,
                    bulk_selected: false,
                });
                editor
                    .items
                    .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                if let Some(idx) = editor
                    .items
                    .iter()
                    .position(|item| item.name.eq_ignore_ascii_case(name))
                {
                    editor.selected_index = idx;
                }
            }
            editor.mode = TagEditorMode::Browse;
            editor.input.clear();
            editor.status = Some(message);
        }
    }

    pub fn tag_editor_cancel_input(&mut self) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            match editor.mode {
                TagEditorMode::Input(_) | TagEditorMode::ConfirmDelete { .. } => {
                    editor.mode = TagEditorMode::Browse;
                    editor.input.clear();
                    editor.status = None;
                }
                TagEditorMode::Browse => {}
            }
        }
    }

    pub fn tag_editor_selected_name(&self) -> Option<String> {
        self.tag_editor_overlay()
            .and_then(|overlay| overlay.items.get(overlay.selected_index))
            .map(|item| item.name.clone())
    }

    pub fn tag_editor_input_value(&self) -> Option<String> {
        self.tag_editor_overlay()
            .and_then(|overlay| match overlay.mode {
                TagEditorMode::Input(_) => Some(overlay.input.trim().to_string()),
                _ => None,
            })
    }

    pub fn tag_editor_finish_rename(&mut self, from: &str, to: &str) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            for item in &mut editor.items {
                if item.name == from {
                    let was_selected = item.selected;
                    let was_original = item.original;
                    let was_bulk = item.bulk_selected;
                    item.name = to.to_string();
                    item.selected = was_selected;
                    item.original = was_original;
                    item.bulk_selected = was_bulk;
                    break;
                }
            }
            editor
                .items
                .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            if let Some(idx) = editor
                .items
                .iter()
                .position(|item| item.name.eq_ignore_ascii_case(to))
            {
                editor.selected_index = idx;
            }
            editor.mode = TagEditorMode::Browse;
            editor.input.clear();
            editor.status = Some(format!("Renamed tag '{from}' → '{to}'"));
        }
    }

    pub fn tag_editor_finish_merge(&mut self, from: &str, to: &str) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            let mut carried_selected = false;
            let mut carried_original = false;
            editor.items.retain(|item| {
                if item.name == from {
                    carried_selected = item.selected;
                    carried_original = item.original;
                    false
                } else {
                    true
                }
            });

            if let Some(target) = editor
                .items
                .iter_mut()
                .find(|item| item.name.eq_ignore_ascii_case(to))
            {
                if carried_selected {
                    target.selected = true;
                }
                if carried_original {
                    target.original = true;
                }
                target.bulk_selected = false;
            } else {
                editor.items.push(TagEditorItem {
                    name: to.to_string(),
                    selected: carried_selected,
                    original: carried_original,
                    bulk_selected: false,
                });
            }

            if editor.items.is_empty() {
                editor.selected_index = 0;
            } else {
                editor
                    .items
                    .sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
                editor.selected_index = editor
                    .items
                    .iter()
                    .position(|item| item.name.eq_ignore_ascii_case(to))
                    .unwrap_or(0);
            }

            editor.mode = TagEditorMode::Browse;
            editor.input.clear();
            editor.status = Some(format!("Merged '{from}' into '{to}'"));
            for item in &mut editor.items {
                item.bulk_selected = false;
            }
        }
    }

    pub fn tag_editor_finish_delete(&mut self, tag: &str) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            editor.items.retain(|item| item.name != tag);
            if editor.selected_index >= editor.items.len() && !editor.items.is_empty() {
                editor.selected_index = editor.items.len() - 1;
            }
            editor.mode = TagEditorMode::Browse;
            editor.input.clear();
            if editor.items.is_empty() {
                editor.status = Some("No tags remain".into());
            } else {
                editor.status = Some(format!("Deleted tag '{tag}'"));
            }
        }
    }

    pub fn tag_editor_set_status<S: Into<String>>(&mut self, message: S) {
        if let Some(editor) = self.tag_editor_overlay_mut() {
            editor.status = Some(message.into());
        }
    }

    pub fn tag_editor_changes(&self) -> Option<(Vec<String>, Vec<String>, i64)> {
        match self.tag_editor_overlay() {
            Some(editor) => {
                let mut add = Vec::new();
                let mut remove = Vec::new();
                for item in &editor.items {
                    if item.selected && !item.original {
                        add.push(item.name.clone());
                    } else if !item.selected && item.original {
                        remove.push(item.name.clone());
                    }
                }
                Some((add, remove, editor.note_id))
            }
            None => None,
        }
    }

    fn normalize_selection(&mut self) {
        if self.notes.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.notes.len() {
            self.selected = self.notes.len() - 1;
        }
    }
}

fn prev_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor == 0 {
        return 0;
    }
    let mut last = 0;
    for (idx, _) in text[..cursor].grapheme_indices(true) {
        last = idx;
    }
    last
}

fn next_grapheme_boundary(text: &str, cursor: usize) -> usize {
    if cursor >= text.len() {
        return text.len();
    }
    let mut iter = text[cursor..].graphemes(true);
    if let Some(grapheme) = iter.next() {
        cursor + grapheme.len()
    } else {
        text.len()
    }
}

fn line_start(text: &str, cursor: usize) -> usize {
    text[..cursor].rfind('\n').map(|idx| idx + 1).unwrap_or(0)
}

fn line_end(text: &str, cursor: usize) -> usize {
    text[cursor..]
        .find('\n')
        .map(|idx| cursor + idx)
        .unwrap_or_else(|| text.len())
}

fn column_at(text: &str, line_start: usize, cursor: usize) -> usize {
    text[line_start..cursor].graphemes(true).count()
}

fn position_for_column(text: &str, line_start: usize, column: usize) -> usize {
    let line_end = line_end(text, line_start);
    let mut position = line_start;
    let mut count = 0;
    for grapheme in text[line_start..line_end].graphemes(true) {
        if count >= column {
            break;
        }
        position += grapheme.len();
        count += 1;
    }
    if column > count {
        line_end
    } else {
        position
    }
}

#[cfg(test)]
mod tests {
    use super::{compute_trash_status, EditorState};
    use time::OffsetDateTime;

    #[test]
    fn editor_undo_redo_cycles() {
        let mut editor = EditorState::new(1, "hello".to_string());
        assert!(editor.insert_char('!'));
        assert_eq!(editor.buffer(), "hello!");
        assert!(editor.undo());
        assert_eq!(editor.buffer(), "hello");
        assert!(!editor.undo());
        assert!(editor.redo());
        assert_eq!(editor.buffer(), "hello!");
    }

    #[test]
    fn editor_word_navigation_skips_whitespace() {
        let mut editor = EditorState::new(1, "alpha  beta".to_string());
        editor.move_end();
        assert!(editor.move_word_left());
        assert_eq!(editor.cursor(), 7); // start of "beta"
        assert!(editor.move_word_left());
        assert_eq!(editor.cursor(), 0);
        assert!(editor.move_word_right());
        assert_eq!(editor.cursor(), 7); // start of "beta"
    }

    #[test]
    fn editor_mark_clean_resets_history() {
        let mut editor = EditorState::new(1, "seed".to_string());
        editor.insert_char('s');
        editor.mark_clean();
        assert!(!editor.is_dirty());
        assert!(!editor.undo());
    }

    #[test]
    fn trash_status_manual_purge_only_when_retention_zero() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let status = compute_trash_status(Some(now), 0).expect("status");
        assert_eq!(status.label, "Manual purge only");
        assert!(!status.expired);
        assert!(status.indefinite);
    }

    #[test]
    fn trash_status_marks_expired_when_past_retention_window() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let deleted_at = now - (86_400 * 2);
        let status = compute_trash_status(Some(deleted_at), 1).expect("status");
        assert!(status.expired);
        assert!(status.label.contains("Expired"));
        assert!(!status.indefinite);
    }

    #[test]
    fn trash_status_reports_remaining_time() {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let deleted_at = now - (86_400 - 3_600); // about 1 hour remaining in a 1 day window
        let status = compute_trash_status(Some(deleted_at), 1).expect("status");
        assert!(!status.expired);
        assert!(!status.indefinite);
        assert!(
            status.label.contains("left"),
            "expected countdown label, got {}",
            status.label
        );
    }
}

fn summarize_record(record: NoteRecord, preview_lines: usize, retention_days: u32) -> NoteSummary {
    let NoteRecord {
        id,
        title,
        body,
        snippet,
        updated_at,
        pinned,
        archived,
        tags,
        deleted_at,
        ..
    } = record;

    let preview = if preview_lines == 0 {
        String::new()
    } else if let Some(snippet) = snippet {
        let trimmed = snippet.trim();
        if trimmed.is_empty() {
            build_preview(&body, preview_lines)
        } else {
            trimmed.to_string()
        }
    } else {
        build_preview(&body, preview_lines)
    };

    NoteSummary {
        id,
        title,
        updated_at: format_timestamp(updated_at),
        preview,
        body,
        pinned,
        archived,
        tags,
        deleted_at,
        deleted_label: deleted_at.map(format_timestamp),
        trash_status: compute_trash_status(deleted_at, retention_days),
    }
}

fn compute_trash_status(deleted_at: Option<i64>, retention_days: u32) -> Option<TrashStatus> {
    let deleted_at = deleted_at?;
    if retention_days == 0 {
        return Some(TrashStatus {
            label: "Manual purge only".into(),
            expired: false,
            indefinite: true,
        });
    }

    let purge_at = deleted_at + i64::from(retention_days) * 86_400;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let remaining = purge_at - now;
    if remaining <= 0 {
        return Some(TrashStatus {
            label: "Expired — purge soon".into(),
            expired: true,
            indefinite: false,
        });
    }

    let label = if remaining >= 86_400 * 2 {
        let days = remaining / 86_400;
        format!("{days}d left")
    } else if remaining >= 86_400 {
        let days = remaining / 86_400;
        let hours = (remaining % 86_400 + 3_599) / 3_600;
        if hours == 0 {
            format!("{days}d left")
        } else {
            format!("{days}d {hours}h left")
        }
    } else if remaining >= 3_600 {
        let hours = (remaining + 3_599) / 3_600;
        format!("{hours}h left")
    } else {
        let minutes = (remaining + 59) / 60;
        format!("{minutes}m left")
    };

    Some(TrashStatus {
        label,
        expired: false,
        indefinite: false,
    })
}

fn build_filter_chips(query: &SearchQuery) -> Vec<String> {
    let mut chips = Vec::new();
    for tag in &query.tags {
        chips.push(format!("tag:{}", tag));
    }
    if let Some(created) = format_range_chip("created", &query.created) {
        chips.push(created);
    }
    if let Some(updated) = format_range_chip("updated", &query.updated) {
        chips.push(updated);
    }
    chips
}

fn format_range_chip(label: &str, range: &RangeFilter) -> Option<String> {
    if !range.has_range() {
        return None;
    }
    let from = range.from.map(format_epoch_date);
    let to = range.to.map(format_epoch_date);
    match (from, to) {
        (Some(start), Some(end)) => Some(format!("{label}:{start}..{end}")),
        (Some(start), None) => Some(format!("{label}:{start}..")),
        (None, Some(end)) => Some(format!("{label}:..{end}")),
        _ => None,
    }
}

fn format_datetime(dt: OffsetDateTime) -> String {
    dt.format(&Rfc3339)
        .unwrap_or_else(|_| dt.unix_timestamp().to_string())
}

fn format_relative_time(dt: OffsetDateTime) -> String {
    let now = OffsetDateTime::now_utc();
    let diff = now - dt;
    if diff.is_negative() {
        return "just now".to_string();
    }
    if diff < Duration::seconds(45) {
        return "just now".to_string();
    }
    if diff < Duration::minutes(90) {
        let mins = diff.whole_minutes().max(1);
        return format!("{mins}m ago");
    }
    if diff < Duration::hours(36) {
        let hours = diff.whole_hours().max(1);
        return format!("{hours}h ago");
    }
    if diff < Duration::days(10) {
        let days = diff.whole_days().max(1);
        return format!("{days}d ago");
    }
    dt.format(&Rfc3339)
        .unwrap_or_else(|_| dt.unix_timestamp().to_string())
}

fn format_timestamp(epoch: i64) -> String {
    OffsetDateTime::from_unix_timestamp(epoch)
        .map(|dt| dt.format(&Rfc3339).unwrap_or_else(|_| epoch.to_string()))
        .unwrap_or_else(|_| epoch.to_string())
}

fn format_epoch_date(epoch: i64) -> String {
    OffsetDateTime::from_unix_timestamp(epoch)
        .map(|dt| dt.date().to_string())
        .unwrap_or_else(|_| epoch.to_string())
}

fn build_preview(body: &str, preview_lines: usize) -> String {
    if preview_lines == 0 {
        return String::new();
    }
    let mut lines = body.lines();
    let mut collected = Vec::with_capacity(preview_lines);
    for _ in 0..preview_lines {
        if let Some(line) = lines.next() {
            collected.push(line.trim_end());
        } else {
            break;
        }
    }
    let mut preview = collected.join("\n");
    if lines.next().is_some() {
        if !preview.is_empty() {
            preview.push_str("\n…");
        } else {
            preview.push('…');
        }
    }
    preview
}

fn build_recovery_preview(body: &str) -> Vec<String> {
    const MAX_LINES: usize = 4;
    const MAX_COLS: usize = 80;
    let mut preview = Vec::new();
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut snippet = trimmed.chars().take(MAX_COLS).collect::<String>();
        if trimmed.chars().count() > MAX_COLS {
            snippet.push('…');
        }
        preview.push(snippet);
        if preview.len() == MAX_LINES {
            break;
        }
    }
    if preview.is_empty() {
        preview.push("(empty draft)".to_string());
    }
    preview
}
