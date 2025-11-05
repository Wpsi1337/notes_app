# Notes TUI – Architecture Overview

## Technology selections

- **Language & runtime**: Rust 1.78+ for performance, robust error handling, and zero-cost abstractions that fit the 200 ms launch and low-latency rendering targets.
- **Terminal UI**: [`ratatui`](https://github.com/ratatui-org/ratatui) with the `crossterm` backend for broad terminal support (kitty, alacritty, foot, xterm, tmux, Windows SSH). Rendering runs on a single thread with diffing and retained component state to stay within the 16 ms frame budget.
- **Storage**: SQLite 3 in WAL mode via [`rusqlite`](https://github.com/rusqlite/rusqlite). A bundled SQLite build (behind a feature flag) ensures FTS5 availability while keeping the default build minimal. Databases live under XDG data directories with automatic creation on first launch.
- **Search index**: SQLite FTS5 virtual table with a custom tokenizer configuration that preserves diacritics and supports prefix search. Fuzzy matching is implemented in Rust before delegating to FTS queries.
- **Configuration**: TOML files parsed with [`serde`](https://serde.rs/) and [`toml`](https://docs.rs/toml) from `~/.config/notetui/config.toml`, with environment overrides via `NOTETUI_CONFIG` and `NOTETUI_DATA`.
- **Logging & observability**: [`tracing` + `tracing-subscriber`] for structured logs written to `~/.local/state/notetui/logs/notetui.log`, with on-demand verbose toggles for debugging.

## Crate layout

```
notes-tui/
├─ Cargo.toml
└─ src/
   ├─ main.rs                // CLI entry point & bootstrap
   ├─ app/
   │   ├─ mod.rs             // Central application state machine
   │   ├─ list.rs            // Virtualised note list + filters
   │   ├─ reader.rs          // Markdown reader/editor pane
   │   ├─ search.rs          // Query parsing & result orchestration
   │   └─ commands.rs        // Action dispatcher & command palette
   ├─ ui/
   │   ├─ mod.rs             // Rendering primitives + theme palette
   │   ├─ components/        // Focused widgets (status bar, dialogs, toasts)
   │   └─ layout.rs          // Split panes + resize handling
   ├─ storage/
   │   ├─ mod.rs             // Connection pool & migrations
   │   ├─ schema.rs          // SQL definitions (tables, triggers)
   │   ├─ repos.rs           // CRUD implementations
   │   └─ search.rs          // FTS index sync helpers
   ├─ config/
   │   ├─ mod.rs             // Config model, defaults, merging overrides
   │   └─ themes.rs          // Built-in theme definitions & loader
   ├─ markdown/
   │   └─ renderer.rs        // Inline markdown → ANSI translation
   ├─ journaling/
   │   └─ autosave.rs        // Crash recovery journal + debounce logic
   └─ cli/
       ├─ mod.rs             // `clap`-powered CLI interface
       └─ commands.rs        // Implementations for `notetui` subcommands
```

Each module exposes a narrow interface so that state changes flow through the `app` layer, which then triggers UI rerenders. This separation allows headless integration tests to drive the state machine without a terminal.

## Data model & schema

Primary tables (simplified names for clarity):

```sql
notes (
    id INTEGER PRIMARY KEY,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    pinned INTEGER NOT NULL DEFAULT 0,
    archived INTEGER NOT NULL DEFAULT 0,
    deleted_at INTEGER
);

tags (
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL UNIQUE
);

note_tags (
    note_id INTEGER NOT NULL REFERENCES notes(id),
    tag_id INTEGER NOT NULL REFERENCES tags(id),
    PRIMARY KEY (note_id, tag_id)
);

fts_notes (
    id UNINDEXED,
    title,
    body,
    content='notes',
    tokenize='unicode 61 tokenchars "_-"',
    content_rowid='id'
);

backups (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    path TEXT NOT NULL
);
```

Triggers keep `updated_at` correct, refresh `fts_notes`, and cascade tag deletions. Deleted notes move to “trash” by setting `deleted_at` rather than removing rows; purge jobs vacuum rows older than the retention window.

## State & event flow

1. **Bootstrap**: main loads config, initialises logging, opens the SQLite connection (creating schema if missing), seeds starter notes, and builds the `App` state struct.
2. **Event loop**: the `App` owns:
   - `AppState`: current route (list/reader/editor/trash/config), filters, search query, selection, sort mode, dirty flags.
   - `Effects`: cross-cutting state such as toasts, modal dialogs, background task handles.
   - `Store`: shared storage facade that batches DB interactions onto a dedicated thread to keep the UI responsive.
3. **Rendering**: `ui::*` renders the state to `ratatui` frames. Virtualised list rendering only lays out visible rows, honoring search highlights and filter badges.
4. **Input handling**: `crossterm` events feed into a keybinding resolver that maps keys → actions based on the active profile (vim/emacs/custom). Actions mutate state and queue storage operations asynchronously. Results feed back into the state via channels.
5. **Auto-save & journaling**: editor component debounces edits into a journal file under `~/.cache/notetui/` so that forced exits recover unsaved work. Saving flushes both DB and journal snapshot.

## Search pipeline

1. Parse the query into tokens (plain text, field qualifiers like `tag:`, `title:`, range filters, `-exclude`, optional `regex` flag).
2. Build a fuzzy expansion map (e.g., `term` → [`term*`, `term~`]) that the FTS engine can execute with prefix matching.
3. Apply non-FTS filters (tags, pinned, date ranges) via SQL `WHERE` clauses against the main table joined to tags.
4. Execute the FTS query with a LIMIT tuned for the UI viewport (default 200). If regex mode is enabled, post-filter the results in Rust to keep SQLite load low.
5. Return ranked results with highlighted spans for the UI to display.

## Testing strategy

- **Unit tests**: pure Rust tests for schema migrations, FTS query builders, fuzzy parser, tag operations, autosave timers.
- **Integration tests**: headless event loop simulations using `crossterm::event::Event` fixtures to validate navigation, search, note lifecycle, and crash recovery flows.
- **Snapshot/render tests**: leverage `ratatui`’s offscreen renderer to capture golden frames for list, reader, dialogs under multiple themes.
- **Performance harness**: optional `cargo criterion` bench that seeds 10k notes and asserts search + render timings.
- **Crash recovery**: scripted tests that simulate process termination during save and assert journal replay correctness on restart.

## Next milestones

1. Implement bootstrap crate (`Cargo.toml`, `main.rs`) with logging, config loader, and database migration runner.
2. Flesh out storage layer with migrations, FTS setup, and seed data for the first run experience.
3. Build the base TUI shell (layout, input loop, empty state UI) to validate startup performance and keyboard handling.
4. Layer in search, tag filtering, and note lifecycle commands incrementally, backed by automated tests.
