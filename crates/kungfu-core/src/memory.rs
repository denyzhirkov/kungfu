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
        // Filter on metadata first (no bodies), then read only the survivors.
        let status_filter = filter
            .status
            .unwrap_or(kungfu_types::memory::MemoryStatus::Active);
        let ids: Vec<String> = self
            .store()
            .list_project_memory_meta()?
            .into_iter()
            .filter(|m| {
                m.status == status_filter
                    && filter.kind.is_none_or(|k| m.kind == k)
                    && filter
                        .tag
                        .as_ref()
                        .is_none_or(|tag| m.tags.iter().any(|t| t == tag))
                    && (!filter.pinned_only || m.pinned)
            })
            .map(|m| m.id)
            .collect();
        self.store().load_project_memory_bodies(&ids)
    }

    pub fn memory_show(&self, id: &str) -> Result<ProjectMemoryEntry> {
        self.store().get_project_memory(id)
    }

    pub fn memory_search(
        &self,
        query: &str,
        filter: &kungfu_memory::project_search::MemoryFilter,
    ) -> Result<Vec<(f64, ProjectMemoryEntry)>> {
        // Generate candidates from the inverted index, then read and score only
        // those bodies. An empty query degenerates to a filtered list, so load
        // all (rare path). Scoring semantics are unchanged from the full scan.
        let entries = if query.trim().is_empty() {
            self.store().load_project_memories()?
        } else {
            let ids = self.store().project_memory_candidates(query)?;
            self.store().load_project_memory_bodies(&ids)?
        };
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
