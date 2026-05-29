use crate::KungfuService;
use anyhow::{bail, Result};
use kungfu_rank::build_context_packet;
use kungfu_types::budget::Budget;
use kungfu_types::context::ContextPacket;
use kungfu_types::symbol::Symbol;
use tracing::info;

impl KungfuService {
    /// Get git history for a file: recent commits.
    pub fn file_history(&self, path: &str, max_entries: usize) -> Result<serde_json::Value> {
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }
        let entries = kungfu_git::file_log(&self.project.root, path, max_entries)?;
        let items: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "hash": e.hash,
                    "date": e.date,
                    "author": e.author,
                    "message": e.message,
                })
            })
            .collect();
        Ok(serde_json::json!({ "path": path, "commits": items }))
    }

    /// Get git blame for a symbol: who changed its code and why.
    pub fn symbol_history(&self, name: &str) -> Result<serde_json::Value> {
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }
        let sym = self.search().get_symbol(name)?;
        let symbol = match sym {
            Some(s) => s,
            None => {
                return Ok(serde_json::json!({ "error": format!("Symbol '{}' not found", name) }))
            }
        };

        let blame = kungfu_git::blame_lines(
            &self.project.root,
            &symbol.path,
            symbol.span.start_line,
            symbol.span.end_line,
        )
        .unwrap_or_default();

        let blame_items: Vec<_> = blame
            .iter()
            .map(|b| {
                serde_json::json!({
                    "hash": b.hash,
                    "author": b.author,
                    "date": b.date,
                    "summary": b.summary,
                })
            })
            .collect();

        let log = kungfu_git::file_log(&self.project.root, &symbol.path, 5).unwrap_or_default();
        let log_items: Vec<_> = log
            .iter()
            .map(|e| {
                serde_json::json!({
                    "hash": e.hash,
                    "date": e.date,
                    "author": e.author,
                    "message": e.message,
                })
            })
            .collect();

        Ok(serde_json::json!({
            "symbol": name,
            "path": symbol.path,
            "lines": format!("{}-{}", symbol.span.start_line, symbol.span.end_line),
            "blame": blame_items,
            "recent_commits": log_items,
        }))
    }

    pub fn change_timeline(
        &self,
        target: &str,
        budget: Budget,
    ) -> Result<Vec<kungfu_types::context::HistoryEvent>> {
        let budget = self.resolve_budget(budget);
        let mut events = Vec::new();

        // Find the file for this target (symbol name or file path)
        let search = self.search();
        let file_path = if let Some(sym) = search.get_symbol(target)? {
            sym.path.clone()
        } else {
            // Try as a file path
            let files = self.store().load_files()?;
            match files.iter().find(|f| f.path.contains(target)) {
                Some(f) => f.path.clone(),
                None => return Ok(events),
            }
        };

        if !kungfu_git::is_git_repo(&self.project.root) {
            return Ok(events);
        }

        // Git log
        let max_entries = match budget {
            Budget::Tiny => 3,
            Budget::Small => 5,
            Budget::Medium => 10,
            _ => 20,
        };
        let log =
            kungfu_git::file_log(&self.project.root, &file_path, max_entries).unwrap_or_default();

        if let Some(first) = log.last() {
            events.push(kungfu_types::context::HistoryEvent {
                event_type: "introduced".to_string(),
                target: target.to_string(),
                detail: format!("First appeared: {} by {}", first.message, first.author),
                date: Some(first.date.clone()),
            });
        }

        // Churn analysis
        let churn = kungfu_git::file_commit_counts(&self.project.root).unwrap_or_default();
        if let Some(count) = churn
            .iter()
            .find(|(p, _)| p.contains(&file_path))
            .map(|(_, c)| *c)
        {
            let avg = if churn.is_empty() {
                1
            } else {
                churn.values().copied().sum::<usize>() / churn.len()
            };
            if count > avg * 2 {
                events.push(kungfu_types::context::HistoryEvent {
                    event_type: "high_churn".to_string(),
                    target: file_path.clone(),
                    detail: format!("{} commits (project avg: {})", count, avg),
                    date: None,
                });
            }
        }

        // Recent changes
        for entry in log.iter().take(3) {
            events.push(kungfu_types::context::HistoryEvent {
                event_type: "recent_change".to_string(),
                target: target.to_string(),
                detail: format!("{}: {}", entry.author, entry.message),
                date: Some(entry.date.clone()),
            });
        }

        // Decision references from memory
        let memories = self.store().load_memories().unwrap_or_default();
        for mem in &memories {
            if mem.kind == kungfu_types::memory::MemoryKind::Decision {
                let related = mem.path == file_path
                    || mem
                        .anchors
                        .iter()
                        .any(|a| target.to_lowercase().contains(a));
                if related {
                    events.push(kungfu_types::context::HistoryEvent {
                        event_type: "decision_ref".to_string(),
                        target: mem.path.clone(),
                        detail: mem.text.chars().take(200).collect(),
                        date: None,
                    });
                }
            }
        }

        Ok(events)
    }

    /// Build a context packet for a specific commit: scored symbols overlapping the commit's
    /// hunks, plus commit metadata. Used by the `commit-context` CLI/MCP tool.
    pub fn commit_context(&self, hash: &str, budget: Budget) -> Result<ContextPacket> {
        self.ensure_fresh_index()?;
        let budget = self.resolve_budget(budget);
        if !kungfu_git::is_git_repo(&self.project.root) {
            anyhow::bail!("not a git repository");
        }

        let meta = kungfu_git::commit_meta(&self.project.root, hash)?;
        let changed_lines = kungfu_git::commit_changed_lines(&self.project.root, hash)?;

        let all_symbols = self.search().get_all_symbols()?;
        let mut seed: Vec<(Symbol, f64)> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

        for (file_path, ranges) in &changed_lines {
            for sym in &all_symbols {
                if sym.path != *file_path {
                    continue;
                }
                let overlaps = ranges
                    .iter()
                    .any(|&(start, end)| sym.span.start_line <= end && sym.span.end_line >= start);
                if overlaps && seen_ids.insert(sym.id.clone()) {
                    seed.push((sym.clone(), 0.9));
                }
            }
        }

        let query = format!("commit {}: {} ({})", &meta.hash, meta.message, meta.author);
        let mut packet = kungfu_rank::build_context_packet(&query, seed, budget);
        packet.history.push(kungfu_types::context::HistoryEvent {
            event_type: "commit".to_string(),
            target: meta.hash.clone(),
            detail: format!("{}: {}", meta.author, meta.message),
            date: Some(meta.date.clone()),
        });
        packet.changed_files = changed_lines.into_iter().map(|(f, _)| f).collect();
        Ok(packet)
    }

    /// Build a context packet covering all commits in a GitHub PR.
    /// Requires the `gh` CLI in PATH and the repo to have a GitHub remote.
    pub fn pr_context(&self, pr_num: u32, budget: Budget) -> Result<ContextPacket> {
        self.ensure_fresh_index()?;
        let budget = self.resolve_budget(budget);

        // Fetch commit list via gh.
        let output = std::process::Command::new("gh")
            .args(["pr", "view", &pr_num.to_string(), "--json", "commits"])
            .current_dir(&self.project.root)
            .output()
            .map_err(|e| anyhow::anyhow!("`gh` CLI not available: {}", e))?;
        if !output.status.success() {
            anyhow::bail!(
                "gh pr view failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let v: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        let hashes: Vec<String> = v
            .get("commits")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c.get("oid").and_then(|o| o.as_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        if hashes.is_empty() {
            anyhow::bail!("PR #{} has no commits or gh returned no data", pr_num);
        }

        // Merge per-commit seeds.
        let all_symbols = self.search().get_all_symbols()?;
        let mut seed: Vec<(Symbol, f64)> = Vec::new();
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut changed_files: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut events: Vec<kungfu_types::context::HistoryEvent> = Vec::new();

        for hash in &hashes {
            let meta = match kungfu_git::commit_meta(&self.project.root, hash) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let cl = kungfu_git::commit_changed_lines(&self.project.root, hash).unwrap_or_default();
            for (file_path, ranges) in &cl {
                changed_files.insert(file_path.clone());
                for sym in &all_symbols {
                    if sym.path != *file_path {
                        continue;
                    }
                    let overlaps = ranges.iter().any(|&(start, end)| {
                        sym.span.start_line <= end && sym.span.end_line >= start
                    });
                    if overlaps && seen_ids.insert(sym.id.clone()) {
                        seed.push((sym.clone(), 0.9));
                    }
                }
            }
            events.push(kungfu_types::context::HistoryEvent {
                event_type: "commit".to_string(),
                target: meta.hash.clone(),
                detail: format!("{}: {}", meta.author, meta.message),
                date: Some(meta.date.clone()),
            });
        }

        let query = format!("PR #{}: {} commits", pr_num, hashes.len());
        let mut packet = kungfu_rank::build_context_packet(&query, seed, budget);
        packet.history = events;
        packet.changed_files = changed_files.into_iter().collect();
        Ok(packet)
    }

    pub fn diff_context(&self, budget: Budget) -> Result<ContextPacket> {
        let budget = self.resolve_budget(budget);
        if !kungfu_git::is_git_repo(&self.project.root) {
            bail!("not a git repository");
        }

        let changed = kungfu_git::changed_files(&self.project.root)?;
        if changed.is_empty() {
            return Ok(ContextPacket {
                query: "diff context".to_string(),
                budget,
                intent: None,
                items: Vec::new(),
                changed_files: Vec::new(),
                rationale: Vec::new(),
                history: Vec::new(),
                evidence: Vec::new(),
                project_memory: Vec::new(),
                memory_conflicts: Vec::new(),
            });
        }

        info!("building context for {} changed files", changed.len());

        let search = self.search();
        let all_symbols = search.get_all_symbols()?;

        let scored: Vec<(Symbol, f64)> = all_symbols
            .into_iter()
            .filter_map(|s| {
                let is_changed = changed
                    .iter()
                    .any(|c| s.path.ends_with(c) || c.ends_with(&s.path));
                if is_changed {
                    Some((s, 0.9))
                } else {
                    None
                }
            })
            .collect();

        Ok(build_context_packet("diff context", scored, budget))
    }
}
