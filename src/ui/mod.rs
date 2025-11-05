use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Frame;
use time::{macros::format_description, OffsetDateTime};

use regex::{Regex, RegexBuilder};

use crate::app::state::{AppState, FocusPane, OverlayState, TagEditorMode};
use crate::journaling::AutoSaveStatus;

pub fn draw_app(frame: &mut Frame, state: &AppState, list_state: &mut ListState) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(2)])
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
        let meta_line = Line::from(Span::styled(
            format!("Updated {}", note.updated_at),
            Style::default().fg(Color::Gray),
        ));
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
        items.push(ListItem::new("No notes yet. Press `a` to create one."));
    }

    let list = List::new(items)
        .block(
            Block::default()
                .title("Notes")
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
    frame.render_widget(detail, columns[1]);

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

    let mut keys_line = Vec::new();
    keys_line.push(Span::styled(
        "Keys: ",
        Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::BOLD),
    ));
    keys_line.push(Span::styled(
        "j/k move • Tab focus • / search • Shift+R regex • a add • p pin • A archive • e edit • Ctrl-s save • Ctrl-z undo • Ctrl-y redo • Ctrl-←/→ word jump • Shift+W wrap • d delete • T trash view • q quit",
        Style::default().fg(Color::DarkGray),
    ));
    lines.push(Line::from(keys_line));

    Text::from(lines)
}

fn format_time_short(dt: OffsetDateTime) -> String {
    dt.format(&format_description!("[hour]:[minute]:[second]"))
        .unwrap_or_else(|_| dt.unix_timestamp().to_string())
}

fn build_highlight_regex(tokens: &[String]) -> Option<Regex> {
    if tokens.is_empty() {
        return None;
    }
    let pattern = tokens
        .iter()
        .filter(|token| !token.is_empty())
        .map(|token| regex::escape(token))
        .collect::<Vec<_>>()
        .join("|");
    if pattern.is_empty() {
        return None;
    }
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()
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

            let instructions = match editor.mode {
                TagEditorMode::Browse => "Space toggles • a add tag • Enter save • Esc cancel",
                TagEditorMode::Adding => "Type tag name • Enter confirm • Esc cancel",
            };

            let header = Paragraph::new(vec![
                Line::from(Span::styled(
                    "Tag Editor",
                    Style::default().add_modifier(Modifier::BOLD),
                )),
                Line::from(Span::styled(instructions, Style::default().fg(Color::Gray))),
            ])
            .block(
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
                    let style = if item.original {
                        Style::default()
                    } else {
                        Style::default().fg(Color::Cyan)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(mark.to_string(), style.add_modifier(Modifier::BOLD)),
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

            let input_para = match editor.mode {
                TagEditorMode::Adding => {
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
