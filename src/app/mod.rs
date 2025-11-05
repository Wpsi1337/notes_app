use std::io::Stdout;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::widgets::ListState;
use ratatui::Terminal;
use time::format_description::well_known::Rfc3339;

use crate::config::{AppConfig, ConfigPaths};
use crate::journaling::{AutoSaveEvent, AutoSaveRuntime, AutoSaveStatus, RecoverySnapshot};
use crate::storage::StorageHandle;
use crate::ui;

mod actions;
pub mod state;

pub use state::{AppState, EditorState, FocusPane, NoteSummary, OverlayState, TagEditorMode};

enum Action {
    Quit,
    SelectNext,
    SelectPrevious,
    ToggleFocus,
    Refresh,
    NewNote,
    RenameNote,
    EnterEdit,
    StartSearch,
    TogglePin,
    ToggleArchive,
    DeleteNote,
    ToggleRegex,
    ToggleTrashView,
    RestoreNote,
    ShowTagEditor,
    ToggleWrap,
    ManualSave,
}

pub struct App {
    pub config: Arc<AppConfig>,
    pub storage: StorageHandle,
    state: AppState,
    list_state: ListState,
    should_quit: bool,
    tick_rate: Duration,
    auto_save: AutoSaveRuntime,
    recovery_snapshots: Vec<RecoverySnapshot>,
}

impl App {
    pub fn new(config: Arc<AppConfig>, storage: StorageHandle, paths: ConfigPaths) -> Result<Self> {
        let preview_lines = config.preview_lines as usize;
        let mut state = AppState::load(&storage, preview_lines)
            .context("loading note summaries for initial state")?;
        let mut list_state = ListState::default();
        if !state.is_empty() {
            list_state.select(Some(state.selected));
        }
        let auto_save_dir = paths.state_dir.join("autosave");
        let auto_save = AutoSaveRuntime::new(auto_save_dir, &config.auto_save)
            .context("initialising autosave runtime")?;
        let recovery_snapshots = auto_save
            .list_recovery()
            .context("loading autosave recovery snapshots")?;
        state.set_autosave_status(auto_save.status());
        if !recovery_snapshots.is_empty() {
            state.set_status_message(Some(format!(
                "Recovered {} autosave draft(s); open the note and press 'e' to review.",
                recovery_snapshots.len()
            )));
        }
        Ok(Self {
            config,
            storage,
            state,
            list_state,
            should_quit: false,
            tick_rate: Duration::from_millis(250),
            auto_save,
            recovery_snapshots,
        })
    }

    pub fn run(&mut self) -> Result<()> {
        let mut terminal = setup_terminal()?;
        let result = self.event_loop(&mut terminal);
        restore_terminal(&mut terminal)?;
        result
    }

    pub fn autosave_status(&self) -> AutoSaveStatus {
        self.auto_save.status()
    }

    pub fn take_recovery_snapshots(&mut self) -> Vec<RecoverySnapshot> {
        std::mem::take(&mut self.recovery_snapshots)
    }

    pub fn discard_recovery_snapshot(&self, note_id: i64) -> Result<()> {
        self.auto_save.discard_snapshot(note_id)
    }

    fn event_loop(&mut self, terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
        let mut last_tick = Instant::now();
        loop {
            terminal
                .draw(|frame| {
                    if !self.state.is_empty() {
                        self.list_state.select(Some(self.state.selected));
                    } else {
                        self.list_state.select(None);
                    }
                    ui::draw_app(frame, &self.state, &mut self.list_state);
                })
                .context("rendering frame")?;

            if self.should_quit {
                break;
            }

            let timeout = self
                .tick_rate
                .checked_sub(last_tick.elapsed())
                .unwrap_or_else(|| Duration::from_millis(0));

            if event::poll(timeout).context("polling for terminal events")? {
                match event::read().context("reading terminal event")? {
                    Event::Key(key) => self.handle_key(key),
                    Event::Resize(_, _) => {
                        // no-op: next draw will naturally adapt to the new size
                    }
                    _ => {}
                }
            }

            if last_tick.elapsed() >= self.tick_rate {
                self.on_tick();
                last_tick = Instant::now();
            }
        }
        Ok(())
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }

        if self.handle_overlay_key(key) {
            return;
        }

        if self.state.is_editing() {
            if self.handle_editor_key(key) {
                return;
            }
        }

        if self.state.is_search_active() {
            match key.code {
                KeyCode::Esc => {
                    if let Err(err) = self.state.cancel_search(&self.storage) {
                        tracing::error!(?err, "failed to cancel search");
                    }
                    return;
                }
                KeyCode::Enter => {
                    self.state.finish_search();
                    return;
                }
                KeyCode::Backspace => {
                    if let Err(err) = self.state.pop_search_char(&self.storage) {
                        tracing::error!(?err, "failed to trim search query");
                    }
                    return;
                }
                KeyCode::Char(ch)
                    if !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                    ) =>
                {
                    if let Err(err) = self.state.push_search_char(&self.storage, ch) {
                        tracing::error!(?err, "failed to extend search query");
                    }
                    return;
                }
                _ => {}
            }
        }

        let action = match key.code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::Quit)
            }
            KeyCode::Char('j') | KeyCode::Down => Some(Action::SelectNext),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::SelectPrevious),
            KeyCode::Tab => Some(Action::ToggleFocus),
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::Refresh)
            }
            KeyCode::Char('a')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::NewNote)
            }
            KeyCode::Char('r')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::RenameNote)
            }
            KeyCode::Char('e')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::EnterEdit)
            }
            KeyCode::Char('p')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::TogglePin)
            }
            KeyCode::Char('d')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::DeleteNote)
            }
            KeyCode::Char('R')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::ToggleRegex)
            }
            KeyCode::Char('T') => Some(Action::ToggleTrashView),
            KeyCode::Char('u')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::RestoreNote)
            }
            KeyCode::Char('W') => Some(Action::ToggleWrap),
            KeyCode::Char('t')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::ShowTagEditor)
            }
            KeyCode::Char('A') => Some(Action::ToggleArchive),
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                Some(Action::ManualSave)
            }
            KeyCode::Char('/')
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                Some(Action::StartSearch)
            }
            _ => None,
        };

        if let Some(action) = action {
            self.handle_action(action);
        }
    }

    fn handle_action(&mut self, action: Action) {
        if self.state.is_editing() {
            match action {
                Action::ManualSave | Action::Quit | Action::ToggleWrap => {}
                _ => {
                    self.state.set_status_message(Some(
                        "Finish editing (Esc to exit, Ctrl-s to save) before performing other actions.",
                    ));
                    return;
                }
            }
        }
        match action {
            Action::Quit => {
                if self.state.is_editing() && !self.exit_editing() {
                    return;
                }
                self.should_quit = true;
            }
            Action::SelectNext => self.state.move_selection(1),
            Action::SelectPrevious => self.state.move_selection(-1),
            Action::ToggleFocus => self.state.toggle_focus(),
            Action::Refresh => {
                if let Err(err) = self.state.refresh(&self.storage) {
                    tracing::error!(?err, "failed to refresh notes from storage");
                }
            }
            Action::NewNote => {
                if self.state.overlay().is_none() {
                    self.state.open_new_note();
                    self.state
                        .set_status_message(Some("Enter a title and press Enter"));
                }
            }
            Action::RenameNote => self.handle_rename_note(),
            Action::EnterEdit => self.handle_enter_edit(),
            Action::StartSearch => {
                self.state.begin_search();
            }
            Action::TogglePin => self.handle_toggle_pin(),
            Action::ToggleArchive => self.handle_toggle_archive(),
            Action::DeleteNote => self.handle_delete_note(),
            Action::ToggleRegex => self.handle_toggle_regex(),
            Action::ToggleTrashView => self.handle_toggle_trash_view(),
            Action::RestoreNote => self.handle_restore_note(),
            Action::ShowTagEditor => self.handle_show_tag_editor(),
            Action::ToggleWrap => self.handle_toggle_wrap(),
            Action::ManualSave => {
                self.handle_manual_save();
            }
        }
    }

    fn on_tick(&mut self) {
        match self.auto_save.poll(&self.storage) {
            Ok(Some(event)) => self.handle_autosave_event(event),
            Ok(None) => {}
            Err(err) => {
                tracing::error!(?err, "autosave tick errored");
            }
        }
        self.state.set_autosave_status(self.auto_save.status());
    }

    fn handle_overlay_key(&mut self, key: KeyEvent) -> bool {
        match self.state.overlay() {
            Some(OverlayState::NewNote(_)) => {
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_overlay();
                        self.state.set_status_message(Some("Canceled new note"));
                    }
                    KeyCode::Enter => {
                        self.submit_new_note();
                    }
                    KeyCode::Backspace => {
                        if let Some(draft) = self.state.new_note_overlay_mut() {
                            draft.title.pop();
                        }
                    }
                    KeyCode::Char(ch)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        if let Some(draft) = self.state.new_note_overlay_mut() {
                            if draft.title.len() < 120 {
                                draft.title.push(ch);
                            }
                        }
                    }
                    _ => {}
                }
                true
            }
            Some(OverlayState::RenameNote(_)) => {
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_overlay();
                        self.state.set_status_message(Some("Rename canceled"));
                    }
                    KeyCode::Enter => {
                        self.submit_rename_note();
                    }
                    KeyCode::Backspace => {
                        if let Some(draft) = self.state.rename_note_overlay_mut() {
                            draft.title.pop();
                        }
                    }
                    KeyCode::Char(ch)
                        if !key.modifiers.intersects(
                            KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                        ) =>
                    {
                        if let Some(draft) = self.state.rename_note_overlay_mut() {
                            if draft.title.len() < 120 {
                                draft.title.push(ch);
                            }
                        }
                    }
                    _ => {}
                }
                true
            }
            Some(OverlayState::DeleteNote(_)) => {
                match key.code {
                    KeyCode::Esc => {
                        self.state.close_overlay();
                        self.state.set_status_message(Some("Delete canceled"));
                    }
                    KeyCode::Enter => {
                        self.submit_delete_note();
                    }
                    _ => {}
                }
                true
            }
            Some(OverlayState::TagEditor(_)) => {
                let mode = self
                    .state
                    .tag_editor_overlay()
                    .map(|overlay| overlay.mode.clone())
                    .unwrap_or(TagEditorMode::Browse);
                match mode {
                    TagEditorMode::Adding => {
                        match key.code {
                            KeyCode::Esc => {
                                self.state.tag_editor_cancel_input();
                            }
                            KeyCode::Enter => {
                                self.state.tag_editor_commit_input();
                            }
                            KeyCode::Backspace => {
                                self.state.tag_editor_pop_char();
                            }
                            KeyCode::Char(ch)
                                if !key.modifiers.intersects(
                                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                                ) =>
                            {
                                self.state.tag_editor_push_char(ch);
                            }
                            _ => {}
                        }
                        true
                    }
                    TagEditorMode::Browse => {
                        match key.code {
                            KeyCode::Esc => {
                                self.state.close_overlay();
                                self.state.set_status_message(Some("Canceled tag changes"));
                            }
                            KeyCode::Enter => {
                                self.apply_tag_editor_changes();
                            }
                            KeyCode::Char('a')
                                if !key.modifiers.intersects(
                                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                                ) =>
                            {
                                self.state.tag_editor_begin_add();
                            }
                            KeyCode::Char(' ')
                                if !key.modifiers.intersects(
                                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                                ) =>
                            {
                                self.state.tag_editor_toggle_selection();
                            }
                            KeyCode::Char('j') | KeyCode::Down => {
                                self.state.tag_editor_move_selection(1);
                            }
                            KeyCode::Char('k') | KeyCode::Up => {
                                self.state.tag_editor_move_selection(-1);
                            }
                            KeyCode::PageDown => {
                                self.state.tag_editor_move_selection(5);
                            }
                            KeyCode::PageUp => {
                                self.state.tag_editor_move_selection(-5);
                            }
                            _ => {}
                        }
                        true
                    }
                }
            }
            None => false,
        }
    }

    fn handle_toggle_trash_view(&mut self) {
        let enabled = !self.state.show_trash;
        match self.state.set_trash_view(enabled, &self.storage) {
            Ok(()) => {
                if enabled {
                    self.state.set_status_message(Some(
                        "Trash view: j/k browse • u restore • d delete • T exit",
                    ));
                } else {
                    self.state.set_status_message(Some("Back to active notes"));
                }
            }
            Err(err) => {
                tracing::error!(?err, "failed to toggle trash view");
                self.state
                    .set_status_message(Some("Failed to toggle trash view"));
            }
        }
    }

    fn handle_restore_note(&mut self) {
        if !self.state.show_trash {
            self.state
                .set_status_message(Some("Restore only available in trash view"));
            return;
        }
        match self.state.restore_selected_note(&self.storage) {
            Ok(()) => {
                self.state.set_status_message(Some("Note restored"));
            }
            Err(err) => {
                tracing::error!(?err, "failed to restore note");
                self.state
                    .set_status_message(Some("Failed to restore note"));
            }
        }
    }

    fn submit_new_note(&mut self) {
        let Some(draft) = self.state.new_note_overlay() else {
            return;
        };
        let title = draft.title.trim();
        if title.is_empty() {
            self.state.set_status_message(Some("Title cannot be empty"));
            return;
        }

        match self.storage.create_note(title, "", false) {
            Ok(note_id) => {
                if let Err(err) = self.state.refresh(&self.storage) {
                    tracing::error!(?err, "failed to refresh after note creation");
                    self.state
                        .set_status_message(Some("Note created, refresh failed"));
                } else {
                    self.state.close_overlay();
                    self.state.select_note_by_id(note_id);
                    self.state.set_status_message(Some("Note created"));
                }
            }
            Err(err) => {
                tracing::error!(?err, "failed to create note");
                self.state.set_status_message(Some("Failed to create note"));
            }
        }
    }

    fn submit_rename_note(&mut self) {
        let Some((note_id, title)) = self
            .state
            .rename_note_overlay()
            .map(|draft| (draft.note_id, draft.title.trim().to_string()))
        else {
            return;
        };
        if title.is_empty() {
            self.state.set_status_message(Some("Title cannot be empty"));
            return;
        }
        let dispatcher = actions::ActionDispatcher::new(&self.storage);
        match dispatcher.rename_note(note_id, &title) {
            Ok(()) => {
                self.state.close_overlay();
                match self.state.refresh(&self.storage) {
                    Ok(()) => {
                        self.state.select_note_by_id(note_id);
                        self.state.set_status_message(Some("Note renamed"));
                    }
                    Err(err) => {
                        tracing::error!(?err, "failed to refresh after rename");
                        self.state
                            .set_status_message(Some("Renamed, refresh failed"));
                    }
                }
            }
            Err(err) => {
                tracing::error!(?err, "failed to rename note");
                self.state.set_status_message(Some("Failed to rename note"));
            }
        }
    }

    fn submit_delete_note(&mut self) {
        let Some(draft) = self.state.delete_note_overlay() else {
            return;
        };
        let note_id = draft.note_id;
        let dispatcher = actions::ActionDispatcher::new(&self.storage);
        match dispatcher.soft_delete(note_id) {
            Ok(()) => {
                self.state.close_overlay();
                match self.state.refresh(&self.storage) {
                    Ok(()) => {
                        self.state.set_status_message(Some("Note moved to trash"));
                    }
                    Err(err) => {
                        tracing::error!(?err, "failed to refresh after delete");
                        self.state
                            .set_status_message(Some("Deleted, refresh failed"));
                    }
                }
            }
            Err(err) => {
                tracing::error!(?err, "failed to delete note");
                self.state.set_status_message(Some("Failed to delete note"));
            }
        }
    }

    fn handle_toggle_pin(&mut self) {
        let Some(note_id) = self.state.selected().map(|n| n.id) else {
            return;
        };
        let should_pin = self.state.selected().map(|n| !n.pinned).unwrap_or(true);
        let dispatcher = actions::ActionDispatcher::new(&self.storage);
        if let Err(err) = dispatcher.toggle_pin(note_id, should_pin) {
            tracing::error!(?err, "failed to toggle pin");
            self.state
                .set_status_message(Some("Failed to update pin state"));
            return;
        }
        if let Err(err) = self.state.refresh(&self.storage) {
            tracing::error!(?err, "failed to refresh after pin toggle");
            self.state
                .set_status_message(Some("Could not refresh notes"));
        } else {
            self.state.select_note_by_id(note_id);
            let message = if should_pin {
                "Note pinned"
            } else {
                "Note unpinned"
            };
            self.state.set_status_message(Some(message));
        }
    }

    fn handle_toggle_archive(&mut self) {
        let Some(note_id) = self.state.selected().map(|n| n.id) else {
            return;
        };
        let should_archive = self.state.selected().map(|n| !n.archived).unwrap_or(true);
        let dispatcher = actions::ActionDispatcher::new(&self.storage);
        if let Err(err) = dispatcher.toggle_archive(note_id, should_archive) {
            tracing::error!(?err, "failed to toggle archive");
            self.state
                .set_status_message(Some("Failed to update archive state"));
            return;
        }
        if let Err(err) = self.state.refresh(&self.storage) {
            tracing::error!(?err, "failed to refresh after archive toggle");
            self.state
                .set_status_message(Some("Could not refresh notes"));
        } else if should_archive {
            self.state.set_status_message(Some("Note archived"));
        } else {
            self.state.select_note_by_id(note_id);
            self.state.set_status_message(Some("Note restored"));
        }
    }

    fn handle_rename_note(&mut self) {
        if self.state.overlay().is_some() {
            return;
        }
        if self.state.show_trash {
            self.state
                .set_status_message(Some("Rename unavailable in trash view"));
            return;
        }
        if self.state.selected().is_none() {
            self.state.set_status_message(Some("No note selected"));
            return;
        }
        self.state.open_rename_note();
        self.state.set_status_message(Some(
            "Rename note: type new title • Enter save • Esc cancel",
        ));
    }

    fn handle_delete_note(&mut self) {
        if self.state.overlay().is_some() {
            return;
        }
        if self.state.selected().is_none() {
            self.state.set_status_message(Some("No note selected"));
            return;
        }
        self.state.open_delete_note();
        self.state
            .set_status_message(Some("Delete note: Enter confirm • Esc cancel"));
    }

    fn handle_toggle_regex(&mut self) {
        match self.state.toggle_regex_mode(&self.storage) {
            Ok(enabled) => {
                let message = if enabled {
                    "Regex search enabled"
                } else {
                    "Regex search disabled"
                };
                self.state.set_status_message(Some(message));
            }
            Err(err) => {
                tracing::error!(?err, "failed to toggle regex mode");
                self.state
                    .set_status_message(Some(format!("Regex error: {}", err)));
            }
        }
    }

    fn handle_show_tag_editor(&mut self) {
        if self.state.selected().is_none() {
            self.state.set_status_message(Some("No note selected"));
            return;
        }
        match self.state.open_tag_editor(&self.storage) {
            Ok(()) => {
                self.state.set_status_message(Some(
                    "Tag editor: j/k move • space toggle • a add • Enter save • Esc cancel",
                ));
            }
            Err(err) => {
                tracing::error!(?err, "failed to open tag editor");
                self.state
                    .set_status_message(Some("Failed to open tag editor"));
            }
        }
    }

    fn handle_toggle_wrap(&mut self) {
        let enabled = self.state.toggle_wrap();
        let message = if enabled {
            "Word wrap enabled"
        } else {
            "Word wrap disabled"
        };
        self.state.set_status_message(Some(message));
    }

    fn handle_enter_edit(&mut self) {
        if self.state.is_editing() {
            self.state
                .set_status_message(Some("Already editing; press Esc to exit edit mode"));
            return;
        }
        if self.state.show_trash {
            self.state
                .set_status_message(Some("Cannot edit notes while viewing trash"));
            return;
        }
        let Some(note) = self.state.selected().cloned() else {
            self.state.set_status_message(Some("No note selected"));
            return;
        };
        if let Err(err) = self.start_editing_internal(&note) {
            tracing::error!(?err, note_id = note.id, "failed to enter edit mode");
            self.state
                .set_status_message(Some("Failed to enter edit mode"));
        }
    }

    fn handle_manual_save(&mut self) {
        if !self.state.is_editing() {
            self.state
                .set_status_message(Some("Manual save is only available while editing"));
            return;
        }
        match self.auto_save.flush_now(&self.storage) {
            Ok(Some(event)) => {
                let was_saved = matches!(event, AutoSaveEvent::Saved { .. });
                self.handle_autosave_event(event);
                if was_saved {
                    self.state.set_status_message(Some("Changes saved"));
                }
            }
            Ok(None) => {
                self.state.set_status_message(Some("No changes to save"));
            }
            Err(err) => {
                tracing::error!(?err, "manual save failed");
                self.state
                    .set_status_message(Some("Manual save failed; see logs"));
            }
        }
        self.state.set_autosave_status(self.auto_save.status());
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> bool {
        if !self.state.is_editing() {
            return false;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('s') => {
                    self.handle_manual_save();
                    return true;
                }
                KeyCode::Char('z') => {
                    if self.editor_undo() {
                        self.state.set_status_message(Some("Undid change"));
                    } else {
                        self.state.set_status_message(Some("Nothing to undo"));
                    }
                    return true;
                }
                KeyCode::Char('y') => {
                    if self.editor_redo() {
                        self.state.set_status_message(Some("Redid change"));
                    } else {
                        self.state.set_status_message(Some("Nothing to redo"));
                    }
                    return true;
                }
                KeyCode::Left => {
                    if let Some(editor) = self.state.editor_mut() {
                        editor.move_word_left();
                    }
                    return true;
                }
                KeyCode::Right => {
                    if let Some(editor) = self.state.editor_mut() {
                        editor.move_word_right();
                    }
                    return true;
                }
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc => {
                if self.exit_editing() {
                    self.state.set_status_message(Some("Exited edit mode"));
                }
                true
            }
            KeyCode::Enter => {
                self.apply_editor_change(|editor| editor.insert_newline());
                true
            }
            KeyCode::Backspace => {
                self.apply_editor_change(|editor| editor.backspace());
                true
            }
            KeyCode::Delete => {
                self.apply_editor_change(|editor| editor.delete());
                true
            }
            KeyCode::Tab => {
                self.apply_editor_change(|editor| editor.insert_char('\t'));
                true
            }
            KeyCode::Char(ch)
                if !key.modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                self.apply_editor_change(|editor| editor.insert_char(ch));
                true
            }
            KeyCode::Left => {
                if let Some(editor) = self.state.editor_mut() {
                    editor.move_left();
                }
                true
            }
            KeyCode::Right => {
                if let Some(editor) = self.state.editor_mut() {
                    editor.move_right();
                }
                true
            }
            KeyCode::Up => {
                if let Some(editor) = self.state.editor_mut() {
                    editor.move_up();
                }
                true
            }
            KeyCode::Down => {
                if let Some(editor) = self.state.editor_mut() {
                    editor.move_down();
                }
                true
            }
            KeyCode::Home => {
                if let Some(editor) = self.state.editor_mut() {
                    editor.move_home();
                }
                true
            }
            KeyCode::End => {
                if let Some(editor) = self.state.editor_mut() {
                    editor.move_end();
                }
                true
            }
            _ => false,
        }
    }

    fn editor_undo(&mut self) -> bool {
        let changed = {
            if let Some(editor) = self.state.editor_mut() {
                editor.undo()
            } else {
                false
            }
        };
        if changed {
            self.state.apply_editor_preview();
            self.queue_autosave_update();
        }
        changed
    }

    fn editor_redo(&mut self) -> bool {
        let changed = {
            if let Some(editor) = self.state.editor_mut() {
                editor.redo()
            } else {
                false
            }
        };
        if changed {
            self.state.apply_editor_preview();
            self.queue_autosave_update();
        }
        changed
    }

    fn apply_editor_change<F>(&mut self, f: F) -> bool
    where
        F: FnOnce(&mut EditorState) -> bool,
    {
        let changed = {
            if let Some(editor) = self.state.editor_mut() {
                f(editor)
            } else {
                return false;
            }
        };
        if changed {
            self.state.apply_editor_preview();
            self.queue_autosave_update();
        }
        changed
    }

    fn queue_autosave_update(&mut self) {
        let Some(editor) = self.state.editor() else {
            return;
        };
        let note_id = editor.note_id();
        let body = editor.buffer().to_string();
        if let Err(err) = self.auto_save.update_buffer(note_id, &body) {
            tracing::error!(?err, note_id, "failed to queue autosave update");
            self.state.set_status_message(Some(
                "Failed to queue autosave update; try Ctrl-s to save manually",
            ));
        }
        self.state.set_autosave_status(self.auto_save.status());
    }

    fn start_editing_internal(&mut self, note: &NoteSummary) -> Result<()> {
        let recovered = self
            .auto_save
            .start_session(note.id, &note.body)
            .context("starting autosave session")?;

        let active_body = recovered
            .as_ref()
            .map(|snapshot| snapshot.body.clone())
            .unwrap_or_else(|| note.body.clone());

        self.state.begin_editor(note.id, active_body);
        self.state.focus = FocusPane::Reader;
        if recovered.is_some() {
            if let Some(editor) = self.state.editor_mut() {
                editor.dirty = true;
            }
        }
        self.state.apply_editor_preview();
        if let Some(snapshot) = &recovered {
            let formatted = snapshot
                .saved_at
                .format(&Rfc3339)
                .unwrap_or_else(|_| snapshot.saved_at.unix_timestamp().to_string());
            self.state.set_status_message(Some(format!(
                "Recovered autosave from {}. Esc to exit • Ctrl-s save",
                formatted
            )));
        } else {
            self.state.set_status_message(Some(
                "Editing note: type to modify • Esc exit • Ctrl-s save",
            ));
        }
        self.recovery_snapshots
            .retain(|entry| entry.note_id != note.id);
        self.state.set_autosave_status(self.auto_save.status());
        Ok(())
    }

    fn exit_editing(&mut self) -> bool {
        let Some(note_id) = self.editing_note_id() else {
            return true;
        };

        if self.state.editor_dirty() {
            match self.auto_save.flush_now(&self.storage) {
                Ok(Some(event)) => {
                    if matches!(event, AutoSaveEvent::Error { .. }) {
                        self.handle_autosave_event(event);
                        return false;
                    }
                    self.handle_autosave_event(event);
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::error!(?err, "autosave flush before exit failed");
                    self.state
                        .set_status_message(Some("Failed to save changes; still in edit mode"));
                    return false;
                }
            }
        }

        if let Err(err) = self.auto_save.end_session(note_id, true) {
            tracing::warn!(?err, note_id, "failed to end autosave session");
        }
        self.state.close_editor();
        self.state.set_autosave_status(self.auto_save.status());
        true
    }

    fn handle_autosave_event(&mut self, event: AutoSaveEvent) {
        match event {
            AutoSaveEvent::Saved { note_id, timestamp } => {
                self.state.on_autosave_saved(note_id, timestamp);
            }
            AutoSaveEvent::Error { note_id, message } => {
                tracing::warn!(note_id, %message, "autosave error");
                self.state.set_status_message(Some(format!(
                    "Autosave error for note #{note_id}: {message}"
                )));
            }
        }
        self.state.set_autosave_status(self.auto_save.status());
    }

    fn editing_note_id(&self) -> Option<i64> {
        self.state.editor().map(|editor| editor.note_id())
    }

    fn apply_tag_editor_changes(&mut self) {
        let Some((add, remove, note_id)) = self.state.tag_editor_changes() else {
            return;
        };
        if add.is_empty() && remove.is_empty() {
            self.state.close_overlay();
            self.state.set_status_message(Some("No tag changes"));
            return;
        }

        let dispatcher = actions::ActionDispatcher::new(&self.storage);
        for tag in &add {
            if let Err(err) = dispatcher.add_tag(note_id, tag) {
                tracing::error!(?err, "failed to add tag");
                self.state
                    .set_status_message(Some(format!("Failed to add tag '{tag}'")));
                return;
            }
        }
        for tag in &remove {
            if let Err(err) = dispatcher.remove_tag(note_id, tag) {
                tracing::error!(?err, "failed to remove tag");
                self.state
                    .set_status_message(Some(format!("Failed to remove tag '{tag}'")));
                return;
            }
        }

        self.state.close_overlay();
        match self.state.refresh(&self.storage) {
            Ok(()) => {
                self.state.select_note_by_id(note_id);
                self.state.set_status_message(Some("Tags updated"));
            }
            Err(err) => {
                tracing::error!(?err, "failed to refresh after tag edit");
                self.state
                    .set_status_message(Some("Tags updated, refresh failed"));
            }
        }
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enabling raw mode")?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .context("switching to alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating terminal backend")?;
    terminal.hide_cursor().context("hiding cursor")?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    terminal.show_cursor().ok();
    disable_raw_mode().context("disabling raw mode")?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .context("restoring screen state")?;
    Ok(())
}
