use crate::params::{
    MemoryAddParam, MemoryIdParam, MemoryListParam, MemorySearchParam, MemoryUpdateParam,
};
use crate::KungfuMcp;

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
        .memory_search(&params.query, &filter)
        .map_err(|e| e.to_string())?;
    // Above the threshold the hit is confident — return the full entry. Below it the match is
    // weak; returning the whole ~1KB body is noise, so emit a compact stub and let the agent
    // pull the full text via memory_get only if the title looks relevant.
    const FULL_CONTENT_THRESHOLD: f64 = 0.45;
    let items: Vec<_> = results
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
    let out = serde_json::to_string_pretty(&items).map_err(|e| e.to_string())?;
    mcp.record_served("memory_search", &out);
    Ok(out)
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
