use crate::KungfuService;
use anyhow::Result;
use kungfu_types::budget::Budget;
use kungfu_types::memory::{MemoryStatus, ProjectMemoryEntry};

/// Budget-capped project-memory search result. `total` is the match count before
/// the cap; `note` is set iff results were cut, `status`/`hint` iff nothing matched.
#[derive(Debug, Clone)]
pub struct MemorySearchResult {
    pub results: Vec<(f64, ProjectMemoryEntry)>,
    pub total: usize,
    pub note: Option<String>,
    pub status: Option<&'static str>,
    pub hint: Option<String>,
}

impl MemorySearchResult {
    /// Cap scored matches to the budget (auto resolves to small).
    pub fn from_scored(results: Vec<(f64, ProjectMemoryEntry)>, budget: Budget) -> Self {
        let total = results.len();
        Self::capped(results, total, budget)
    }

    /// Entry cap for a budget (auto resolves to small).
    fn limit(budget: Budget) -> usize {
        match budget {
            Budget::Tiny => 3,
            Budget::Small | Budget::Auto => 5,
            Budget::Medium => 10,
            Budget::Full => 25,
        }
    }

    /// `results` is already cut to at most `limit(budget)`; `total` is the pre-cap match count.
    fn capped(mut results: Vec<(f64, ProjectMemoryEntry)>, total: usize, budget: Budget) -> Self {
        if total == 0 {
            return Self {
                results,
                total,
                note: None,
                status: Some("no_match"),
                hint: Some(
                    "no memory matched — try broader terms or drop the kind/tag/pinned filter; with an empty query this means no entries match the filters (or the store is empty — add one with memory_add)"
                        .to_string(),
                ),
            };
        }
        let limit = Self::limit(budget);
        let note = (total > limit).then(|| {
            let action = if budget == Budget::Full {
                "narrow with kind/tag"
            } else {
                "raise budget or narrow with kind/tag"
            };
            format!("showing {limit} of {total} — {action}")
        });
        results.truncate(limit);
        Self {
            results,
            total,
            note,
            status: None,
            hint: None,
        }
    }
}

/// Health + repair summary for the manual memory store (`kungfu doctor`).
#[derive(Debug, Clone, serde::Serialize)]
pub struct MemoryDoctor {
    pub total: usize,
    pub active: usize,
    pub archived: usize,
    pub legacy_present: bool,
    pub absorbed: Option<kungfu_storage::AbsorbReport>,
}

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
        budget: Budget,
    ) -> Result<MemorySearchResult> {
        if query.trim().is_empty() {
            return self.memory_list_capped(filter, budget);
        }
        // Generate candidates from the inverted index, then read and score only
        // those bodies. Scoring semantics are unchanged from the full scan.
        let ids = self.store().project_memory_candidates(query)?;
        let entries = self.store().load_project_memory_bodies(&ids)?;
        Ok(MemorySearchResult::from_scored(
            kungfu_memory::project_search::search_project_memory(query, &entries, filter),
            budget,
        ))
    }

    /// Empty-query search = filtered list: pinned first, then newest. Ordering and filtering
    /// run on metadata; only the capped survivors have their bodies read.
    fn memory_list_capped(
        &self,
        filter: &kungfu_memory::project_search::MemoryFilter,
        budget: Budget,
    ) -> Result<MemorySearchResult> {
        let status = filter.status.unwrap_or(MemoryStatus::Active);
        let mut metas: Vec<_> = self
            .store()
            .list_project_memory_meta()?
            .into_iter()
            .filter(|m| {
                m.status == status
                    && filter.kind.is_none_or(|k| m.kind == k)
                    && filter
                        .tag
                        .as_ref()
                        .is_none_or(|tag| m.tags.iter().any(|t| t == tag))
                    && (!filter.pinned_only || m.pinned)
            })
            .collect();
        metas.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then_with(|| b.updated_at.cmp(&a.updated_at))
        });
        let total = metas.len();
        let ids: Vec<String> = metas
            .into_iter()
            .take(MemorySearchResult::limit(budget))
            .map(|m| m.id)
            .collect();
        let mut by_id: std::collections::HashMap<String, ProjectMemoryEntry> = self
            .store()
            .load_project_memory_bodies(&ids)?
            .into_iter()
            .map(|e| (e.id.clone(), e))
            .collect();
        let results = ids
            .iter()
            .filter_map(|id| by_id.remove(id))
            .map(|e| (0.0, e))
            .collect();
        Ok(MemorySearchResult::capped(results, total, budget))
    }

    pub fn memory_update(
        &self,
        id: &str,
        content: Option<&str>,
        title: Option<&str>,
        tags: Option<Vec<String>>,
        pinned: Option<bool>,
        status: Option<MemoryStatus>,
    ) -> Result<ProjectMemoryEntry> {
        self.store().update_project_memory(id, |e| {
            if let Some(s) = status {
                e.status = s;
            }
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

    /// Diagnose the manual memory store; with `fix`, absorb a stray legacy file.
    pub fn memory_doctor(&self, fix: bool) -> Result<MemoryDoctor> {
        let absorbed = if fix && self.store().project_memory_legacy_present() {
            self.store().absorb_project_memory_legacy()?
        } else {
            None
        };
        let metas = self.store().list_project_memory_meta()?;
        let archived = metas
            .iter()
            .filter(|m| m.status == MemoryStatus::Archived)
            .count();
        let active = metas
            .iter()
            .filter(|m| m.status == MemoryStatus::Active)
            .count();
        Ok(MemoryDoctor {
            total: metas.len(),
            active,
            archived,
            legacy_present: self.store().project_memory_legacy_present(),
            absorbed,
        })
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
