# Notes TUI

Notes TUI is a fast, keyboard-first note-taking application for the terminal. It pairs a responsive `ratatui` interface with an SQLite/FTS5 storage engine so that searching and switching between thousands of notes remains instantaneous even over SSH or inside tmux.

## Status

🚧 Work in progress. This repository currently contains the project scaffolding, architectural plan, and supporting infrastructure to begin implementing the core experience.

## Planned highlights

- Two-pane layout (note list + reader/editor) with smooth virtualised scrolling.
- Global fuzzy search across titles, tags, and note bodies powered by SQLite FTS5.
- Vim-flavoured keyboard shortcuts (with Emacs and custom profiles).
- Resilient storage: WAL-mode SQLite, crash recovery journal, configurable backups.
- Rich tagging workflow: autocomplete, multi-select filters, rename/merge operations.
- Clean XDG integration for config, data, cache, and log directories.
- Headless CLI tools for automation (`notetui new`, `notetui search`, …).

## CLI snippets

- `notetui new "Title"` — create a pinned note using stdin for the body.
- `notetui search tag:project created:2024-01-01..` — search notes tagged `project` updated this year.
- `notetui tag add 42 urgent` — attach the `urgent` tag to note `#42`.
- `notetui tag remove 42 urgent` — detach the tag.
- `notetui tag list 42` — print the tags assigned to the note.

## TUI shortcuts

- `a` — open quick-create modal (type a title, press Enter to save).
- `/` — start search; Esc cancels.
- `Shift+R` — toggle regex mode for the current search.
- `p` — toggle pin; `Shift+A` — toggle archive.
- `t` — open tag editor (space toggles, `a` adds, Enter saves).
- `r` — rename selected note (active view).
- `e` — enter edit mode for the focused note (Esc exits, `Ctrl-s` saves immediately).
- `d` — move selected note to trash (with confirmation).
- `T` — toggle trash view; inside trash use `u` to restore, `Shift+U` to restore all, and `Shift+P` to purge all trashed notes.
- `Ctrl-r` — refresh from storage.
- `Ctrl-z` / `Ctrl-y` — undo / redo while editing.
- `Ctrl-←` / `Ctrl-→` — jump by words while editing.
- `Shift+W` — toggle word wrap for the preview/editor pane.

Autosave is enabled by default with crash recovery snapshots written under `~/.local/state/notetui/autosave/`. The status bar shows when a save is pending, complete, or has encountered an error. If the app detects leftover autosave drafts on launch, it opens a recovery dialog where you can restore (`Enter`) or discard (`d` / `D`) each snapshot.

The trash view surfaces a countdown until each note is purged based on the `retention_days` setting in your config. Set `retention_days = 0` to disable automatic purging and rely solely on the bulk purge command.

For deeper detail, see [`docs/architecture.md`](docs/architecture.md).

## Building

```bash
cargo build
```

By default the build uses SQLite bundled with the `rusqlite` crate (ensuring FTS5 support). To link against the system SQLite instead:

```bash
cargo build --no-default-features --features sqlite-system
```

## Contributing

1. Ensure you are using Rust 1.78 or newer (`rustup update stable`).
2. Install the [cargo-nextest](https://nexte.st/) runner for faster test cycles (optional).
3. Run `cargo fmt` and `cargo clippy --all-targets` before sending patches.

We use GitHub issues to track planned features and bugs once the MVP is in place. Until then, keep discussion in the project tracker.
