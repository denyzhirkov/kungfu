use crate::cache::CACHE_CAPACITY;
use crate::params::{AffectedParam, CouplingParam, HotspotsParam};
use crate::KungfuMcp;
use kungfu_types::stats::{compression_ratio, CommandStats};

pub(crate) fn usage_stats(mcp: &KungfuMcp) -> Result<String, String> {
    let cache = mcp.cache.lock().map_err(|e| e.to_string())?;
    let total_cache = cache.hits + cache.misses;
    let hit_rate = if total_cache > 0 {
        (cache.hits as f64 / total_cache as f64) * 100.0
    } else {
        0.0
    };

    // Ratio only over results that referenced source files (they have a baseline); path-less
    // results are reported separately so they cannot drag the number down.
    let raw_bytes = cache.raw_bytes_baseline;
    let fb_served = cache.file_backed_bytes_served;
    let savings_ratio = compression_ratio(raw_bytes, fb_served);
    let estimated_tokens_saved = raw_bytes.saturating_sub(fb_served) / 4;

    let persistent = mcp
        .service()
        .and_then(|svc| svc.usage_stats().map_err(|e| e.to_string()))
        .unwrap_or_default();

    let life_raw = persistent.file_backed_raw_baseline;
    let life_fb = persistent.file_backed_bytes_served;
    let life_ratio = persistent.file_backed_ratio();
    let life_tokens_saved = persistent.file_backed_tokens_saved();

    let mut cmds: Vec<(&String, &CommandStats)> = persistent.per_command.iter().collect();
    cmds.sort_by(|a, b| b.1.bytes_served.cmp(&a.1.bytes_served).then(a.0.cmp(b.0)));
    // One line per command (pretty-printed arrays would spend a line per number).
    let per_command: Vec<String> = cmds
        .into_iter()
        .map(|(name, c)| {
            format!(
                "{name} {} {} {} {}",
                c.calls, c.bytes_served, c.raw_bytes_baseline, c.unmeasured_bytes_served
            )
        })
        .collect();

    let fmt_ratio = |r: Option<f64>| r.map_or("n/a".to_string(), |r| format!("{:.1}x", r));

    serde_json::to_string_pretty(&serde_json::json!({
        "session": {
            "calls_served": cache.calls_served,
            "bytes_served": cache.bytes_served,
            "file_backed_calls": cache.file_backed_calls,
            "file_backed_bytes_served": fb_served,
            "raw_bytes_baseline": raw_bytes,
            "pathless_calls": cache.pathless_calls,
            "pathless_bytes_served": cache.pathless_bytes_served,
            "compression_ratio": fmt_ratio(savings_ratio),
            "estimated_tokens_saved": estimated_tokens_saved,
            "baseline_method": "referenced source files' on-disk size / bytes of those results; path-less results excluded",
            "cache": {
                "entries": cache.entries.len(),
                "capacity": CACHE_CAPACITY,
                "hits": cache.hits,
                "misses": cache.misses,
                "hit_rate_pct": format!("{:.1}", hit_rate),
            }
        },
        "lifetime": {
            "total_calls": persistent.total_calls,
            "total_bytes_served": persistent.total_bytes_served,
            "file_backed_calls": persistent.file_backed_calls,
            "file_backed_bytes_served": life_fb,
            "file_backed_raw_baseline": life_raw,
            "pathless_calls": persistent.pathless_calls,
            "pathless_bytes_served": persistent.pathless_bytes_served,
            "unmeasured_calls": persistent.unmeasured_calls,
            "unmeasured_bytes_served": persistent.unmeasured_bytes_served,
            "compression_ratio": fmt_ratio(life_ratio),
            "estimated_tokens_saved": life_tokens_saved,
            "counting_since": persistent.counting_since,
            "baseline_method": "same, counted since counting_since; CLI calls are unmeasured and excluded",
            "per_command_columns": "command calls bytes_served raw_bytes_baseline unmeasured_bytes_served",
            "per_command": per_command,
            "first_used": persistent.first_used,
            "last_used": persistent.last_used,
        },
    }))
    .map_err(|e| e.to_string())
}

pub(crate) fn hotspots(mcp: &KungfuMcp, params: HotspotsParam) -> Result<String, String> {
    let top = params.top.unwrap_or(20);
    let churn = params.churn.unwrap_or(false);
    let files = params.files.unwrap_or(false);
    let service = mcp.service()?;
    let entries = service
        .hotspots(top, churn, files)
        .map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())?;
    mcp.record_served("hotspots", &out);
    Ok(out)
}

pub(crate) fn onboard(mcp: &KungfuMcp) -> Result<String, String> {
    mcp.cached("onboard", "", "", || {
        let service = mcp.service()?;
        let info = service.onboard().map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&info).map_err(|e| e.to_string())
    })
}

pub(crate) fn affected(mcp: &KungfuMcp, params: AffectedParam) -> Result<String, String> {
    let depth = params.depth.unwrap_or(3);
    let staged = params.staged.unwrap_or(false);
    let name = params.name.clone();
    let cache_key = if staged {
        format!("staged:{}", depth)
    } else {
        format!("{}:{}", name, depth)
    };
    mcp.cached("affected", &cache_key, "", || {
        let service = mcp.service()?;
        let result = if staged {
            service.affected_staged(depth).map_err(|e| e.to_string())?
        } else {
            if name.is_empty() {
                return Err("symbol name required (or set staged=true)".to_string());
            }
            service.affected(&name, depth).map_err(|e| e.to_string())?
        };
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn smart_test(mcp: &KungfuMcp) -> Result<String, String> {
    // No cache — depends on current diff state
    let service = mcp.service()?;
    let result = service.smart_test().map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("smart_test", &out);
    Ok(out)
}

pub(crate) fn test_subjects(
    mcp: &KungfuMcp,
    params: crate::params::SymbolNameParam,
) -> Result<String, String> {
    let name = params.name.clone();
    mcp.cached("test_subjects", &name, "", || {
        let service = mcp.service()?;
        let results = service.test_subjects(&name).map_err(|e| e.to_string())?;
        let items: Vec<_> = results
            .iter()
            .map(|(sym, reason)| {
                serde_json::json!({
                    "name": sym.name,
                    "kind": sym.kind.to_string(),
                    "path": sym.path,
                    "line": sym.span.start_line,
                    "signature": sym.signature,
                    "reason": reason,
                })
            })
            .collect();
        serde_json::to_string_pretty(&items).map_err(|e| e.to_string())
    })
}

pub(crate) fn review(mcp: &KungfuMcp) -> Result<String, String> {
    // No cache — depends on current diff state
    let service = mcp.service()?;
    let result = service.review().map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("review", &out);
    Ok(out)
}

pub(crate) fn verify_change(
    mcp: &KungfuMcp,
    params: crate::params::VerifyChangeParam,
) -> Result<String, String> {
    // No cache — depends on current diff state
    let depth = params.depth.unwrap_or(3);
    let service = mcp.service()?;
    let result = service.verify_change(depth).map_err(|e| e.to_string())?;
    let out = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())?;
    mcp.record_served("verify_change", &out);
    Ok(out)
}

pub(crate) fn embeddings_status(mcp: &KungfuMcp) -> Result<String, String> {
    let service = mcp.service()?;
    let status = service.embeddings_status().map_err(|e| e.to_string())?;
    let mut value = serde_json::to_value(&status).map_err(|e| e.to_string())?;
    value["job_running"] = serde_json::json!(
        crate::cache::EMBED_JOB_RUNNING.load(std::sync::atomic::Ordering::SeqCst)
    );
    serde_json::to_string_pretty(&value).map_err(|e| e.to_string())
}

/// Build runs in the background: inference (and a possible ~130MB weight download)
/// must never block the MCP request path. Poll `embeddings_status` for progress.
pub(crate) fn embeddings_build(mcp: &KungfuMcp) -> Result<String, String> {
    use std::sync::atomic::Ordering;
    if crate::cache::EMBED_JOB_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return serde_json::to_string_pretty(&serde_json::json!({
            "status": "already_running",
            "hint": "an embeddings job is in progress — poll embeddings_status",
        }))
        .map_err(|e| e.to_string());
    }
    let root = mcp.project_root.clone();
    std::thread::spawn(move || {
        let result = kungfu_core::KungfuService::open(&root)
            .map_err(|e| e.to_string())
            .and_then(|svc| svc.embeddings_build().map_err(|e| e.to_string()));
        match result {
            Ok(r) => tracing::info!(
                "embeddings build done: {} embedded, {} total, {} up-to-date",
                r.embedded,
                r.total_in_store,
                r.skipped_up_to_date
            ),
            Err(e) => tracing::warn!("embeddings build failed: {}", e),
        }
        crate::cache::EMBED_JOB_RUNNING.store(false, Ordering::SeqCst);
    });
    serde_json::to_string_pretty(&serde_json::json!({
        "status": "started",
        "hint": "building in the background (downloads weights on first run) — poll embeddings_status until indexed_vectors catches up",
    }))
    .map_err(|e| e.to_string())
}

pub(crate) fn coupling(mcp: &KungfuMcp, params: CouplingParam) -> Result<String, String> {
    let top = params.top.unwrap_or(20);
    mcp.cached("coupling", &top.to_string(), "", || {
        let service = mcp.service()?;
        let entries = service.coupling(top).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    })
}
