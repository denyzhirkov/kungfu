use crate::KungfuService;
use anyhow::Result;
use kungfu_types::memory::ProjectMemoryEntry;

impl KungfuService {
    #[allow(clippy::too_many_arguments)]
    pub fn memory_add(
        &self,
        kind: kungfu_types::memory::ProjectMemoryKind,
        content: &str,
        title: Option<&str>,
        tags: Vec<String>,
        related_files: Vec<String>,
        related_symbols: Vec<String>,
        pinned: bool,
    ) -> Result<ProjectMemoryEntry> {
        let id = self.store().next_project_memory_id()?;
        let now = chrono::Utc::now().to_rfc3339();
        let entry = ProjectMemoryEntry {
            id,
            kind,
            title: title.map(|s| s.to_string()),
            content: content.to_string(),
            tags,
            related_files,
            related_symbols,
            pinned,
            status: kungfu_types::memory::MemoryStatus::Active,
            supersedes: None,
            created_at: now.clone(),
            updated_at: now,
        };
        self.store().add_project_memory(entry)
    }

    pub fn memory_list(
        &self,
        filter: &kungfu_memory::project_search::MemoryFilter,
    ) -> Result<Vec<ProjectMemoryEntry>> {
        let entries = self.store().load_project_memories()?;
        let status_filter = filter
            .status
            .unwrap_or(kungfu_types::memory::MemoryStatus::Active);
        let filtered: Vec<_> = entries
            .into_iter()
            .filter(|e| {
                if e.status != status_filter {
                    return false;
                }
                if let Some(kind) = filter.kind {
                    if e.kind != kind {
                        return false;
                    }
                }
                if let Some(ref tag) = filter.tag {
                    if !e.tags.iter().any(|t| t == tag) {
                        return false;
                    }
                }
                if filter.pinned_only && !e.pinned {
                    return false;
                }
                true
            })
            .collect();
        Ok(filtered)
    }

    pub fn memory_show(&self, id: &str) -> Result<ProjectMemoryEntry> {
        let entries = self.store().load_project_memories()?;
        entries
            .into_iter()
            .find(|e| e.id == id)
            .ok_or_else(|| anyhow::anyhow!("memory entry not found: {}", id))
    }

    pub fn memory_search(
        &self,
        query: &str,
        filter: &kungfu_memory::project_search::MemoryFilter,
    ) -> Result<Vec<(f64, ProjectMemoryEntry)>> {
        let entries = self.store().load_project_memories()?;
        Ok(kungfu_memory::project_search::search_project_memory(
            query, &entries, filter,
        ))
    }

    pub fn memory_update(
        &self,
        id: &str,
        content: Option<&str>,
        title: Option<&str>,
        tags: Option<Vec<String>>,
        pinned: Option<bool>,
    ) -> Result<ProjectMemoryEntry> {
        self.store().update_project_memory(id, |e| {
            if let Some(c) = content {
                e.content = c.to_string();
            }
            if let Some(t) = title {
                e.title = Some(t.to_string());
            }
            if let Some(t) = tags {
                e.tags = t;
            }
            if let Some(p) = pinned {
                e.pinned = p;
            }
        })
    }

    pub fn memory_archive(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.store().archive_project_memory(id)
    }

    pub fn memory_remove(&self, id: &str) -> Result<()> {
        self.store().remove_project_memory(id)
    }

    pub fn memory_pin(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.store().update_project_memory(id, |e| {
            e.pinned = true;
        })
    }

    pub fn memory_unpin(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.store().update_project_memory(id, |e| {
            e.pinned = false;
        })
    }
}
