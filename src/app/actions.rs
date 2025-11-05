use anyhow::Result;

use crate::storage::{StorageHandle, TagDeleteOutcome, TagRenameOutcome};

pub struct ActionDispatcher<'a> {
    storage: &'a StorageHandle,
}

impl<'a> ActionDispatcher<'a> {
    pub fn new(storage: &'a StorageHandle) -> Self {
        Self { storage }
    }

    pub fn toggle_pin(&self, note_id: i64, pin: bool) -> Result<()> {
        self.storage.set_note_pinned(note_id, pin)
    }

    pub fn toggle_archive(&self, note_id: i64, archive: bool) -> Result<()> {
        self.storage.set_note_archived(note_id, archive)
    }

    pub fn add_tag(&self, note_id: i64, tag: &str) -> Result<()> {
        self.storage.add_tag_to_note(note_id, tag)
    }

    pub fn remove_tag(&self, note_id: i64, tag: &str) -> Result<()> {
        self.storage.remove_tag_from_note(note_id, tag)
    }

    pub fn rename_tag(&self, current: &str, new_name: &str) -> Result<TagRenameOutcome> {
        self.storage.rename_tag(current, new_name)
    }

    pub fn delete_tag(&self, tag: &str) -> Result<TagDeleteOutcome> {
        self.storage.delete_tag(tag)
    }

    pub fn rename_note(&self, note_id: i64, title: &str) -> Result<()> {
        self.storage.rename_note_title(note_id, title)
    }

    pub fn soft_delete(&self, note_id: i64) -> Result<()> {
        self.storage.soft_delete_note(note_id)
    }

    pub fn restore_all_trash(&self) -> Result<usize> {
        self.storage.restore_all_trash()
    }

    pub fn purge_all_trash(&self) -> Result<usize> {
        self.storage.purge_all_trash()
    }
}
