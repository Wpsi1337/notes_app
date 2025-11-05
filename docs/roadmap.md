# Implementation Roadmap

## Completed in this iteration

- Bootstrapped the Rust workspace (`notes-tui` crate) with `cargo check` passing.
- Added architecture overview and README scaffolding.
- Implemented configuration loader that respects XDG directories and environment overrides.
- Provisioned SQLite storage layer with schema migrations, WAL tuning, and first-run seed data.
- Stubbed CLI interface with `tui`, `new`, and `search` commands (the latter return placeholder behaviour for now).
- Introduced the initial TUI event loop with a two-pane layout, list navigation, and focus toggling.
- Added live search mode that filters notes as you type with inline status feedback.
- Wired the first note action dispatcher (pin/unpin) with UI indicators and keyboard shortcuts.
- Added archive/unarchive handling via the dispatcher with status cues in the UI.
- Added CLI tag management commands for adding, removing, and listing note tags.
- Implemented in-app quick-create modal (press `a`) for adding new notes without leaving the TUI.
- Upgraded search flow with a structured parser, tag/date filtering, and FTS-backed snippets in the list preview.
- Wired the `notetui search` command into the structured search engine with snippet output and filter support.
- Added regression tests for the CLI search pipeline (tag filters, regex) and surfaced regex/filter chips inside the TUI status bar.
- Built the in-app tag editor overlay (press `t`) with toggles, new-tag entry, and live persistence via the dispatcher.
- Added inline note renaming overlay (`r`) wired to storage refresh and status messaging.
- Implemented soft delete with confirmation (`d`) plus trash view/restore (`T`/`u`) while keeping WAL triggers intact.
- Delivered edit mode with autosave + crash-recovery journaling, manual save (`Ctrl-s`), and live status indicators.
- Layered undo/redo history, word-jump navigation, and a wrap toggle into the editor with corresponding shortcuts and tests.

## Near-term milestones

1. **Recovery UX**: surface autosave snapshots at launch with restore/discard workflows and clearer crash-recovery messaging.
2. **Tag management UI**: add rename/merge/delete flows and a proper tag editor (modal/palette) backed by the dispatcher.
3. **Trash lifecycle follow-ups**: expose retention countdowns, add purge-all/restore-all commands, and thread trash filters into the command palette once it lands.
4. **Search & ranking polish**: enrich highlight spans (title/body/tag), experiment with BM25 tuning, and add fallbacks when FTS omits regex-filtered rows.
5. **Integration & snapshot tests**: drive the TUI with scripted key events to cover quick-create, search filters, trash restore, autosave recovery, and editor flows.
6. **Command palette & bulk actions**: surface dispatcher operations (archive, tag edits, purge) through a discoverable palette with fuzzy matching.

## Longer-term targets

- Full keyboard mapping framework (vim/emacs/custom) with run-time reload from config edits.
- Command palette, confirmation dialogs, and toast subsystem.
- Markdown rendering polish with theme-aware styling and accessibility checks.
- Backup/sync hooks plus optional export/import tooling.
