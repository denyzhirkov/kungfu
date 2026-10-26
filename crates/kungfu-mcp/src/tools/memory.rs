use crate::params::{
    parse_budget, MemoryAddParam, MemoryIdParam, MemoryListParam, MemorySearchParam,
    MemoryUpdateParam,
};
use crate::KungfuMcp;
use kungfu_core::MemorySearchResult;

pub(crate) fn memory_add(mcp: &KungfuMcp, params: MemoryAddParam) -> Result<String, String> {
    let kind: kungfu_types::memory::ProjectMemoryKind =
        params.kind.parse().map_err(|e: String| e)?;
    let service = mcp.service()?;
    let entry = service
        .memory_add(
            kind,
            &params.content,
            params.title.as_deref(),
            params.tags.unwrap_or_default(),
            params.files.unwrap_or_default(),
            params.symbols.unwrap_or_default(),
            params.pin.unwrap_or(false),
        )
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())
}

pub(crate) fn memory_search(mcp: &KungfuMcp, params: MemorySearchParam) -> Result<String, String> {
    let filter = kungfu_memory::project_search::MemoryFilter {
        kind: params
            .kind
            .as_deref()
            .map(|k| k.parse().map_err(|e: String| e))
            .transpose()?,
        tag: params.tag,
        ..Default::default()
    };
    let service = mcp.service()?;
    let results = service
        .memory_search(
            &params.query,
            &filter,
            parse_budget(params.budget.as_deref()),
        )
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&search_output(&results)).map_err(|e| e.to_string())?;
    mcp.record_served("memory_search", &out);
    Ok(out)
}

fn search_output(r: &MemorySearchResult) -> serde_json::Value {
    // Above the threshold the hit is confident — return the full entry. Below it the match is
    // weak; returning the whole ~1KB body is noise, so emit a compact stub and let the agent
    // pull the full text via memory_get only if the title looks relevant.
    const FULL_CONTENT_THRESHOLD: f64 = 0.45;
    let items: Vec<_> = r
        .results
        .iter()
        .map(|(score, e)| {
            if *score >= FULL_CONTENT_THRESHOLD {
                serde_json::json!({
                    "score": score,
                    "entry": e,
                })
            } else {
                serde_json::json!({
                    "score": score,
                    "id": e.id,
                    "kind": e.kind,
                    "title": e.title,
                    "excerpt": excerpt(&e.content, 160),
                    "hint": "low relevance — call memory_get with this id for the full entry",
                })
            }
        })
        .collect();
    let mut out = serde_json::json!({ "results": items, "total": r.total });
    if let Some(note) = &r.note {
        out["note"] = serde_json::json!(note);
    }
    if let Some(status) = r.status {
        out["status"] = serde_json::json!(status);
    }
    if let Some(hint) = &r.hint {
        out["hint"] = serde_json::json!(hint);
    }
    out
}

/// First `max_chars` characters with an ellipsis when truncated (char-safe).
fn excerpt(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars).collect();
        format!("{head}…")
    }
}

pub(crate) fn memory_list(mcp: &KungfuMcp, params: MemoryListParam) -> Result<String, String> {
    let filter = kungfu_memory::project_search::MemoryFilter {
        kind: params
            .kind
            .as_deref()
            .map(|k| k.parse().map_err(|e: String| e))
            .transpose()?,
        tag: params.tag,
        pinned_only: params.pinned.unwrap_or(false),
        ..Default::default()
    };
    let service = mcp.service()?;
    let entries = service.memory_list(&filter).map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
    mcp.record_served("memory_list", &out);
    Ok(out)
}

pub(crate) fn memory_get(mcp: &KungfuMcp, params: MemoryIdParam) -> Result<String, String> {
    let service = mcp.service()?;
    let entry = service.memory_show(&params.id).map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())?;
    mcp.record_served("memory_get", &out);
    Ok(out)
}

pub(crate) fn memory_update(mcp: &KungfuMcp, params: MemoryUpdateParam) -> Result<String, String> {
    let service = mcp.service()?;
    let entry = service
        .memory_update(
            &params.id,
            params.content.as_deref(),
            params.title.as_deref(),
            params.tags,
            params.pin,
        )
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())
}

pub(crate) fn memory_archive(mcp: &KungfuMcp, params: MemoryIdParam) -> Result<String, String> {
    let service = mcp.service()?;
    let entry = service
        .memory_archive(&params.id)
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::budget::Budget;
    use kungfu_types::memory::{MemoryStatus, ProjectMemoryEntry, ProjectMemoryKind};

    fn hits(n: usize) -> Vec<(f64, ProjectMemoryEntry)> {
        (0..n)
            .map(|i| {
                (
                    0.9,
                    ProjectMemoryEntry {
                        id: format!("m{i}"),
                        kind: ProjectMemoryKind::Fact,
                        title: None,
                        content: "body".into(),
                        tags: vec![],
                        related_files: vec![],
                        related_symbols: vec![],
                        pinned: false,
                        status: MemoryStatus::Active,
                        supersedes: None,
                        created_at: "2026-01-01T00:00:00Z".into(),
                        updated_at: "2026-01-01T00:00:00Z".into(),
                    },
                )
            })
            .collect()
    }

    fn out(n: usize, budget: Budget) -> serde_json::Value {
        search_output(&MemorySearchResult::from_scored(hits(n), budget))
    }

    #[test]
    fn tiny_truncates_with_note_and_total() {
        let out = out(7, Budget::Tiny);
        assert_eq!(out["results"].as_array().unwrap().len(), 3);
        assert_eq!(out["total"], 7);
        assert_eq!(
            out["note"],
            "showing 3 of 7 — raise budget or narrow with kind/tag"
        );
    }

    #[test]
    fn within_limit_has_no_note() {
        let out = out(5, Budget::Auto);
        assert_eq!(out["results"].as_array().unwrap().len(), 5);
        assert!(out.get("note").is_none());
    }

    #[test]
    fn full_budget_caps_at_25_and_does_not_suggest_raising() {
        let out = out(30, Budget::Full);
        assert_eq!(out["results"].as_array().unwrap().len(), 25);
        assert_eq!(out["total"], 30);
        assert_eq!(out["note"], "showing 25 of 30 — narrow with kind/tag");
    }

    #[test]
    fn empty_is_no_match_with_hint() {
        let out = out(0, Budget::Small);
        assert_eq!(out["status"], "no_match");
        assert_eq!(out["total"], 0);
        assert!(out["hint"].as_str().unwrap().contains("memory_list"));
    }
}
