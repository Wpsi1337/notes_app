use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use regex::{Regex, RegexBuilder};
use rusqlite::config::DbConfig;
use rusqlite::{params, Connection, OptionalExtension};
use time::OffsetDateTime;

use crate::config::{ConfigPaths, StorageOptions};
use crate::search::SearchQuery;

mod schema;

const TAG_DELIMITER: &str = "|:|";
const FTS_ROW_LIMIT: usize = 200;
const BM25_TITLE_WEIGHT: f64 = 0.2;
const BM25_BODY_WEIGHT: f64 = 1.0;

#[derive(Debug, Clone, Copy)]
pub struct WalCheckpointStats {
    pub busy_frames: i64,
    pub wal_frames: i64,
    pub checkpointed_frames: i64,
}

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
    pub deleted_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagRenameOutcome {
    Renamed {
        from: String,
        to: String,
    },
    Merged {
        from: String,
        to: String,
        reassigned: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagDeleteOutcome {
    pub tag: String,
    pub detached: usize,
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

    pub fn run_wal_health_check(&self) -> Result<WalCheckpointStats> {
        self.with_connection(|conn| {
            let mut stmt = conn
                .prepare("PRAGMA wal_checkpoint(PASSIVE)")
                .context("preparing wal checkpoint pragma")?;
            let mut rows = stmt.query([]).context("executing wal checkpoint pragma")?;
            if let Some(row) = rows.next()? {
                Ok(WalCheckpointStats {
                    busy_frames: row.get(0)?,
                    wal_frames: row.get(1)?,
                    checkpointed_frames: row.get(2)?,
                })
            } else {
                bail!("wal checkpoint returned no rows");
            }
        })
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
                        COALESCE(GROUP_CONCAT(t.name, '{delim}'), ''),
                        n.deleted_at
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
                        deleted_at: row.get::<_, Option<i64>>(8)?,
                    })
                })?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(records)
        })
    }

    fn fetch_notes_batch(&self, limit: usize, offset: usize) -> Result<Vec<NoteRecord>> {
        self.with_connection(|conn| {
            let sql = format!(
                "SELECT n.id,
                        n.title,
                        n.body,
                        n.created_at,
                        n.updated_at,
                        n.pinned,
                        n.archived,
                        COALESCE(GROUP_CONCAT(t.name, '{delim}'), ''),
                        n.deleted_at
                 FROM notes n
                 LEFT JOIN note_tags nt ON nt.note_id = n.id
                 LEFT JOIN tags t ON t.id = nt.tag_id
                 WHERE n.deleted_at IS NULL
                   AND n.archived = 0
                 GROUP BY n.id
                 ORDER BY n.pinned DESC, n.updated_at DESC
                 LIMIT ?1 OFFSET ?2",
                delim = TAG_DELIMITER
            );
            let mut stmt = conn.prepare(&sql)?;
            let records = stmt
                .query_map(params![limit as i64, offset as i64], |row| {
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
                        deleted_at: row.get::<_, Option<i64>>(8)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
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
                        COALESCE(GROUP_CONCAT(t.name, '{delim}'), ''),
                        n.deleted_at
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
                        deleted_at: row.get::<_, Option<i64>>(8)?,
                        tags: parse_tags(&tags),
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok(records)
        })
    }

    pub fn search_notes(&self, query: &SearchQuery, limit: usize) -> Result<Vec<NoteRecord>> {
        if !query.has_terms() && !query.has_filters() && query.regex_pattern.is_none() {
            return self.fetch_recent_notes(limit);
        }

        if query.regex_pattern.is_some() && !query.has_terms() {
            let regex = RegexBuilder::new(query.regex_pattern.as_deref().unwrap())
                .case_insensitive(true)
                .build()
                .context("compiling regex search pattern")?;
            return self.search_regex_only(query, limit, regex);
        }

        let regex = if let Some(pattern) = query.regex_pattern.as_deref() {
            Some(
                RegexBuilder::new(pattern)
                    .case_insensitive(true)
                    .build()
                    .context("compiling regex search pattern")?,
            )
        } else {
            None
        };

        let fetch_limit = limit.max(FTS_ROW_LIMIT);
        let mut notes = if query.has_terms() {
            self.search_with_terms(query, fetch_limit)?
        } else {
            self.fetch_recent_notes(fetch_limit)?
        };

        apply_filters(&mut notes, query);
        if let Some(regex) = &regex {
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
        let title_priority_tokens = query
            .highlight_terms()
            .into_iter()
            .map(|token| token.to_lowercase())
            .collect::<Vec<_>>();
        self.with_connection(|conn| {
            let sql = format!(
                "SELECT n.id,
                        n.title,
                        n.body,
                        n.created_at,
                        n.updated_at,
                        n.pinned,
                        n.archived,
                        COALESCE((
                            SELECT GROUP_CONCAT(t2.name, '{delim}')
                            FROM note_tags nt2
                            INNER JOIN tags t2 ON t2.id = nt2.tag_id
                            WHERE nt2.note_id = n.id
                        ), '') AS tags,
                        n.deleted_at,
                        snippet(fts_notes, -1, '', '', ' ... ', 20) AS snippet
                 FROM fts_notes
                 INNER JOIN notes n ON n.id = fts_notes.rowid
                 WHERE n.deleted_at IS NULL
                   AND n.archived = 0
                   AND fts_notes MATCH ?1
                 ORDER BY n.pinned DESC,
                          bm25(fts_notes, {title_weight}, {body_weight}),
                          n.updated_at DESC
                 LIMIT ?2",
                delim = TAG_DELIMITER,
                title_weight = BM25_TITLE_WEIGHT,
                body_weight = BM25_BODY_WEIGHT
            );
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params![match_expr, limit as i64],
                |row| -> rusqlite::Result<NoteRecord> {
                    let tags: String = row.get(7)?;
                    let deleted_at = row.get::<_, Option<i64>>(8)?;
                    let snippet: String = row.get(9)?;
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
                        deleted_at,
                    })
                },
            )?;
            let notes = rows
                .collect::<Result<Vec<_>, _>>()
                .context("querying search results")?;
            if title_priority_tokens.is_empty() {
                Ok(notes)
            } else {
                Ok(prioritize_title_matches(notes, &title_priority_tokens))
            }
        })
    }

    fn search_regex_only(
        &self,
        query: &SearchQuery,
        limit: usize,
        regex: Regex,
    ) -> Result<Vec<NoteRecord>> {
        let mut results = Vec::new();
        let mut offset = 0usize;
        let batch_size = limit.max(FTS_ROW_LIMIT);
        loop {
            let mut batch = self.fetch_notes_batch(batch_size, offset)?;
            if batch.is_empty() {
                break;
            }
            apply_filters(&mut batch, query);
            batch.retain(|note| regex.is_match(&note.title) || regex.is_match(&note.body));
            results.extend(batch);
            if results.len() >= limit {
                break;
            }
            offset += batch_size;
        }
        if results.len() > limit {
            results.truncate(limit);
        }
        Ok(results)
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

    pub fn rename_tag(&self, current: &str, new_name: &str) -> Result<TagRenameOutcome> {
        let from = current.trim();
        let to = new_name.trim();
        if from.is_empty() || to.is_empty() {
            bail!("tag names cannot be empty");
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let source_id: i64 = tx
            .query_row(
                "SELECT id FROM tags WHERE name = ?1",
                params![from],
                |row| row.get(0),
            )
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("tag '{from}' not found"))?;

        let existing: Option<i64> = tx
            .query_row("SELECT id FROM tags WHERE name = ?1", params![to], |row| {
                row.get(0)
            })
            .optional()?;

        let outcome = match existing {
            Some(target_id) if target_id != source_id => {
                let reassigned = tx.execute(
                    "INSERT OR IGNORE INTO note_tags (note_id, tag_id)
                     SELECT note_id, ?1 FROM note_tags WHERE tag_id = ?2",
                    params![target_id, source_id],
                )?;
                tx.execute(
                    "DELETE FROM note_tags WHERE tag_id = ?1",
                    params![source_id],
                )?;
                tx.execute("DELETE FROM tags WHERE id = ?1", params![source_id])?;
                TagRenameOutcome::Merged {
                    from: from.to_string(),
                    to: to.to_string(),
                    reassigned,
                }
            }
            _ => {
                tx.execute(
                    "UPDATE tags SET name = ?1 WHERE id = ?2",
                    params![to, source_id],
                )?;
                TagRenameOutcome::Renamed {
                    from: from.to_string(),
                    to: to.to_string(),
                }
            }
        };

        tx.commit()?;
        Ok(outcome)
    }

    pub fn delete_tag(&self, name: &str) -> Result<TagDeleteOutcome> {
        let tag = name.trim();
        if tag.is_empty() {
            bail!("tag name cannot be empty");
        }
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        let tag_id: i64 = tx
            .query_row("SELECT id FROM tags WHERE name = ?1", params![tag], |row| {
                row.get(0)
            })
            .optional()?
            .ok_or_else(|| anyhow::anyhow!("tag '{tag}' not found"))?;

        let detached = tx.execute("DELETE FROM note_tags WHERE tag_id = ?1", params![tag_id])?;
        tx.execute("DELETE FROM tags WHERE id = ?1", params![tag_id])?;
        tx.commit()?;
        Ok(TagDeleteOutcome {
            tag: tag.to_string(),
            detached,
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

    pub fn tag_exists(&self, name: &str) -> Result<bool> {
        let tag = name.trim();
        if tag.is_empty() {
            return Ok(false);
        }
        self.with_connection(|conn| {
            let exists = conn
                .query_row("SELECT 1 FROM tags WHERE name = ?1", params![tag], |_row| {
                    Ok(())
                })
                .optional()?
                .is_some();
            Ok(exists)
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

    pub fn fetch_note_by_id(&self, note_id: i64) -> Result<Option<NoteRecord>> {
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
                        n.deleted_at
                 FROM notes n
                 LEFT JOIN note_tags nt ON nt.note_id = n.id
                 LEFT JOIN tags t ON t.id = nt.tag_id
                 WHERE n.id = ?1 AND n.deleted_at IS NULL
                 GROUP BY n.id",
                delim = TAG_DELIMITER
            );
            let mut stmt = conn.prepare(&sql)?;
            let result = stmt
                .query_row(params![note_id], |row| {
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
                        deleted_at: row.get::<_, Option<i64>>(8)?,
                    })
                })
                .optional()?;
            Ok(result)
        })
    }

    pub fn restore_all_trash(&self) -> Result<usize> {
        self.with_connection(|conn| {
            let count = conn.execute(
                "UPDATE notes SET deleted_at = NULL WHERE deleted_at IS NOT NULL",
                [],
            )?;
            Ok(count)
        })
    }

    pub fn purge_all_trash(&self) -> Result<usize> {
        self.with_connection(|conn| {
            let count = conn.execute("DELETE FROM notes WHERE deleted_at IS NOT NULL", [])?;
            Ok(count)
        })
    }

    pub fn purge_expired_trash(&self, retention_days: u32) -> Result<usize> {
        if retention_days == 0 {
            return Ok(0);
        }
        let threshold =
            OffsetDateTime::now_utc().unix_timestamp() - i64::from(retention_days) * 86_400;
        self.with_connection(|conn| {
            let count = conn.execute(
                "DELETE FROM notes WHERE deleted_at IS NOT NULL AND deleted_at <= ?1",
                params![threshold],
            )?;
            Ok(count)
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
        let has_whitespace = trimmed.chars().any(|ch| ch.is_whitespace());
        let fragment = if has_whitespace {
            if let Some(col) = column {
                format!("{col}:\"{escaped}\"")
            } else {
                format!("\"{escaped}\"")
            }
        } else {
            let token = escaped.replace(':', " ");
            if let Some(col) = column {
                format!("{col}:{token}*")
            } else {
                format!("{token}*")
            }
        };
        parts.push(fragment);
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

fn prioritize_title_matches(notes: Vec<NoteRecord>, tokens: &[String]) -> Vec<NoteRecord> {
    let mut with_title = Vec::new();
    let mut without_title = Vec::new();
    for note in notes {
        if title_contains_any(&note.title, tokens) {
            with_title.push(note);
        } else {
            without_title.push(note);
        }
    }
    with_title.extend(without_title);
    with_title
}

fn title_contains_any(title: &str, tokens: &[String]) -> bool {
    if tokens.is_empty() {
        return false;
    }
    let haystack = title.to_lowercase();
    tokens
        .iter()
        .any(|token| !token.is_empty() && haystack.contains(token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigPaths, StorageOptions};
    use crate::search::SearchQuery;
    use tempfile::TempDir;

    fn temp_paths(root: &TempDir) -> ConfigPaths {
        let base = root.path();
        let config_dir = base.join("config");
        let data_dir = base.join("data");
        let cache_dir = base.join("cache");
        let state_dir = base.join("state");
        let log_dir = base.join("logs");
        let backup_dir = base.join("backups");
        ConfigPaths {
            config_dir: config_dir.clone(),
            config_file: config_dir.join("config.toml"),
            data_dir: data_dir.clone(),
            database_path: data_dir.join("notes.db"),
            cache_dir,
            backup_dir,
            log_dir,
            state_dir,
        }
    }

    fn storage_options(paths: &ConfigPaths) -> StorageOptions {
        let mut options = StorageOptions::default();
        options.database_path = paths.database_path.clone();
        options.backup_dir = paths.backup_dir.clone();
        options
    }

    fn init_storage() -> anyhow::Result<(TempDir, StorageHandle)> {
        let temp = TempDir::new()?;
        let paths = temp_paths(&temp);
        paths.ensure_directories()?;
        let opts = storage_options(&paths);
        let storage = init(&paths, &opts)?;
        Ok((temp, storage))
    }

    #[test]
    fn rename_tag_updates_all_references() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let note_id = storage.create_note("Test", "body", false)?;
        storage.add_tag_to_note(note_id, "alpha")?;

        let outcome = storage.rename_tag("alpha", "beta")?;
        assert!(matches!(
            outcome,
            TagRenameOutcome::Renamed { ref from, ref to }
                if from == "alpha" && to == "beta"
        ));

        let summary = storage.fetch_recent_notes(5)?;
        let tags = summary
            .iter()
            .find(|note| note.id == note_id)
            .expect("note present")
            .tags
            .clone();
        assert!(tags.contains(&"beta".to_string()));
        assert!(!tags.iter().any(|tag| tag == "alpha"));
        Ok(())
    }

    #[test]
    fn rename_tag_merges_into_existing_tag() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let alpha_note = storage.create_note("Alpha", "alpha body", false)?;
        let beta_note = storage.create_note("Beta", "beta body", false)?;
        storage.add_tag_to_note(alpha_note, "alpha")?;
        storage.add_tag_to_note(beta_note, "beta")?;

        let outcome = storage.rename_tag("alpha", "beta")?;
        match outcome {
            TagRenameOutcome::Merged {
                from,
                to,
                reassigned,
            } => {
                assert_eq!(from, "alpha");
                assert_eq!(to, "beta");
                assert!(reassigned >= 1);
            }
            other => panic!("expected merged outcome, got {other:?}"),
        }

        let notes = storage.fetch_recent_notes(10)?;
        for note in notes {
            assert!(!note.tags.iter().any(|tag| tag == "alpha"));
        }
        Ok(())
    }

    #[test]
    fn delete_tag_unlinks_all_notes() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let note = storage.create_note("Disposable", "body", false)?;
        storage.add_tag_to_note(note, "alpha")?;

        let outcome = storage.delete_tag("alpha")?;
        assert_eq!(outcome.tag, "alpha");
        assert_eq!(outcome.detached, 1);

        let notes = storage.fetch_recent_notes(5)?;
        let tags = notes
            .iter()
            .find(|summary| summary.id == note)
            .expect("note present")
            .tags
            .clone();
        assert!(!tags.iter().any(|tag| tag == "alpha"));
        Ok(())
    }

    #[test]
    fn purge_expired_trash_skips_when_retention_zero() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let note_id = storage.create_note("Trash Test", "body", false)?;
        storage.soft_delete_note(note_id)?;

        let purged = storage.purge_expired_trash(0)?;
        assert_eq!(purged, 0);

        let trashed = storage.fetch_trashed_notes(10)?;
        assert_eq!(trashed.len(), 1);
        assert_eq!(trashed[0].id, note_id);
        Ok(())
    }

    #[test]
    fn search_prefers_title_matches_over_body_hits() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let title_hit = storage.create_note("Nimbus Project Plan", "body text", false)?;
        let body_hit =
            storage.create_note("Weekly notes", "Discuss nimbus project rollout", false)?;

        let mut query = SearchQuery::default();
        query.terms = vec!["nimbus".into(), "project".into()];

        let results = storage.search_notes(&query, 10)?;
        assert!(results.len() >= 2, "expected at least two search results");
        assert_eq!(results[0].id, title_hit, "title match should rank first");
        assert_eq!(results[1].id, body_hit, "body-only match should follow");
        Ok(())
    }

    #[test]
    fn search_returns_snippet_for_title_only_matches() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let _note = storage.create_note("QuasarNotebook", "plain body", false)?;

        let mut query = SearchQuery::default();
        query.terms = vec!["QuasarNotebook".into()];

        let results = storage.search_notes(&query, 5)?;
        assert!(!results.is_empty(), "expected at least one result");
        let snippet = results[0]
            .snippet
            .as_deref()
            .unwrap_or_default()
            .to_lowercase();
        assert!(
            snippet.contains("quasarnotebook"),
            "expected snippet to include the title hit, got {snippet:?}"
        );
        Ok(())
    }

    #[test]
    fn regex_only_search_scans_beyond_recent_batch() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let target = storage.create_note("Very Old Regex Note", "foo123bar body", false)?;
        for i in 0..300 {
            let filler = storage.create_note(&format!("Pinned filler {i}"), "no match", true)?;
            // touch the filler title so updated_at bumps to ensure it stays ahead
            storage.rename_note_title(filler, &format!("Pinned filler {i} updated"))?;
        }

        let mut query = SearchQuery::default();
        query.regex_pattern = Some("foo[0-9]+bar".into());

        let results = storage.search_notes(&query, 5)?;
        assert!(!results.is_empty(), "expected regex match");
        assert_eq!(results[0].id, target);
        Ok(())
    }

    #[test]
    fn wal_health_check_runs() -> anyhow::Result<()> {
        let (_temp, storage) = init_storage()?;
        let stats = storage.run_wal_health_check()?;
        assert!(
            stats.busy_frames >= 0 && stats.wal_frames >= 0 && stats.checkpointed_frames >= 0,
            "expected non-negative wal stats, got {:?}",
            stats
        );
        Ok(())
    }
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
