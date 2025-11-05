use std::cmp::Ordering;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;

use crate::config::AutoSaveConfig;
use crate::storage::StorageHandle;

const SNAPSHOT_EXTENSION: &str = "json";
const SNAPSHOT_TMP_EXTENSION: &str = "json.tmp";

#[derive(Debug, Clone)]
pub struct RecoverySnapshot {
    pub note_id: i64,
    pub saved_at: OffsetDateTime,
    pub body: String,
}

#[derive(Debug, Clone)]
pub enum AutoSaveStatus {
    Disabled,
    Inactive,
    Idle {
        note_id: i64,
        last_saved_at: Option<OffsetDateTime>,
    },
    Pending {
        note_id: i64,
        since: OffsetDateTime,
    },
    Error {
        note_id: i64,
        message: String,
        occurred_at: OffsetDateTime,
    },
}

#[derive(Debug, Clone)]
pub enum AutoSaveEvent {
    Saved {
        note_id: i64,
        timestamp: OffsetDateTime,
    },
    Error {
        note_id: i64,
        message: String,
    },
}

#[derive(Debug)]
pub struct AutoSaveRuntime {
    enabled: bool,
    crash_recovery: bool,
    debounce: Duration,
    journal_dir: PathBuf,
    session: Option<Session>,
}

#[derive(Debug)]
struct Session {
    note_id: i64,
    buffer: String,
    dirty: bool,
    dirty_since: Option<Instant>,
    dirty_since_wall: Option<OffsetDateTime>,
    last_saved_at: Option<OffsetDateTime>,
    last_error: Option<AutoSaveFailure>,
    snapshot_path: PathBuf,
}

#[derive(Debug, Clone)]
struct AutoSaveFailure {
    message: String,
    occurred_at: OffsetDateTime,
}

#[derive(Debug, Serialize, Deserialize)]
struct SnapshotRecord {
    note_id: i64,
    saved_at: i64,
    body: String,
}

impl AutoSaveRuntime {
    pub fn new(journal_dir: PathBuf, config: &AutoSaveConfig) -> Result<Self> {
        if config.crash_recovery {
            fs::create_dir_all(&journal_dir).with_context(|| {
                format!("creating autosave journal dir {}", journal_dir.display())
            })?;
        }
        Ok(Self {
            enabled: config.enabled,
            crash_recovery: config.crash_recovery,
            debounce: Duration::from_millis(config.debounce_ms),
            journal_dir,
            session: None,
        })
    }

    pub fn journal_dir(&self) -> &Path {
        &self.journal_dir
    }

    pub fn status(&self) -> AutoSaveStatus {
        if !self.enabled && !self.crash_recovery {
            return AutoSaveStatus::Disabled;
        }
        let Some(session) = &self.session else {
            return AutoSaveStatus::Inactive;
        };
        if let Some(failure) = &session.last_error {
            return AutoSaveStatus::Error {
                note_id: session.note_id,
                message: failure.message.clone(),
                occurred_at: failure.occurred_at,
            };
        }
        if session.dirty {
            let since = session
                .dirty_since_wall
                .unwrap_or_else(OffsetDateTime::now_utc);
            return AutoSaveStatus::Pending {
                note_id: session.note_id,
                since,
            };
        }
        AutoSaveStatus::Idle {
            note_id: session.note_id,
            last_saved_at: session.last_saved_at,
        }
    }

    pub fn has_active_session(&self) -> bool {
        self.session.is_some()
    }

    pub fn has_dirty_changes(&self) -> bool {
        self.session.as_ref().map(|s| s.dirty).unwrap_or(false)
    }

    pub fn start_session(
        &mut self,
        note_id: i64,
        initial_body: &str,
    ) -> Result<Option<RecoverySnapshot>> {
        let snapshot = if self.crash_recovery {
            self.read_snapshot(note_id)?
        } else {
            None
        };

        let buffer = snapshot
            .as_ref()
            .map(|snap| snap.body.clone())
            .unwrap_or_else(|| initial_body.to_string());

        let mut session = Session::new(note_id, buffer, self.snapshot_path(note_id));

        if snapshot.is_some() {
            session.mark_dirty_immediate(self.debounce);
        }

        self.session = Some(session);
        Ok(snapshot)
    }

    pub fn update_buffer(&mut self, note_id: i64, contents: &str) -> Result<()> {
        let Some(session) = self.session.as_mut() else {
            return Ok(());
        };
        if session.note_id != note_id {
            return Ok(());
        }
        if session.buffer == contents {
            return Ok(());
        }
        session.buffer.clear();
        session.buffer.push_str(contents);
        session.mark_dirty_now();
        if self.crash_recovery {
            Self::write_snapshot(&self.journal_dir, session)?;
        }
        Ok(())
    }

    pub fn poll(&mut self, storage: &StorageHandle) -> Result<Option<AutoSaveEvent>> {
        if !self.enabled {
            return Ok(None);
        }
        self.flush_internal(storage, FlushKind::Debounced)
    }

    pub fn flush_now(&mut self, storage: &StorageHandle) -> Result<Option<AutoSaveEvent>> {
        self.flush_internal(storage, FlushKind::Immediate)
    }

    pub fn end_session(&mut self, note_id: i64, clear_snapshot: bool) -> Result<()> {
        let Some(session) = self.session.as_ref() else {
            return Ok(());
        };
        if session.note_id != note_id {
            return Ok(());
        }
        let session = self.session.take().unwrap();
        if clear_snapshot && self.crash_recovery {
            Self::remove_snapshot_path(&session.snapshot_path)?;
        }
        drop(session);
        Ok(())
    }

    pub fn discard_snapshot(&self, note_id: i64) -> Result<()> {
        if !self.crash_recovery {
            return Ok(());
        }
        Self::remove_snapshot_path(&self.snapshot_path(note_id))
    }

    pub fn list_recovery(&self) -> Result<Vec<RecoverySnapshot>> {
        if !self.crash_recovery {
            return Ok(Vec::new());
        }
        let mut snapshots = Vec::new();
        let dir = match fs::read_dir(&self.journal_dir) {
            Ok(dir) => dir,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading autosave journal {}", self.journal_dir.display())
                })
            }
        };

        for entry in dir {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err) => {
                    tracing::warn!(?err, "skipping unreadable autosave entry");
                    continue;
                }
            };
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some(SNAPSHOT_EXTENSION) {
                continue;
            }
            match self.read_snapshot_path(&path) {
                Ok(snapshot) => snapshots.push(snapshot),
                Err(err) => {
                    tracing::warn!(?err, "failed to parse autosave snapshot {}", path.display());
                }
            }
        }

        snapshots.sort_by(|a, b| match b.saved_at.cmp(&a.saved_at) {
            Ordering::Equal => b.note_id.cmp(&a.note_id),
            other => other,
        });
        Ok(snapshots)
    }

    fn flush_internal(
        &mut self,
        storage: &StorageHandle,
        mode: FlushKind,
    ) -> Result<Option<AutoSaveEvent>> {
        let Some(session) = self.session.as_mut() else {
            return Ok(None);
        };
        if !session.dirty {
            return Ok(None);
        }
        if mode == FlushKind::Debounced {
            let ready = session
                .dirty_since
                .map(|since| since.elapsed() >= self.debounce)
                .unwrap_or(false);
            if !ready {
                return Ok(None);
            }
        }
        let timestamp = OffsetDateTime::now_utc();
        match storage.update_note_body(session.note_id, &session.buffer) {
            Ok(()) => {
                session.dirty = false;
                session.dirty_since = None;
                session.dirty_since_wall = None;
                session.last_saved_at = Some(timestamp);
                session.last_error = None;
                if self.crash_recovery {
                    Self::remove_snapshot_path(&session.snapshot_path)?;
                }
                Ok(Some(AutoSaveEvent::Saved {
                    note_id: session.note_id,
                    timestamp,
                }))
            }
            Err(err) => {
                let message = err.to_string();
                session.last_error = Some(AutoSaveFailure {
                    message: message.clone(),
                    occurred_at: timestamp,
                });
                if self.crash_recovery {
                    Self::write_snapshot(&self.journal_dir, session)?;
                }
                Ok(Some(AutoSaveEvent::Error {
                    note_id: session.note_id,
                    message,
                }))
            }
        }
    }

    fn write_snapshot(dir: &Path, session: &Session) -> Result<()> {
        let record = SnapshotRecord {
            note_id: session.note_id,
            saved_at: OffsetDateTime::now_utc().unix_timestamp(),
            body: session.buffer.clone(),
        };
        let json = serde_json::to_vec_pretty(&record).context("serialising autosave snapshot")?;
        fs::create_dir_all(dir)
            .with_context(|| format!("ensuring autosave dir {}", dir.display()))?;
        let final_path = session.snapshot_path.clone();
        let tmp_path = final_path.with_extension(SNAPSHOT_TMP_EXTENSION);
        fs::write(&tmp_path, &json).with_context(|| {
            format!("writing temporary autosave snapshot {}", tmp_path.display())
        })?;
        fs::rename(&tmp_path, &final_path).with_context(|| {
            format!(
                "atomically persisting autosave snapshot {}",
                final_path.display()
            )
        })?;
        Ok(())
    }

    fn read_snapshot(&self, note_id: i64) -> Result<Option<RecoverySnapshot>> {
        let path = self.snapshot_path(note_id);
        if !path.exists() {
            return Ok(None);
        }
        self.read_snapshot_path(&path).map(Some)
    }

    fn read_snapshot_path(&self, path: &Path) -> Result<RecoverySnapshot> {
        let raw = fs::read(path)
            .with_context(|| format!("reading autosave snapshot {}", path.display()))?;
        let record: SnapshotRecord = serde_json::from_slice(&raw)
            .with_context(|| format!("parsing autosave snapshot {}", path.display()))?;
        let saved_at = OffsetDateTime::from_unix_timestamp(record.saved_at)
            .unwrap_or_else(|_| OffsetDateTime::now_utc());
        Ok(RecoverySnapshot {
            note_id: record.note_id,
            saved_at,
            body: record.body,
        })
    }

    fn remove_snapshot_path(path: &Path) -> Result<()> {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => {
                Err(err).with_context(|| format!("removing autosave snapshot {}", path.display()))
            }
        }
    }

    fn snapshot_path(&self, note_id: i64) -> PathBuf {
        self.journal_dir
            .join(format!("note-{note_id}.{}", SNAPSHOT_EXTENSION))
    }
}

impl Session {
    fn new(note_id: i64, buffer: String, snapshot_path: PathBuf) -> Self {
        Self {
            note_id,
            buffer,
            dirty: false,
            dirty_since: None,
            dirty_since_wall: None,
            last_saved_at: None,
            last_error: None,
            snapshot_path,
        }
    }

    fn mark_dirty_now(&mut self) {
        self.dirty = true;
        self.dirty_since = Some(Instant::now());
        self.dirty_since_wall = Some(OffsetDateTime::now_utc());
        self.last_error = None;
    }

    fn mark_dirty_immediate(&mut self, debounce: Duration) {
        let now = Instant::now();
        self.dirty = true;
        self.dirty_since = Some(match now.checked_sub(debounce) {
            Some(instant) => instant,
            None => now,
        });
        self.dirty_since_wall = Some(OffsetDateTime::now_utc());
        self.last_error = None;
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq)]
enum FlushKind {
    Debounced,
    Immediate,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AutoSaveConfig, ConfigPaths, StorageOptions};
    use crate::storage;
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

    #[test]
    fn autosave_flushes_to_storage_and_clears_snapshot() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let paths = temp_paths(&temp);
        paths.ensure_directories()?;
        let storage_opts = storage_options(&paths);
        let storage = storage::init(&paths, &storage_opts)?;
        let note_id = storage.create_note("Test", "original", false)?;

        let journal_dir = paths.state_dir.join("autosave");
        let mut runtime = AutoSaveRuntime::new(
            journal_dir.clone(),
            &AutoSaveConfig {
                debounce_ms: 0,
                enabled: true,
                crash_recovery: true,
            },
        )?;

        runtime.start_session(note_id, "original")?;
        runtime.update_buffer(note_id, "updated body")?;

        let snapshot_path = journal_dir.join(format!("note-{note_id}.json"));
        assert!(snapshot_path.exists());

        let event = runtime.poll(&storage)?;
        match event {
            Some(AutoSaveEvent::Saved { .. }) => {}
            other => panic!("expected saved event, got {other:?}"),
        }

        assert!(!snapshot_path.exists());

        let records = storage.fetch_recent_notes(10)?;
        let updated = records
            .into_iter()
            .find(|note| note.id == note_id)
            .expect("note present");
        assert_eq!(updated.body, "updated body");

        Ok(())
    }

    #[test]
    fn autosave_reports_recovery_snapshots() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let paths = temp_paths(&temp);
        paths.ensure_directories()?;
        let storage_opts = storage_options(&paths);
        let storage = storage::init(&paths, &storage_opts)?;
        let note_id = storage.create_note("Test", "initial", false)?;

        let journal_dir = paths.state_dir.join("autosave");
        let config = AutoSaveConfig {
            debounce_ms: 0,
            enabled: true,
            crash_recovery: true,
        };

        {
            let mut runtime = AutoSaveRuntime::new(journal_dir.clone(), &config)?;
            runtime.start_session(note_id, "initial")?;
            runtime.update_buffer(note_id, "pending body")?;
            // Drop without flushing to simulate crash/restart.
        }

        let mut runtime = AutoSaveRuntime::new(journal_dir.clone(), &config)?;
        let snapshots = runtime.list_recovery()?;
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].note_id, note_id);
        assert_eq!(snapshots[0].body, "pending body");

        let recovered = runtime.start_session(note_id, "initial")?;
        assert!(recovered.is_some());
        assert_eq!(recovered.unwrap().body, "pending body");

        Ok(())
    }
}
