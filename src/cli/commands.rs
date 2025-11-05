use std::fmt::Write as _;
use std::io::{self, Read};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use rusqlite::{params, Connection, OptionalExtension};
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::app::App;
use crate::config::AppConfig;
use crate::search::{parse_query, regex_pattern_from_input};
use crate::storage::{NoteRecord, StorageHandle, TagRenameOutcome};

#[derive(Args, Debug, Clone)]
pub struct NewArgs {
    /// Title for the note (prompted if omitted)
    #[arg()]
    pub title: Option<String>,
    /// Provide the note body inline. If omitted, reads from stdin.
    #[arg(long)]
    pub body: Option<String>,
    /// Pin the new note
    #[arg(long)]
    pub pin: bool,
}

#[derive(Args, Debug, Clone)]
pub struct SearchArgs {
    /// Search query terms (supports tag:, title:, created:/updated: ranges)
    #[arg()]
    pub query: Vec<String>,
    /// Use regex search (not yet supported)
    #[arg(long)]
    pub regex: bool,
    /// Limit the number of results printed
    #[arg(long, default_value_t = 20)]
    pub limit: usize,
}

#[derive(Subcommand, Debug, Clone)]
pub enum TagCommand {
    /// Attach a tag to a note
    Add(TagAddArgs),
    /// Remove a tag from a note
    Remove(TagRemoveArgs),
    /// List tags associated with a note
    List(TagListArgs),
    /// Rename a tag across all notes
    Rename(TagRenameArgs),
    /// Merge one tag into another existing tag
    Merge(TagMergeArgs),
    /// Delete a tag from all notes
    Delete(TagDeleteArgs),
}

#[derive(Args, Debug, Clone)]
pub struct TagAddArgs {
    /// Note identifier
    pub note_id: i64,
    /// Tag to add (whitespace trimmed)
    pub tag: String,
}

#[derive(Args, Debug, Clone)]
pub struct TagRemoveArgs {
    /// Note identifier
    pub note_id: i64,
    /// Tag to remove
    pub tag: String,
}

#[derive(Args, Debug, Clone)]
pub struct TagListArgs {
    /// Note identifier
    pub note_id: i64,
}

#[derive(Args, Debug, Clone)]
pub struct TagRenameArgs {
    /// Existing tag name (will be renamed)
    pub from: String,
    /// New tag name
    pub to: String,
}

#[derive(Args, Debug, Clone)]
pub struct TagMergeArgs {
    /// Source tag that will be merged
    pub from: String,
    /// Target tag that must already exist
    pub into: String,
}

#[derive(Args, Debug, Clone)]
pub struct TagDeleteArgs {
    /// Tag name to delete
    pub tag: String,
}

#[derive(Args, Debug, Clone)]
pub struct TagArgs {
    #[command(subcommand)]
    pub command: TagCommand,
}

pub fn run_tui(app: &mut App) -> Result<()> {
    app.run()
}

pub fn new_note(_config: Arc<AppConfig>, storage: StorageHandle, args: NewArgs) -> Result<()> {
    let mut title = match args.title {
        Some(t) => t,
        None => prompt("Title")?,
    };
    title = title.trim().to_owned();
    if title.is_empty() {
        bail!("note title cannot be empty");
    }
    let body = if let Some(body) = args.body {
        body
    } else {
        read_stdin()?.unwrap_or_else(|| String::from(""))
    };

    let note_id = storage
        .create_note(&title, &body, args.pin)
        .context("creating note")?;
    println!(
        "Created note #{note_id}{}",
        if args.pin { " (pinned)" } else { "" }
    );
    Ok(())
}

pub fn search_notes(
    _config: Arc<AppConfig>,
    storage: StorageHandle,
    args: SearchArgs,
) -> Result<()> {
    let output = run_search(&storage, &args)?;
    print!("{output}");
    Ok(())
}

fn run_search(storage: &StorageHandle, args: &SearchArgs) -> Result<String> {
    let raw_query = args.query.join(" ");
    let trimmed = raw_query.trim();
    if trimmed.is_empty() {
        bail!("search query cannot be empty");
    }

    let mut query = parse_query(trimmed);
    if !query.has_terms() && !query.has_filters() {
        bail!("search query must contain terms or filters");
    }
    if args.regex {
        query.regex_pattern = regex_pattern_from_input(trimmed);
    }

    let mut storage_query = query.clone();
    if args.regex && storage_query.regex_pattern.is_some() {
        storage_query.terms.clear();
        storage_query.title_terms.clear();
    }

    let results = storage
        .search_notes(&storage_query, args.limit)
        .context("executing search")?;
    Ok(format_search_results(&results))
}

fn format_search_results(notes: &[NoteRecord]) -> String {
    if notes.is_empty() {
        return "No matches found.\n".to_string();
    }
    let mut out = String::new();
    for note in notes {
        let mut headline = format!("#{}  {}", note.id, note.title);
        if note.pinned {
            headline.push_str("  [PINNED]");
        }
        if note.archived {
            headline.push_str("  [ARCHIVED]");
        }
        let _ = writeln!(&mut out, "{headline}");
        let _ = writeln!(
            &mut out,
            "    updated {}",
            format_timestamp(note.updated_at)
        );
        if !note.tags.is_empty() {
            let _ = writeln!(&mut out, "    tags    {}", format_tags(&note.tags));
        }
        if let Some(snippet) = build_snippet(note, 2) {
            let _ = writeln!(&mut out, "    {snippet}");
        }
        out.push('\n');
    }
    out
}

pub fn handle_tag_command(
    _config: Arc<AppConfig>,
    storage: StorageHandle,
    args: TagArgs,
) -> Result<()> {
    match args.command {
        TagCommand::Add(args) => tag_add(&storage, args),
        TagCommand::Remove(args) => tag_remove(&storage, args),
        TagCommand::List(args) => tag_list(&storage, args),
        TagCommand::Rename(args) => tag_rename(&storage, args),
        TagCommand::Merge(args) => tag_merge(&storage, args),
        TagCommand::Delete(args) => tag_delete(&storage, args),
    }
}

fn prompt(label: &str) -> Result<String> {
    use std::io::Write;
    let mut stdout = io::stdout();
    write!(stdout, "{}: ", label)?;
    stdout.flush()?;
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    Ok(input.trim_end().to_owned())
}

fn read_stdin() -> Result<Option<String>> {
    if atty::is(atty::Stream::Stdin) {
        return Ok(None);
    }
    let mut buf = String::new();
    io::stdin().read_to_string(&mut buf)?;
    Ok(Some(buf))
}

fn tag_add(storage: &StorageHandle, args: TagAddArgs) -> Result<()> {
    let note_id = args.note_id;
    let mut tag = args.tag.trim().to_string();
    if tag.is_empty() {
        bail!("tag cannot be empty");
    }
    if tag.len() > 64 {
        tag.truncate(64);
    }

    let conn = storage.connect().context("opening DB connection")?;
    let note_title = ensure_note_exists(&conn, note_id)?;
    drop(conn);

    storage
        .add_tag_to_note(note_id, &tag)
        .with_context(|| format!("adding tag '{tag}' to note {note_id}"))?;
    println!(
        "Added tag '{}' to note #{} ({})",
        tag,
        note_id,
        note_title.unwrap_or_else(|| "<untitled>".into())
    );
    Ok(())
}

fn tag_remove(storage: &StorageHandle, args: TagRemoveArgs) -> Result<()> {
    let note_id = args.note_id;
    let tag = args.tag.trim();
    if tag.is_empty() {
        bail!("tag cannot be empty");
    }
    let conn = storage.connect().context("opening DB connection")?;
    let note_title = ensure_note_exists(&conn, note_id)?;
    drop(conn);

    storage
        .remove_tag_from_note(note_id, tag)
        .with_context(|| format!("removing tag '{tag}' from note {note_id}"))?;
    println!(
        "Removed tag '{}' from note #{} ({})",
        tag,
        note_id,
        note_title.unwrap_or_else(|| "<untitled>".into())
    );
    Ok(())
}

fn tag_list(storage: &StorageHandle, args: TagListArgs) -> Result<()> {
    let note_id = args.note_id;
    let conn = storage.connect().context("opening DB connection")?;
    let title = ensure_note_exists(&conn, note_id)?;
    let mut stmt = conn
        .prepare(
            "SELECT t.name
             FROM tags t
             JOIN note_tags nt ON nt.tag_id = t.id
             WHERE nt.note_id = ?
             ORDER BY t.name COLLATE NOCASE",
        )
        .context("preparing tag list query")?;
    let rows = stmt
        .query_map([note_id], |row| row.get::<_, String>(0))
        .context("querying note tags")?;

    println!(
        "Tags for note #{} ({})",
        note_id,
        title.unwrap_or_else(|| "<untitled>".into())
    );
    let mut count = 0;
    for row in rows {
        let tag = row?;
        println!("- {}", tag);
        count += 1;
    }
    if count == 0 {
        println!("(no tags)");
    }
    Ok(())
}

fn tag_rename(storage: &StorageHandle, args: TagRenameArgs) -> Result<()> {
    let from = args.from.trim();
    if from.is_empty() {
        bail!("source tag cannot be empty");
    }
    let mut to = args.to.trim().to_string();
    if to.is_empty() {
        bail!("destination tag cannot be empty");
    }
    if to.len() > 64 {
        to.truncate(64);
    }
    if from.eq_ignore_ascii_case(&to) {
        bail!("source and destination tags are the same");
    }

    let outcome = storage
        .rename_tag(from, &to)
        .with_context(|| format!("renaming tag '{from}' to '{to}'"))?;
    match outcome {
        TagRenameOutcome::Renamed { from, to } => {
            println!("Renamed tag '{from}' to '{to}'");
        }
        TagRenameOutcome::Merged {
            from,
            to,
            reassigned,
        } => {
            println!(
                "Merged tag '{from}' into '{to}' (relinked {} note{})",
                reassigned,
                if reassigned == 1 { "" } else { "s" }
            );
        }
    }
    Ok(())
}

fn tag_merge(storage: &StorageHandle, args: TagMergeArgs) -> Result<()> {
    let from = args.from.trim();
    if from.is_empty() {
        bail!("source tag cannot be empty");
    }
    let mut into = args.into.trim().to_string();
    if into.is_empty() {
        bail!("target tag cannot be empty");
    }
    if into.len() > 64 {
        into.truncate(64);
    }
    if from.eq_ignore_ascii_case(&into) {
        bail!("source and target tags must differ");
    }

    if !storage
        .tag_exists(&into)
        .with_context(|| format!("checking if tag '{into}' exists"))?
    {
        bail!("target tag '{into}' does not exist");
    }

    let outcome = storage
        .rename_tag(from, &into)
        .with_context(|| format!("merging tag '{from}' into '{into}'"))?;
    match outcome {
        TagRenameOutcome::Merged {
            from,
            to,
            reassigned,
        } => {
            println!(
                "Merged tag '{from}' into '{to}' (relinked {} note{})",
                reassigned,
                if reassigned == 1 { "" } else { "s" }
            );
        }
        TagRenameOutcome::Renamed { from, to } => {
            println!("Renamed tag '{from}' to '{to}' (target was missing, renamed instead)");
        }
    }
    Ok(())
}

fn tag_delete(storage: &StorageHandle, args: TagDeleteArgs) -> Result<()> {
    let tag = args.tag.trim();
    if tag.is_empty() {
        bail!("tag cannot be empty");
    }
    let outcome = storage
        .delete_tag(tag)
        .with_context(|| format!("deleting tag '{tag}'"))?;
    let plural = if outcome.detached == 1 { "" } else { "s" };
    println!(
        "Deleted tag '{}' (removed from {} note{})",
        outcome.tag, outcome.detached, plural
    );
    Ok(())
}

fn ensure_note_exists(conn: &Connection, note_id: i64) -> Result<Option<String>> {
    let title: Option<String> = conn
        .query_row(
            "SELECT title FROM notes WHERE id = ?1 AND deleted_at IS NULL",
            params![note_id],
            |row| row.get(0),
        )
        .optional()
        .context("checking note existence")?;
    if title.is_none() {
        bail!("note #{note_id} not found");
    }
    Ok(title)
}

fn build_snippet(note: &NoteRecord, fallback_lines: usize) -> Option<String> {
    if let Some(snippet) = note.snippet.as_ref() {
        let cleaned = snippet.replace('\n', " ").trim().to_string();
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    if fallback_lines == 0 {
        return None;
    }
    let mut segments = Vec::new();
    for line in note.body.lines().take(fallback_lines) {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            segments.push(trimmed.to_string());
        }
    }
    if segments.is_empty() {
        None
    } else {
        let snippet = segments.join(" ");
        let truncated = snippet.chars().take(160).collect::<String>();
        Some(truncated)
    }
}

fn format_tags(tags: &[String]) -> String {
    tags.iter()
        .map(|tag| format!("#{}", tag))
        .collect::<Vec<_>>()
        .join(" ")
}

fn format_timestamp(epoch: i64) -> String {
    OffsetDateTime::from_unix_timestamp(epoch)
        .map(|dt| dt.format(&Rfc3339).unwrap_or_else(|_| epoch.to_string()))
        .unwrap_or_else(|_| epoch.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConfigPaths, StorageOptions};
    use crate::storage;
    use tempfile::TempDir;

    type TestResult<T = ()> = Result<T>;

    #[test]
    fn cli_search_filters_tags_and_marks_pinned() -> TestResult {
        let (_temp_dir, storage) = setup_storage()?;
        let project_id = storage.create_note("Project Plan", "Timeline overview", true)?;
        storage.add_tag_to_note(project_id, "project")?;

        let misc_id = storage.create_note("Misc Note", "Just chatter", false)?;
        storage.add_tag_to_note(misc_id, "misc")?;

        let args = SearchArgs {
            query: vec!["tag:project".into()],
            regex: false,
            limit: 10,
        };
        let output = run_search(&storage, &args)?;

        assert!(output.contains("Project Plan"));
        assert!(output.contains("[PINNED]"));
        assert!(!output.contains("Misc Note"));
        Ok(())
    }

    #[test]
    fn cli_search_supports_regex_filtering() -> TestResult {
        let (_temp_dir, storage) = setup_storage()?;
        let regex_id = storage.create_note("Regex Note", "alpha foo123bar omega", false)?;
        storage.add_tag_to_note(regex_id, "regex")?;

        let miss_id = storage.create_note("Regex Miss", "alpha foozzz omega", false)?;
        storage.add_tag_to_note(miss_id, "regex")?;

        let args = SearchArgs {
            query: vec!["tag:regex".into(), "foo[0-9]+bar".into()],
            regex: true,
            limit: 10,
        };
        let output = run_search(&storage, &args)?;

        assert!(output.contains("Regex Note"));
        assert!(!output.contains("Regex Miss"));
        Ok(())
    }

    #[test]
    fn cli_tag_rename_updates_tag() -> TestResult {
        let (_temp_dir, storage) = setup_storage()?;
        let note_id = storage.create_note("Rename target", "body", false)?;
        storage.add_tag_to_note(note_id, "alpha")?;

        tag_rename(
            &storage,
            TagRenameArgs {
                from: "alpha".into(),
                to: "beta".into(),
            },
        )?;

        let tags = storage
            .fetch_recent_notes(5)?
            .into_iter()
            .find(|note| note.id == note_id)
            .expect("note present")
            .tags;
        assert!(tags.contains(&"beta".to_string()));
        assert!(!tags.iter().any(|tag| tag == "alpha"));
        Ok(())
    }

    #[test]
    fn cli_tag_merge_combines_tags() -> TestResult {
        let (_temp_dir, storage) = setup_storage()?;
        let alpha_note = storage.create_note("Alpha note", "body", false)?;
        let beta_note = storage.create_note("Beta note", "body", false)?;
        storage.add_tag_to_note(alpha_note, "alpha")?;
        storage.add_tag_to_note(beta_note, "beta")?;

        tag_merge(
            &storage,
            TagMergeArgs {
                from: "alpha".into(),
                into: "beta".into(),
            },
        )?;

        let notes = storage.fetch_recent_notes(10)?;
        let alpha_tags = notes
            .iter()
            .find(|note| note.id == alpha_note)
            .expect("alpha note present")
            .tags
            .clone();
        assert!(alpha_tags.contains(&"beta".to_string()));
        assert!(!alpha_tags.iter().any(|tag| tag == "alpha"));

        // The beta note should still have beta only.
        let beta_tags = notes
            .iter()
            .find(|note| note.id == beta_note)
            .expect("beta note present")
            .tags
            .clone();
        assert!(beta_tags.contains(&"beta".to_string()));
        assert!(!beta_tags.iter().any(|tag| tag == "alpha"));

        Ok(())
    }

    #[test]
    fn cli_tag_delete_removes_tag_globally() -> TestResult {
        let (_temp_dir, storage) = setup_storage()?;
        let note_id = storage.create_note("Delete tag note", "body", false)?;
        storage.add_tag_to_note(note_id, "obsolete")?;

        tag_delete(
            &storage,
            TagDeleteArgs {
                tag: "obsolete".into(),
            },
        )?;

        let tags = storage
            .fetch_recent_notes(5)?
            .into_iter()
            .find(|note| note.id == note_id)
            .expect("note present")
            .tags;
        assert!(!tags.iter().any(|tag| tag == "obsolete"));

        let all_tags = storage.list_all_tags()?;
        assert!(!all_tags.iter().any(|tag| tag == "obsolete"));
        Ok(())
    }

    fn setup_storage() -> TestResult<(TempDir, StorageHandle)> {
        let temp = TempDir::new().context("creating temp dir")?;
        let root = temp.path();
        let paths = ConfigPaths {
            config_dir: root.join("config"),
            config_file: root.join("config/config.toml"),
            data_dir: root.join("data"),
            database_path: root.join("data/notes.db"),
            cache_dir: root.join("cache"),
            backup_dir: root.join("backups"),
            log_dir: root.join("logs"),
            state_dir: root.join("state"),
        };
        let mut storage_opts = StorageOptions::default();
        storage_opts.database_path = paths.database_path.clone();
        storage_opts.backup_dir = paths.backup_dir.clone();
        storage_opts.backup_on_exit = false;

        let handle = storage::init(&paths, &storage_opts)?;
        Ok((temp, handle))
    }
}
