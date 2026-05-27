use crate::cache::CACHE_CAPACITY;
use crate::params::{AffectedParam, CouplingParam, HotspotsParam};
use crate::KungfuMcp;

pub(crate) fn usage_stats(mcp: &KungfuMcp) -> Result<String, String> {
    let cache = mcp.cache.lock().map_err(|e| e.to_string())?;
    let total_cache = cache.hits + cache.misses;
    let hit_rate = if total_cache > 0 {
        (cache.hits as f64 / total_cache as f64) * 100.0
    } else {
        0.0
    };

    // Estimate raw size: each call would read ~8KB (avg file) without kungfu
    // Token estimate: ~4 chars per token
    let estimated_raw_bytes = cache.calls_served * 8192;
    let kungfu_bytes = cache.bytes_served;
    let savings_ratio = if kungfu_bytes > 0 {
        estimated_raw_bytes as f64 / kungfu_bytes as f64
    } else {
        0.0
    };
    let estimated_tokens_saved = estimated_raw_bytes.saturating_sub(kungfu_bytes) / 4;

    let persistent = mcp
        .service()
        .and_then(|svc| svc.usage_stats().map_err(|e| e.to_string()))
        .unwrap_or_default();

    serde_json::to_string_pretty(&serde_json::json!({
        "session": {
            "calls_served": cache.calls_served,
            "bytes_served": kungfu_bytes,
            "estimated_raw_bytes": estimated_raw_bytes,
            "compression_ratio": format!("{:.1}x", savings_ratio),
            "estimated_tokens_saved": estimated_tokens_saved,
            "cache": {
                "entries": cache.entries.len(),
                "capacity": CACHE_CAPACITY,
                "hits": cache.hits,
                "misses": cache.misses,
                "hit_rate_pct": format!("{:.1}", hit_rate),
            }
        },
        "lifetime": persistent,
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
    serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
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
    let name = params.name.clone();
    mcp.cached("affected", &name, &depth.to_string(), || {
        let service = mcp.service()?;
        let result = service.affected(&name, depth).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
    })
}

pub(crate) fn smart_test(mcp: &KungfuMcp) -> Result<String, String> {
    // No cache — depends on current diff state
    let service = mcp.service()?;
    let result = service.smart_test().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

pub(crate) fn review(mcp: &KungfuMcp) -> Result<String, String> {
    // No cache — depends on current diff state
    let service = mcp.service()?;
    let result = service.review().map_err(|e| e.to_string())?;
    serde_json::to_string_pretty(&result).map_err(|e| e.to_string())
}

pub(crate) fn coupling(mcp: &KungfuMcp, params: CouplingParam) -> Result<String, String> {
    let top = params.top.unwrap_or(20);
    mcp.cached("coupling", &top.to_string(), "", || {
        let service = mcp.service()?;
        let entries = service.coupling(top).map_err(|e| e.to_string())?;
        serde_json::to_string_pretty(&entries).map_err(|e| e.to_string())
    })
}
