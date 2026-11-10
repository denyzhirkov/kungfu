use crate::params::{
    parse_budget, MemoryAddParam, MemoryIdParam, MemorySearchParam, MemoryUpdateParam,
};
use crate::KungfuMcp;
use kungfu_core::MemorySearchResult;
use kungfu_types::memory::MemoryStatus;

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
        pinned_only: params.pinned.unwrap_or(false),
        status: params
            .status
            .as_deref()
            .map(|s| s.parse().map_err(|e: String| e))
            .transpose()?,
    };
    let query = params.query.as_deref().unwrap_or("");
    let service = mcp.service()?;
    let results = service
        .memory_search(query, &filter, parse_budget(params.budget.as_deref()))
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&search_output(&results, query.trim().is_empty()))
        .map_err(|e| e.to_string())?;
    mcp.record_served("memory_search", &out);
    Ok(out)
}

fn search_output(r: &MemorySearchResult, listing: bool) -> serde_json::Value {
    // Above the threshold the hit is confident — return the full entry. Below it the match is
    // weak; returning the whole ~1KB body is noise, so emit a compact stub and let the agent
    // pull the full text via memory_get only if the title looks relevant.
    const FULL_CONTENT_THRESHOLD: f64 = 0.45;
    let items: Vec<_> = r
        .results
        .iter()
        .map(|(score, e)| {
            if listing {
                // A listing is an overview: compact stubs, full text via memory_get.
                serde_json::json!({
                    "id": e.id,
                    "kind": e.kind,
                    "title": e.title,
                    "pinned": e.pinned,
                    "updated_at": e.updated_at,
                    "excerpt": excerpt(&e.content, 160),
                    "hint": "call memory_get with this id for the full entry",
                })
            } else if *score >= FULL_CONTENT_THRESHOLD {
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

pub(crate) fn memory_get(mcp: &KungfuMcp, params: MemoryIdParam) -> Result<String, String> {
    let service = mcp.service()?;
    let entry = service.memory_show(&params.id).map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())?;
    mcp.record_served("memory_get", &out);
    Ok(out)
}

pub(crate) fn memory_update(mcp: &KungfuMcp, params: MemoryUpdateParam) -> Result<String, String> {
    let status = params
        .status
        .as_deref()
        .map(|s| match s.parse::<MemoryStatus>() {
            Ok(st @ (MemoryStatus::Active | MemoryStatus::Archived)) => Ok(st),
            _ => Err(format!(
                "invalid status '{s}' — expected \"active\" or \"archived\""
            )),
        })
        .transpose()?;
    let service = mcp.service()?;
    let entry = service
        .memory_update(
            &params.id,
            params.content.as_deref(),
            params.title.as_deref(),
            params.tags,
            params.pin,
            status,
        )
        .map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&entry).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::budget::Budget;
    use kungfu_types::memory::{ProjectMemoryEntry, ProjectMemoryKind};
    use std::path::PathBuf;

    fn temp_mcp(tag: &str) -> (KungfuMcp, PathBuf) {
        let tmp = std::env::temp_dir().join(format!("kungfu_mcp_mem_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::create_dir_all(&tmp).unwrap();
        kungfu_project::init_project(&tmp).unwrap();
        (KungfuMcp::new(tmp.clone()), tmp)
    }

    fn add(mcp: &KungfuMcp, content: &str) -> String {
        let out = memory_add(
            mcp,
            MemoryAddParam {
                content: content.into(),
                kind: "fact".into(),
                title: None,
                tags: None,
                files: None,
                symbols: None,
                pin: None,
            },
        )
        .unwrap();
        serde_json::from_str::<serde_json::Value>(&out).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn search(mcp: &KungfuMcp, query: Option<&str>) -> serde_json::Value {
        search_with(mcp, query, None, "tiny")
    }

    fn search_with(
        mcp: &KungfuMcp,
        query: Option<&str>,
        status: Option<&str>,
        budget: &str,
    ) -> serde_json::Value {
        let out = memory_search(
            mcp,
            MemorySearchParam {
                query: query.map(String::from),
                kind: None,
                tag: None,
                pinned: None,
                status: status.map(String::from),
                budget: Some(budget.into()),
            },
        )
        .unwrap();
        serde_json::from_str(&out).unwrap()
    }

    fn update_status(mcp: &KungfuMcp, id: &str, status: &str) -> Result<String, String> {
        memory_update(
            mcp,
            MemoryUpdateParam {
                id: id.into(),
                content: None,
                title: None,
                tags: None,
                pin: None,
                status: Some(status.into()),
            },
        )
    }

    #[test]
    fn empty_query_lists_capped_with_note() {
        let (mcp, tmp) = temp_mcp("list");
        for i in 0..5 {
            add(&mcp, &format!("entry number {i}"));
        }
        for q in [None, Some("")] {
            let out = search(&mcp, q);
            assert_eq!(out["results"].as_array().unwrap().len(), 3);
            assert_eq!(out["total"], 5);
            assert!(out["note"].as_str().unwrap().starts_with("showing 3 of 5"));
        }
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn archived_status_hides_entry_and_active_restores() {
        let (mcp, tmp) = temp_mcp("archive");
        let id = add(&mcp, "needle content");
        assert_eq!(search(&mcp, Some("needle"))["total"], 1);
        update_status(&mcp, &id, "archived").unwrap();
        assert_eq!(search(&mcp, Some("needle"))["status"], "no_match");
        assert_eq!(search(&mcp, None)["status"], "no_match");
        let archived = search_with(&mcp, None, Some("archived"), "tiny");
        assert_eq!(archived["results"][0]["id"], id.as_str());
        update_status(&mcp, &id, "active").unwrap();
        assert_eq!(search(&mcp, Some("needle"))["total"], 1);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn listing_is_pinned_first_then_newest_as_compact_stubs() {
        let (mcp, tmp) = temp_mcp("order");
        let ids: Vec<String> = (0..4).map(|i| add(&mcp, &format!("entry {i}"))).collect();
        // Updating bumps updated_at, so the oldest id becomes the newest entry.
        memory_update(
            &mcp,
            MemoryUpdateParam {
                id: ids[0].clone(),
                content: Some("entry 0 edited".into()),
                title: None,
                tags: None,
                pin: None,
                status: None,
            },
        )
        .unwrap();
        memory_update(
            &mcp,
            MemoryUpdateParam {
                id: ids[1].clone(),
                content: None,
                title: None,
                tags: None,
                pin: Some(true),
                status: None,
            },
        )
        .unwrap();
        let out = search_with(&mcp, None, None, "full");
        let order: Vec<&str> = out["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["id"].as_str().unwrap())
            .collect();
        assert_eq!(order, [&ids[1], &ids[0], &ids[3], &ids[2]]);
        assert!(out["results"][0].get("entry").is_none());
        assert_eq!(out["results"][0]["pinned"], true);
        std::fs::remove_dir_all(tmp).ok();
    }

    #[test]
    fn invalid_status_is_an_error() {
        let (mcp, tmp) = temp_mcp("badstatus");
        let id = add(&mcp, "content");
        let err = update_status(&mcp, &id, "deleted").unwrap_err();
        assert!(err.contains("invalid status 'deleted'"), "{err}");
        std::fs::remove_dir_all(tmp).ok();
    }

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
        search_output(&MemorySearchResult::from_scored(hits(n), budget), false)
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
        assert!(out["hint"].as_str().unwrap().contains("memory_add"));
    }
}
