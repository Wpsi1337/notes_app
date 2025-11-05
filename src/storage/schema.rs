use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r#"
        PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS notes (
            id INTEGER PRIMARY KEY,
            title TEXT NOT NULL,
            body TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            pinned INTEGER NOT NULL DEFAULT 0,
            archived INTEGER NOT NULL DEFAULT 0,
            deleted_at INTEGER
        );

        CREATE TABLE IF NOT EXISTS tags (
            id INTEGER PRIMARY KEY,
            name TEXT NOT NULL UNIQUE
        );

        CREATE TABLE IF NOT EXISTS note_tags (
            note_id INTEGER NOT NULL,
            tag_id INTEGER NOT NULL,
            PRIMARY KEY (note_id, tag_id),
            FOREIGN KEY (note_id) REFERENCES notes(id) ON DELETE CASCADE,
            FOREIGN KEY (tag_id) REFERENCES tags(id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS backups (
            id INTEGER PRIMARY KEY,
            created_at INTEGER NOT NULL,
            path TEXT NOT NULL
        );

        CREATE VIRTUAL TABLE IF NOT EXISTS fts_notes USING fts5(
            title,
            body,
            content='notes',
            content_rowid='id',
            tokenize='unicode61'
        );

        CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
            INSERT INTO fts_notes(rowid, title, body)
            VALUES (new.id, new.title, new.body);
        END;

        CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
            INSERT INTO fts_notes(fts_notes, rowid, title, body)
            VALUES ('delete', old.id, old.title, old.body);
        END;

        CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
            INSERT INTO fts_notes(fts_notes, rowid, title, body)
            VALUES ('delete', old.id, old.title, old.body);
            INSERT INTO fts_notes(rowid, title, body)
            VALUES (new.id, new.title, new.body);
        END;

        CREATE TRIGGER IF NOT EXISTS notes_touch_updated AFTER UPDATE OF body, title, pinned, archived ON notes
        BEGIN
            UPDATE notes SET updated_at = strftime('%s', 'now') WHERE id = new.id;
        END;
        "#,
    )
    .context("applying schema migrations")?;
    Ok(())
}
