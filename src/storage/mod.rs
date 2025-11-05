use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use regex::RegexBuilder;
use rusqlite::config::DbConfig;
use rusqlite::{params, Connection, OptionalExtension};
use time::OffsetDateTime;

use crate::config::{ConfigPaths, StorageOptions};
use crate::search::SearchQuery;

mod schema;

const TAG_DELIMITER: &str = "|:|";
const FTS_ROW_LIMIT: usize = 200;

#[derive(Debug, Clone)]
pub struct NoteRecord {
    pub id: i64,
    pub title: String,
    pub body: String,
    pub snippet: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub pinned: bool,
    pub archived: bool,
    pub tags: Vec<String>,
}

#[derive(Clone)]
pub struct StorageHandle {
    db_path: Arc<PathBuf>,
    options: Arc<StorageOptions>,
}

impl StorageHandle {
    pub fn connect(&self) -> Result<Connection> {
        let conn = Connection::open(&*self.db_path)
            .with_context(|| format!("opening database {}", self.db_path.display()))?;
        prepare_connection(&conn, &self.options)?;
        Ok(conn)
    }

    pub fn with_connection<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Connection) -> Result<T>,
    {
        let conn = self.connect()?;
        f(&conn)
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn fetch_recent_notes(&self, limit: usize) -> Result<Vec<NoteRecord>> {
        self.with_connection(|conn| {
            let sql = format!(
                "SELECT n.id,
                        n.title,
                        n.body,
                        n.created_at,
                        n.updated_at,
                        n.pinned,
                        n.archived,
                        COALESCE(GROUP_CONCAT(t.name, '{delim}'), '')
                 FROM notes n
                 LEFT JOIN note_tags nt ON nt.note_id = n.id
                 LEFT JOIN tags t ON t.id = nt.tag_id
                 WHERE n.deleted_at IS NULL
                   AND n.archived = 0
                 GROUP BY n.id
                 ORDER BY n.pinned DESC, n.updated_at DESC
                 LIMIT ?1",
                delim = TAG_DELIMITER
            );
            let mut stmt = conn.prepare(&sql)?;
            let records = stmt
                .query_map([limit as i64], |row| {
                    let tags: String = row.get(7)?;
                    Ok(NoteRecord {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        body: row.get(2)?,
                        snippet: None,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                        pinned: row.get::<_, i64>(5)? != 0,
                        archived: row.get::<_, i64>(6)? != 0,
                        tags: parse_tags(&tags),
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(records)
        })
    }

    pub fn fetch_trashed_notes(&self, limit: usize) -> Result<Vec<NoteRecord>> {
        self.with_connection(|conn| {
            let sql = format!(
                "SELECT n.id,
                        n.title,
                        n.body,
                        n.created_at,
                        n.updated_at,
                        n.pinned,
                        n.archived,
                        COALESCE(GROUP_CONCAT(t.name, '{delim}'), '')
                 FROM notes n
                 LEFT JOIN note_tags nt ON nt.note_id = n.id
                 LEFT JOIN tags t ON t.id = nt.tag_id
                 WHERE n.deleted_at IS NOT NULL
                 GROUP BY n.id
                 ORDER BY n.deleted_at DESC
                 LIMIT ?1",
                delim = TAG_DELIMITER
            );
            let mut stmt = conn.prepare(&sql)?;
            let records = stmt
                .query_map([limit as i64], |row| {
                    let tags: String = row.get(7)?;
                    Ok(NoteRecord {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        body: row.get(2)?,
                        snippet: None,
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                        pinned: row.get::<_, i64>(5)? != 0,
                        archived: row.get::<_, i64>(6)? != 0,
                        tags: parse_tags(&tags),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(records)
        })
    }

    pub fn search_notes(&self, query: &SearchQuery, limit: usize) -> Result<Vec<NoteRecord>> {
        if !query.has_terms() && !query.has_filters() {
            return self.fetch_recent_notes(limit);
        }

        let fetch_limit = limit.max(FTS_ROW_LIMIT);
        let mut notes = if query.has_terms() {
            self.search_with_terms(query, fetch_limit)?
        } else {
            self.fetch_recent_notes(fetch_limit)?
        };

        apply_filters(&mut notes, query);
        if let Some(pattern) = query.regex_pattern.as_deref() {
            let regex = RegexBuilder::new(pattern)
                .case_insensitive(true)
                .build()
                .context("compiling regex search pattern")?;
            notes.retain(|note| regex.is_match(&note.title) || regex.is_match(&note.body));
        }
        if notes.len() > limit {
            notes.truncate(limit);
        }
        Ok(notes)
    }

    fn search_with_terms(&self, query: &SearchQuery, limit: usize) -> Result<Vec<NoteRecord>> {
        let Some(match_expr) = build_match_expression(query) else {
            return Ok(Vec::new());
        };
        self.with_connection(|conn| {
            let sql = format!(
                "SELECT n.id,
                        n.title,
                        n.body,
                        n.created_at,
                        n.updated_at,
                        n.pinned,
                        n.archived,
                        COALESCE(GROUP_CONCAT(t.name, '{delim}'), '') AS tags,
                        snippet(fts_notes, 1, '', '', ' ... ', 20) AS snippet
                 FROM fts_notes
                 INNER JOIN notes n ON n.id = fts_notes.rowid
                 LEFT JOIN note_tags nt ON nt.note_id = n.id
                 LEFT JOIN tags t ON t.id = nt.tag_id
                 WHERE n.deleted_at IS NULL
                   AND n.archived = 0
                   AND fts_notes MATCH ?1
                 GROUP BY n.id
                 ORDER BY n.pinned DESC, bm25(fts_notes), n.updated_at DESC
                 LIMIT ?2",
                delim = TAG_DELIMITER
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![match_expr, limit as i64],
                |row| -> rusqlite::Result<NoteRecord> {
                    let tags: String = row.get(7)?;
                    let snippet: String = row.get(8)?;
                    let snippet = snippet.trim();
                    Ok(NoteRecord {
                        id: row.get(0)?,
                        title: row.get(1)?,
                        body: row.get(2)?,
                        snippet: if snippet.is_empty() {
                            None
                        } else {
                            Some(snippet.to_string())
                        },
                        created_at: row.get(3)?,
                        updated_at: row.get(4)?,
                        pinned: row.get::<_, i64>(5)? != 0,
                        archived: row.get::<_, i64>(6)? != 0,
                        tags: parse_tags(&tags),
                    })
                },
            )?;
            rows.collect::<Result<Vec<_>, _>>()
                .context("querying search results")
        })
    }

    pub fn set_note_pinned(&self, note_id: i64, pinned: bool) -> Result<()> {
        self.with_connection(|conn| {
            let updated = conn
                .execute(
                    "UPDATE notes SET pinned = ?1 WHERE id = ?2",
                    params![if pinned { 1 } else { 0 }, note_id],
                )
                .context("updating note pinned state")?;
            if updated == 0 {
                bail!("note {note_id} not found");
            }
            Ok(())
        })
    }

    pub fn set_note_archived(&self, note_id: i64, archived: bool) -> Result<()> {
        self.with_connection(|conn| {
            let updated = conn
                .execute(
                    "UPDATE notes SET archived = ?1 WHERE id = ?2",
                    params![if archived { 1 } else { 0 }, note_id],
                )
                .context("updating note archived state")?;
            if updated == 0 {
                bail!("note {note_id} not found");
            }
            Ok(())
        })
    }

    pub fn create_note(&self, title: &str, body: &str, pinned: bool) -> Result<i64> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            bail!("note title cannot be empty");
        }
        self.with_connection(|conn| {
            let now = OffsetDateTime::now_utc().unix_timestamp();
            conn.execute(
                "INSERT INTO notes (title, body, created_at, updated_at, pinned, archived)
                 VALUES (?1, ?2, ?3, ?3, ?4, 0)",
                params![trimmed, body, now, if pinned { 1 } else { 0 }],
            )
            .context("inserting note")?;
            Ok(conn.last_insert_rowid())
        })
    }

    pub fn add_tag_to_note(&self, note_id: i64, tag_name: &str) -> Result<()> {
        let tag = tag_name.trim();
        if tag.is_empty() {
            bail!("tag name cannot be empty");
        }
        self.with_connection(|conn| {
            let tag_id = match conn
                .query_row("SELECT id FROM tags WHERE name = ?1", params![tag], |row| {
                    row.get::<_, i64>(0)
                })
                .optional()?
            {
                Some(id) => id,
                None => {
                    conn.execute("INSERT INTO tags (name) VALUES (?1)", params![tag])
                        .context("inserting tag")?;
                    conn.last_insert_rowid()
                }
            };
            conn.execute(
                "INSERT OR IGNORE INTO note_tags (note_id, tag_id) VALUES (?1, ?2)",
                params![note_id, tag_id],
            )
            .context("linking tag to note")?;
            Ok(())
        })
    }

    pub fn remove_tag_from_note(&self, note_id: i64, tag_name: &str) -> Result<()> {
        let tag = tag_name.trim();
        if tag.is_empty() {
            bail!("tag name cannot be empty");
        }
        self.with_connection(|conn| {
            let affected = conn.execute(
                "DELETE FROM note_tags
                 WHERE note_id = ?1
                   AND tag_id = (SELECT id FROM tags WHERE name = ?2)",
                params![note_id, tag],
            )?;
            if affected == 0 {
                bail!("tag '{tag}' not associated with note {note_id}");
            }
            Ok(())
        })
    }

    pub fn rename_note_title(&self, note_id: i64, title: &str) -> Result<()> {
        let trimmed = title.trim();
        if trimmed.is_empty() {
            bail!("note title cannot be empty");
        }
        self.with_connection(|conn| {
            let updated = conn.execute(
                "UPDATE notes SET title = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                params![trimmed, note_id],
            )?;
            if updated == 0 {
                bail!("note {note_id} not found");
            }
            Ok(())
        })
    }

    pub fn update_note_body(&self, note_id: i64, body: &str) -> Result<()> {
        self.with_connection(|conn| {
            let updated = conn.execute(
                "UPDATE notes SET body = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                params![body, note_id],
            )?;
            if updated == 0 {
                bail!("note {note_id} not found");
            }
            Ok(())
        })
    }

    pub fn soft_delete_note(&self, note_id: i64) -> Result<()> {
        let now = OffsetDateTime::now_utc().unix_timestamp();
        self.with_connection(|conn| {
            let updated = conn.execute(
                "UPDATE notes SET deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
                params![now, note_id],
            )?;
            if updated == 0 {
                bail!("note {note_id} not found");
            }
            Ok(())
        })
    }

    pub fn list_all_tags(&self) -> Result<Vec<String>> {
        self.with_connection(|conn| {
            let mut stmt = conn.prepare("SELECT name FROM tags ORDER BY name COLLATE NOCASE")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()
                .context("fetching all tags")
        })
    }

    pub fn restore_note(&self, note_id: i64) -> Result<()> {
        self.with_connection(|conn| {
            let updated = conn.execute(
                "UPDATE notes SET deleted_at = NULL WHERE id = ?1 AND deleted_at IS NOT NULL",
                params![note_id],
            )?;
            if updated == 0 {
                bail!("note {note_id} not found in trash");
            }
            Ok(())
        })
    }
}

fn build_match_expression(query: &SearchQuery) -> Option<String> {
    let mut clauses = Vec::new();
    if let Some(clause) = build_clause(None, &query.terms) {
        clauses.push(clause);
    }
    if let Some(clause) = build_clause(Some("title"), &query.title_terms) {
        clauses.push(clause);
    }
    if clauses.is_empty() {
        None
    } else {
        Some(clauses.join(" AND "))
    }
}

fn build_clause(column: Option<&str>, terms: &[String]) -> Option<String> {
    let mut parts = Vec::new();
    for term in terms {
        let trimmed = term.trim();
        if trimmed.is_empty() {
            continue;
        }
        let escaped = trimmed.replace('"', "\"\"");
        if let Some(col) = column {
            parts.push(format!("{col}:\"{escaped}\"*"));
        } else {
            parts.push(format!("\"{escaped}\"*"));
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" AND "))
    }
}

fn parse_tags(raw: &str) -> Vec<String> {
    if raw.is_empty() {
        return Vec::new();
    }
    raw.split(TAG_DELIMITER)
        .filter(|tag| !tag.is_empty())
        .map(|tag| tag.to_string())
        .collect()
}

fn apply_filters(notes: &mut Vec<NoteRecord>, query: &SearchQuery) {
    if !query.has_filters() {
        return;
    }

    let tags_filter = if query.tags.is_empty() {
        None
    } else {
        Some(
            query
                .tags
                .iter()
                .map(|tag| tag.to_lowercase())
                .collect::<Vec<_>>(),
        )
    };

    notes.retain(|note| {
        if let Some(filter_tags) = &tags_filter {
            let note_tags: HashSet<String> =
                note.tags.iter().map(|tag| tag.to_lowercase()).collect();
            for tag in filter_tags {
                if !note_tags.contains(tag) {
                    return false;
                }
            }
        }

        if let Some(from) = query.created.from {
            if note.created_at < from {
                return false;
            }
        }
        if let Some(to) = query.created.to {
            if note.created_at >= to {
                return false;
            }
        }

        if let Some(from) = query.updated.from {
            if note.updated_at < from {
                return false;
            }
        }
        if let Some(to) = query.updated.to {
            if note.updated_at >= to {
                return false;
            }
        }

        true
    });
}

pub fn init(paths: &ConfigPaths, storage: &StorageOptions) -> Result<StorageHandle> {
    let db_path = &paths.database_path;
    let existed = db_path.exists();
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating data directory {}", parent.display()))?;
    }
    let conn = Connection::open(db_path)
        .with_context(|| format!("opening database {}", db_path.display()))?;
    prepare_connection(&conn, storage)?;
    schema::apply(&conn)?;
    if !existed {
        seed_initial_notes(&conn)?;
    }
    Ok(StorageHandle {
        db_path: Arc::new(db_path.clone()),
        options: Arc::new(storage.clone()),
    })
}

fn prepare_connection(conn: &Connection, storage: &StorageOptions) -> Result<()> {
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)
        .context("enabling foreign keys")?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .context("setting journal_mode=WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .context("setting synchronous=NORMAL")?;
    conn.pragma_update(
        None,
        "wal_autocheckpoint",
        storage.wal_autocheckpoint.to_string(),
    )
    .context("setting wal_autocheckpoint")?;
    Ok(())
}

fn seed_initial_notes(conn: &Connection) -> Result<()> {
    let existing: Option<i64> = conn
        .query_row("SELECT id FROM notes LIMIT 1", [], |row| row.get(0))
        .optional()
        .context("checking for existing notes")?;
    if existing.is_some() {
        return Ok(());
    }

    tracing::info!("seeding first-run notes");
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let notes = [
        (
            "Welcome to Notes TUI",
            r#"# Welcome to Notes TUI

This is your new note space. Press `?` inside the app to see keyboard shortcuts.
"#,
        ),
        (
            "Keyboard Shortcuts",
            r#"# Keyboard Highlights

- `j` / `k`: move up or down the list
- `Enter`: open the selected note
- `e`: edit the note
- `/`: start a quick search
- `:`: command palette
- `q`: quit
"#,
        ),
        ("Inbox", "Capture quick thoughts here.\n"),
    ];

    for (title, body) in notes {
        conn.execute(
            "INSERT INTO notes (title, body, created_at, updated_at, pinned, archived)
             VALUES (?1, ?2, ?3, ?3, 0, 0)",
            params![title, body, now],
        )
        .context("inserting seed note")?;
    }

    Ok(())
}
