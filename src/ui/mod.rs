use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use time::{macros::format_description, OffsetDateTime};
use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

use regex::Regex;

use crate::app::state::{
    AppState, BulkTrashAction, EditorState, FocusPane, NoteSummary, OverlayState, TagEditorMode,
    TagInputKind,
};
use crate::highlight::build_highlight_regex;
use crate::journaling::AutoSaveStatus;

pub fn draw_app(frame: &mut Frame, state: &AppState, list_state: &mut ListState) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(4)])
        .split(frame.size());

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(40), Constraint::Percentage(60)])
        .split(vertical[0]);

    let list_block_style = if matches!(state.focus, FocusPane::List) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let tokens = state.search_tokens();
    let highlight_regex = build_highlight_regex(&tokens);
    let highlight_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let mut items = Vec::with_capacity(state.notes.len());
    for note in &state.notes {
        let mut title_spans = Vec::new();
        let is_editing = state
            .editor()
            .map(|editor| editor.note_id() == note.id)
            .unwrap_or(false);
        let editing_dirty = is_editing && state.editor_dirty();
        if is_editing {
            let label = if editing_dirty { "✎* " } else { "✎ " };
            title_spans.push(Span::styled(
                label,
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if note.pinned {
            title_spans.push(Span::styled(
                "★ ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if note.archived {
            title_spans.push(Span::styled(
                "[A] ",
                Style::default()
                    .fg(Color::Gray)
                    .add_modifier(Modifier::ITALIC),
            ));
        }
        title_spans.extend(highlight_line(
            &note.title,
            highlight_regex.as_ref(),
            highlight_style,
            Style::default().add_modifier(Modifier::BOLD),
        ));
        let title_line = Line::from(title_spans);
        let meta_line = if state.show_trash {
            let mut spans = Vec::new();
            let deleted_style = Style::default().fg(Color::Gray);
            let deleted_label = note
                .deleted_label
                .as_deref()
                .map(|label| format!("Deleted {}", label))
                .unwrap_or_else(|| "Deleted — unknown time".to_string());
            spans.push(Span::styled(deleted_label, deleted_style));
            if let Some(status) = &note.trash_status {
                spans.push(Span::raw(" • "));
                let status_style = if status.expired {
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD | Modifier::ITALIC)
                } else if status.indefinite {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::ITALIC)
                } else {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC)
                };
                spans.push(Span::styled(status.label.clone(), status_style));
            }
            Line::from(spans)
        } else {
            Line::from(Span::styled(
                format!("Updated {}", note.updated_at),
                Style::default().fg(Color::Gray),
            ))
        };
        let mut preview_lines = Vec::new();
        for line in note.preview.lines() {
            preview_lines.push(Line::from(highlight_line(
                line,
                highlight_regex.as_ref(),
                highlight_style,
                Style::default(),
            )));
        }
        if preview_lines.is_empty() {
            preview_lines.push(Line::from(""));
        }
        let mut lines = Vec::with_capacity(2 + preview_lines.len());
        lines.push(title_line);
        lines.push(meta_line);
        if let Some(tag_line) =
            render_tag_line(&note.tags, highlight_regex.as_ref(), highlight_style)
        {
            lines.push(tag_line);
        }
        lines.extend(preview_lines);
        items.push(ListItem::new(lines));
    }
    if items.is_empty() {
        if state.show_trash {
            items.push(ListItem::new("Trash is empty."));
        } else {
            items.push(ListItem::new("No notes yet. Press `a` to create one."));
        }
    }

    let list_title = if state.show_trash { "Trash" } else { "Notes" };
    let list = List::new(items)
        .block(
            Block::default()
                .title(list_title)
                .borders(Borders::ALL)
                .border_style(list_block_style),
        )
        .highlight_style(
            Style::default()
                .bg(Color::Blue)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▸ ");
    frame.render_stateful_widget(list, columns[0], list_state);

    let detail_block_style = if matches!(state.focus, FocusPane::Reader) {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    };

    let preview_text: Text = state
        .selected()
        .map(|note| {
            let mut lines = Vec::new();
            let mut header_spans = Vec::new();
            let editing_this_note = state
                .editor()
                .map(|editor| editor.note_id() == note.id)
                .unwrap_or(false);
            let editor_dirty = editing_this_note && state.editor_dirty();
            if editing_this_note {
                let label = if editor_dirty { "[EDIT*] " } else { "[EDIT] " };
                header_spans.push(Span::styled(
                    label,
                    Style::default()
                        .fg(Color::Magenta)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if note.pinned {
                header_spans.push(Span::styled(
                    "★ ",
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            if note.archived {
                header_spans.push(Span::styled(
                    "[A] ",
                    Style::default()
                        .fg(Color::Gray)
                        .add_modifier(Modifier::ITALIC),
                ));
            }
            header_spans.extend(highlight_line(
                &note.title,
                highlight_regex.as_ref(),
                highlight_style,
                Style::default().add_modifier(Modifier::BOLD),
            ));
            lines.push(Line::from(header_spans));
            let updated_label = if editing_this_note && editor_dirty {
                format!("Updated {} (unsaved)", note.updated_at)
            } else {
                format!("Updated {}", note.updated_at)
            };
            lines.push(Line::from(Span::styled(
                updated_label,
                Style::default().fg(Color::Gray),
            )));
            if let Some(tag_line) =
                render_tag_line(&note.tags, highlight_regex.as_ref(), highlight_style)
            {
                lines.push(tag_line);
            }
            lines.push(Line::from(""));
            let body_text = if editing_this_note {
                state.editor_buffer().unwrap_or(note.body.as_str())
            } else {
                note.body.as_str()
            };
            lines.extend(highlight_body(
                body_text,
                highlight_regex.as_ref(),
                highlight_style,
            ));
            Text::from(lines)
        })
        .unwrap_or_else(|| Text::from("Select a note to see its contents."));

    let mut detail = Paragraph::new(preview_text).block(
        Block::default()
            .title("Preview")
            .borders(Borders::ALL)
            .border_style(detail_block_style),
    );
    if state.wrap_enabled() {
        detail = detail.wrap(Wrap { trim: false });
    }
    frame.render_widget(Clear, columns[1]);
    frame.render_widget(detail, columns[1]);
    if let (Some(note), Some(editor)) = (state.selected(), state.editor()) {
        if editor.note_id() == note.id {
            if let Some((cursor_x, cursor_y)) =
                editor_cursor_screen_position(editor, note, columns[1], state.wrap_enabled())
            {
                frame.set_cursor(cursor_x, cursor_y);
            }
        }
    }

    let status = build_status_line(state);
    let status_paragraph = Paragraph::new(status).style(Style::default().fg(Color::Gray));
    frame.render_widget(status_paragraph, vertical[1]);

    render_overlay(frame, state);
}

fn build_status_line(state: &AppState) -> Text<'static> {
    let total = state.len();
    let position = if state.is_empty() {
        "0/0".to_string()
    } else {
        format!("{}/{}", state.selected + 1, total)
    };
    let focus = match state.focus {
        FocusPane::List => "List",
        FocusPane::Reader => "Reader",
    };

    let mut spans = vec![
        Span::raw(format!("Total: {total} ")),
        Span::raw(" | Selected: "),
        Span::styled(position, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" | Focus: "),
        Span::styled(focus, Style::default().add_modifier(Modifier::BOLD)),
    ];

    if state.show_trash {
        spans.push(Span::raw(" | View: "));
        spans.push(Span::styled(
            "Trash",
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::BOLD | Modifier::ITALIC),
        ));
        if let Some(note) = state.selected() {
            spans.push(Span::raw(" | Purge: "));
            let style = note.trash_status.as_ref().map(|status| {
                if status.expired {
                    Style::default()
                        .fg(Color::Red)
                        .add_modifier(Modifier::BOLD | Modifier::ITALIC)
                } else if status.indefinite {
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::ITALIC)
                } else {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::ITALIC)
                }
            });
            if let Some(label_style) = style {
                spans.push(Span::styled(
                    note.trash_status
                        .as_ref()
                        .map(|status| status.label.clone())
                        .unwrap_or_else(|| "n/a".into()),
                    label_style,
                ));
            } else {
                spans.push(Span::styled("n/a", Style::default().fg(Color::Gray)));
            }
        }
    }

    let tokens = state.search_tokens();
    if state.is_search_active()
        || !tokens.is_empty()
        || !state.search_filter_chips().is_empty()
        || state.is_regex_enabled()
    {
        let label_style = if state.is_search_active() {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::raw(" | Search "));
        spans.push(Span::styled("/", label_style));
        if tokens.is_empty() && state.search_query().is_empty() {
            spans.push(Span::styled(
                "(type to search)",
                Style::default().fg(Color::DarkGray),
            ));
        } else {
            spans.push(Span::styled(
                state.search_query().to_string(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
        if state.is_search_active() {
            spans.push(Span::styled(" ▌", Style::default().fg(Color::Cyan)));
        }
        if state.is_regex_enabled() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                "[regex]",
                Style::default()
                    .fg(Color::Magenta)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        for chip in state.search_filter_chips() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("[{chip}]"),
                Style::default().fg(Color::Green),
            ));
        }
        if let Some(error) = state.search_error() {
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                format!("! {error}"),
                Style::default().fg(Color::Red),
            ));
        }
    }

    if state.is_editing() {
        spans.push(Span::raw(" | Mode: "));
        let edit_label = if state.editor_dirty() {
            "EDIT*"
        } else {
            "EDIT"
        };
        spans.push(Span::styled(
            edit_label,
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }

    spans.push(Span::raw(" | Wrap: "));
    spans.push(Span::styled(
        if state.wrap_enabled() { "on" } else { "off" },
        Style::default().fg(Color::Gray),
    ));

    match state.autosave_status() {
        AutoSaveStatus::Disabled => {
            spans.push(Span::raw(" | Autosave: disabled"));
        }
        AutoSaveStatus::Inactive => {
            spans.push(Span::raw(" | Autosave: idle"));
        }
        AutoSaveStatus::Idle { last_saved_at, .. } => {
            spans.push(Span::raw(" | Autosave: saved"));
            if let Some(ts) = last_saved_at {
                spans.push(Span::raw(" "));
                spans.push(Span::styled(
                    format_time_short(*ts),
                    Style::default().fg(Color::Gray),
                ));
            }
        }
        AutoSaveStatus::Pending { since, .. } => {
            spans.push(Span::raw(" | Autosave: "));
            spans.push(Span::styled(
                "pending",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" since "));
            spans.push(Span::styled(
                format_time_short(*since),
                Style::default().fg(Color::Gray),
            ));
        }
        AutoSaveStatus::Error { message, .. } => {
            spans.push(Span::raw(" | Autosave: "));
            spans.push(Span::styled(
                format!("error ({message})"),
                Style::default().fg(Color::Red),
            ));
        }
    }

    if let Some(message) = &state.status_message {
        spans.push(Span::raw(" | "));
        spans.push(Span::styled(
            message.clone(),
            Style::default().fg(Color::Cyan),
        ));
    }

    let mut lines = Vec::with_capacity(2);
    lines.push(Line::from(spans));

    let mut keys_line1 = Vec::new();
    keys_line1.push(Span::styled(
        "Keys: ",
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    ));
    keys_line1.push(Span::styled(
        "j/k move • Tab focus • / search • Shift+R regex • a add • p pin • A archive",
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::from(keys_line1));

    let mut keys_line2 = Vec::new();
    keys_line2.push(Span::styled(
        "      e edit • Ctrl-s save • Ctrl-z undo • Ctrl-y redo • Ctrl-←/→ word jump",
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::from(keys_line2));

    let mut keys_line3 = Vec::new();
    keys_line3.push(Span::styled(
        "      Shift+W wrap • d delete • T trash view • q quit",
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::from(keys_line3));

    Text::from(lines)
}

fn format_time_short(dt: OffsetDateTime) -> String {
    dt.format(&format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| dt.unix_timestamp().to_string())
}

fn highlight_line(
    text: &str,
    regex: Option<&Regex>,
    highlight_style: Style,
    base_style: Style,
) -> Vec<Span<'static>> {
    if let Some(re) = regex {
        let mut spans = Vec::new();
        let mut last = 0;
        for mat in re.find_iter(text) {
            if mat.start() > last {
                spans.push(Span::styled(
                    text[last..mat.start()].to_string(),
                    base_style,
                ));
            }
            spans.push(Span::styled(mat.as_str().to_string(), highlight_style));
            last = mat.end();
        }
        if last < text.len() {
            spans.push(Span::styled(text[last..].to_string(), base_style));
        }
        if spans.is_empty() {
            spans.push(Span::styled(text.to_string(), base_style));
        }
        spans
    } else {
        vec![Span::styled(text.to_string(), base_style)]
    }
}

fn highlight_body(body: &str, regex: Option<&Regex>, highlight_style: Style) -> Vec<Line<'static>> {
    if body.is_empty() {
        return vec![Line::from("")];
    }
    body.lines()
        .map(|line| {
            Line::from(highlight_line(
                line,
                regex,
                highlight_style,
                Style::default(),
            ))
        })
        .collect()
}

fn editor_cursor_screen_position(
    editor: &EditorState,
    note: &NoteSummary,
    area: Rect,
    wrap_enabled: bool,
) -> Option<(u16, u16)> {
    let inner_width = area.width.saturating_sub(2);
    let inner_height = area.height.saturating_sub(2);
    if inner_width == 0 || inner_height == 0 {
        return None;
    }

    let mut row = preview_body_offset(note);
    let mut col = 0usize;
    let width_limit = inner_width as usize;
    let buffer = editor.buffer();
    let cursor = editor.cursor().min(buffer.len());

    for grapheme in buffer[..cursor].graphemes(true) {
        if grapheme == "\n" {
            row += 1;
            col = 0;
            continue;
        }
        let glyph_width = UnicodeWidthStr::width(grapheme);
        if wrap_enabled && glyph_width > 0 && col + glyph_width > width_limit {
            row += 1;
            col = 0;
        }
        col += glyph_width;
    }

    let max_row = inner_height;
    let row = row.min(max_row);
    let limit = width_limit.max(1);
    let col = col.min(limit - 1) as u16;

    let cursor_x = area.x + 1 + col;
    let cursor_y = area.y + 1 + row;
    Some((cursor_x, cursor_y))
}

fn preview_body_offset(note: &NoteSummary) -> u16 {
    let mut offset = 3; // header, meta, blank line
    if !note.tags.is_empty() {
        offset += 1;
    }
    offset
}

fn render_tag_line(
    tags: &[String],
    regex: Option<&Regex>,
    highlight_style: Style,
) -> Option<Line<'static>> {
    if tags.is_empty() {
        return None;
    }
    let base_style = Style::default().fg(Color::Green);
    let mut spans = Vec::new();
    for (idx, tag) in tags.iter().enumerate() {
        let token = format!("#{tag}");
        spans.extend(highlight_line(&token, regex, highlight_style, base_style));
        if idx + 1 < tags.len() {
            spans.push(Span::raw(" "));
        }
    }
    Some(Line::from(spans))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::highlight::build_highlight_regex;
    use ratatui::style::Style;
    use ratatui::text::Span;

    fn span_texts(spans: &[Span<'static>]) -> Vec<String> {
        spans
            .iter()
            .map(|span| span.content.clone().into_owned())
            .collect()
    }

    #[test]
    fn highlight_regex_prefers_longer_tokens_first() {
        let regex = build_highlight_regex(&["not".into(), "note".into()]).expect("regex");
        let spans = highlight_line("notebook", Some(&regex), Style::default(), Style::default());
        assert_eq!(
            span_texts(&spans),
            vec![String::from("note"), String::from("book")]
        );
    }

    #[test]
    fn highlight_regex_deduplicates_case_insensitive_tokens() {
        let regex =
            build_highlight_regex(&["Note".into(), "note".into(), "NOTE".into()]).expect("regex");
        let spans = highlight_line("note", Some(&regex), Style::default(), Style::default());
        assert_eq!(span_texts(&spans), vec![String::from("note")]);
    }
}

fn render_overlay(frame: &mut Frame, state: &AppState) {
    match state.overlay() {
        Some(OverlayState::NewNote(draft)) => {
            let area = centered_rect(60, 30, frame.size());
            frame.render_widget(Clear, area);
            let mut title_display = draft.title.clone();
            title_display.push('▌');
            let paragraph = Paragraph::new(vec![
                Line::from(Span::styled(
                    "Create New Note",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(title_display),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter to save • Esc to cancel",
                    Style::default().fg(Color::Gray),
                )),
            ])
            .block(
                Block::default()
                    .title("New Note")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        Some(OverlayState::RenameNote(draft)) => {
            let area = centered_rect(60, 30, frame.size());
            frame.render_widget(Clear, area);
            let mut title_display = draft.title.clone();
            title_display.push('▌');
            let paragraph = Paragraph::new(vec![
                Line::from(Span::styled(
                    "Rename Note",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(
                    format!("Note #{}, current title:", draft.note_id),
                    Style::default().fg(Color::Gray),
                )),
                Line::from(""),
                Line::from(title_display),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter to save • Esc to cancel",
                    Style::default().fg(Color::Gray),
                )),
            ])
            .block(
                Block::default()
                    .title("Rename Note")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        Some(OverlayState::DeleteNote(draft)) => {
            let area = centered_rect(60, 30, frame.size());
            frame.render_widget(Clear, area);
            let paragraph = Paragraph::new(vec![
                Line::from(Span::styled(
                    "Delete Note",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("Are you sure you want to move '#{}' to trash?", draft.title),
                    Style::default(),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "Enter to confirm • Esc to cancel",
                    Style::default().fg(Color::Gray),
                )),
            ])
            .block(
                Block::default()
                    .title(format!("Confirm Delete (#{})", draft.note_id))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Red)),
            )
            .wrap(Wrap { trim: false });
            frame.render_widget(paragraph, area);
        }
        Some(OverlayState::BulkTrash(dialog)) => {
            let (title, body_lines, accent) = match dialog.action {
                BulkTrashAction::RestoreAll => (
                    "Restore All Notes",
                    vec![
                        Line::from(Span::styled(
                            "Restore every note from the trash?",
                            Style::default().add_modifier(Modifier::BOLD),
                        )),
                        Line::from(""),
                        Line::from("Enter or y restore • Esc cancel"),
                    ],
                    Color::Green,
                ),
                BulkTrashAction::PurgeAll => (
                    "Purge Trash",
                    vec![
                        Line::from(Span::styled(
                            "Permanently delete every trashed note?",
                            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
                        )),
                        Line::from(Span::styled(
                            "This cannot be undone.",
                            Style::default().fg(Color::Red),
                        )),
                        Line::from(""),
                        Line::from(Span::styled(
                            "Enter or y purge • Esc cancel",
                            Style::default().fg(Color::Red),
                        )),
                    ],
                    Color::Red,
                ),
            };
            let area = centered_rect(50, 30, frame.size());
            frame.render_widget(Clear, area);
            let paragraph = Paragraph::new(body_lines).block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(accent)),
            );
            frame.render_widget(paragraph, area);
        }
        Some(OverlayState::TagEditor(editor)) => {
            let area = centered_rect(60, 65, frame.size());
            frame.render_widget(Clear, area);

            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints(
                    [
                        Constraint::Length(3),
                        Constraint::Min(5),
                        Constraint::Length(3),
                        Constraint::Length(1),
                    ]
                    .as_ref(),
                )
                .split(area);

            let instructions = match &editor.mode {
                TagEditorMode::Browse => {
                    "Space toggle • v mark • a add • r rename • m merge • M merge marks • x delete • Enter save • Esc close"
                }
                TagEditorMode::Input(TagInputKind::Add) => {
                    "Type tag name • Enter confirm • Esc cancel"
                }
                TagEditorMode::Input(TagInputKind::Rename { .. }) => {
                    "Rename tag • Enter apply • Esc cancel"
                }
                TagEditorMode::Input(TagInputKind::Merge { .. }) => {
                    "Merge into existing tag • Enter merge • Esc cancel"
                }
                TagEditorMode::ConfirmDelete { .. } => "Delete tag • y confirm • n / Esc cancel",
            };

            let mut header_lines = vec![
                Line::from(Span::styled(
                    "Tag Editor",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(instructions, Style::default().fg(Color::Gray))),
            ];

            if !editor.suggestions.is_empty() {
                let chips = editor
                    .suggestions
                    .iter()
                    .enumerate()
                    .take(9)
                    .map(|(idx, tag)| format!("{}:{tag}", idx + 1))
                    .collect::<Vec<_>>()
                    .join("  ");
                header_lines.push(Line::from(Span::styled(
                    format!("Suggestions (1-9): {chips}"),
                    Style::default().fg(Color::Yellow),
                )));
            }

            let header = Paragraph::new(header_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );
            frame.render_widget(header, layout[0]);

            let items: Vec<ListItem> = editor
                .items
                .iter()
                .map(|item| {
                    let mark = if item.selected { "[x]" } else { "[ ]" };
                    let bulk = if item.bulk_selected { "*" } else { " " };
                    let style = if item.original {
                        Style::default()
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(mark.to_string(), style.add_modifier(Modifier::BOLD)),
                        Span::raw(" "),
                        Span::styled(
                            bulk.to_string(),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(item.name.clone(), style),
                    ]))
                })
                .collect();

            let mut list_state = ListState::default();
            if !editor.items.is_empty() {
                list_state.select(Some(editor.selected_index));
            }
            let list = List::new(items)
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_style(Style::default()),
                )
                .highlight_style(
                    Style::default()
                        .bg(Color::Blue)
                        .fg(Color::Black)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▸ ");
            frame.render_stateful_widget(list, layout[1], &mut list_state);

            let input_para = match &editor.mode {
                TagEditorMode::Input(TagInputKind::Add) => {
                    let mut display = editor.input.clone();
                    display.push('▌');
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            format!("New tag for note #{}", editor.note_id),
                            Style::default().fg(Color::Cyan),
                        )),
                        Line::from(display),
                    ])
                }
                TagEditorMode::Input(TagInputKind::Rename { original }) => {
                    let mut display = editor.input.clone();
                    display.push('▌');
                    Paragraph::new(vec![
                        Line::from(Span::styled(
                            format!("Renaming '{}'", original),
                            Style::default().fg(Color::Cyan),
                        )),
                        Line::from(display),
                    ])
                }
                TagEditorMode::Input(TagInputKind::Merge { sources }) => {
                    let mut display = editor.input.clone();
                    display.push('▌');
                    let label = if sources.len() == 1 {
                        format!("Merge '{}' into tag:", sources[0])
                    } else {
                        format!("Merge {} tags into tag:", sources.len())
                    };
                    Paragraph::new(vec![
                        Line::from(Span::styled(label, Style::default().fg(Color::Cyan))),
                        Line::from(display),
                    ])
                }
                TagEditorMode::ConfirmDelete { tag } => Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("Delete tag '{}'", tag),
                        Style::default().fg(Color::Red),
                    )),
                    Line::from("Press y to confirm or n / Esc to cancel"),
                ]),
                TagEditorMode::Browse => Paragraph::new(vec![
                    Line::from(Span::styled(
                        format!("Editing tags for note #{}", editor.note_id),
                        Style::default().fg(Color::Cyan),
                    )),
                    Line::from(""),
                ]),
            }
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            );
            frame.render_widget(input_para, layout[2]);

            let status_line = editor
                .status
                .as_ref()
                .map(|msg| Span::styled(msg.clone(), Style::default().fg(Color::Yellow)))
                .unwrap_or_else(|| Span::raw(" "));
            frame.render_widget(Paragraph::new(Line::from(status_line)), layout[3]);
        }
        Some(OverlayState::Recovery(overlay)) => {
            let area = centered_rect(70, 60, frame.size());
            frame.render_widget(Clear, area);

            let mut lines = Vec::new();
            lines.push(Line::from(Span::styled(
                "Recovered Drafts",
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(Span::styled(
                "Enter restore • d discard • D discard all • j/k move • Esc close",
                Style::default().fg(Color::Gray),
            )));
            lines.push(Line::from(""));

            if overlay.entries.is_empty() {
                lines.push(Line::from("No autosave drafts."));
            } else {
                for (idx, entry) in overlay.entries.iter().enumerate() {
                    let marker = if idx == overlay.selected { "➤" } else { "  " };
                    let mut spans = Vec::new();
                    spans.push(Span::styled(
                        marker,
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ));
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(
                        format!("Note #{}", entry.note_id),
                        if entry.missing {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default().add_modifier(Modifier::BOLD)
                        },
                    ));
                    spans.push(Span::raw("  "));
                    spans.push(Span::styled(
                        &entry.title,
                        if entry.missing {
                            Style::default().fg(Color::Yellow)
                        } else {
                            Style::default()
                        },
                    ));
                    spans.push(Span::raw("  "));
                    if entry.missing {
                        spans.push(Span::styled(
                            "[missing]",
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ));
                        spans.push(Span::raw("  "));
                    }
                    spans.push(Span::styled(
                        entry.saved_relative.clone(),
                        Style::default().fg(Color::Cyan),
                    ));
                    spans.push(Span::raw(" ("));
                    spans.push(Span::styled(
                        entry.saved_at.clone(),
                        Style::default().fg(Color::Gray),
                    ));
                    spans.push(Span::raw(")"));
                    lines.push(Line::from(spans));

                    for preview in &entry.preview {
                        lines.push(Line::from(Span::styled(
                            format!("    {}", preview),
                            Style::default().fg(Color::DarkGray),
                        )));
                    }
                    lines.push(Line::from(""));
                }
            }

            let paragraph = Paragraph::new(lines).block(
                Block::default()
                    .title("Autosave Recovery")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Magenta)),
            );
            frame.render_widget(paragraph, area);
        }
        None => {}
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints(
            [
                Constraint::Percentage((100 - percent_y) / 2),
                Constraint::Percentage(percent_y),
                Constraint::Percentage((100 - percent_y) / 2),
            ]
            .as_ref(),
        )
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage((100 - percent_x) / 2),
                Constraint::Percentage(percent_x),
                Constraint::Percentage((100 - percent_x) / 2),
            ]
            .as_ref(),
        )
        .split(vertical[1])[1]
}
