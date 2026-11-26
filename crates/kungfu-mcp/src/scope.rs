/// Filter a JSON array result by scope: keep only items where "path" starts with the scope prefix.
pub(crate) fn apply_scope(json_str: &str, scope: Option<&str>) -> String {
    let scope = match scope {
        Some(s) if !s.is_empty() => s,
        _ => return json_str.to_string(),
    };

    // Try parsing as array
    if let Ok(serde_json::Value::Array(arr)) = serde_json::from_str::<serde_json::Value>(json_str) {
        let filtered: Vec<_> = arr
            .into_iter()
            .filter(|item| {
                item.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.starts_with(scope))
                    .unwrap_or(true)
            })
            .collect();
        return serde_json::to_string_pretty(&filtered).unwrap_or_else(|_| json_str.to_string());
    }

    // Try parsing as object with "items" / "results" array
    if let Ok(mut obj) = serde_json::from_str::<serde_json::Value>(json_str) {
        if let Some(items) = obj.get_mut("items").and_then(|v| v.as_array_mut()) {
            items.retain(|item| {
                item.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.starts_with(scope))
                    .unwrap_or(true)
            });
            return serde_json::to_string_pretty(&obj).unwrap_or_else(|_| json_str.to_string());
        }
        if let Some(results) = obj.get_mut("results").and_then(|v| v.as_array_mut()) {
            results.retain(|item| {
                item.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.starts_with(scope))
                    .unwrap_or(true)
            });
            return serde_json::to_string_pretty(&obj).unwrap_or_else(|_| json_str.to_string());
        }
        // Also check "key_symbols" and "related_files" in explore_file result
        if let Some(syms) = obj
            .get_mut("siblings_in_file")
            .and_then(|v| v.as_array_mut())
        {
            syms.retain(|item| {
                item.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.starts_with(scope))
                    .unwrap_or(true)
            });
        }
        if let Some(related) = obj.get_mut("related_files").and_then(|v| v.as_array_mut()) {
            related.retain(|item| {
                item.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.starts_with(scope))
                    .unwrap_or(true)
            });
        }
        if let Some(others) = obj.get_mut("other_matches").and_then(|v| v.as_array_mut()) {
            others.retain(|item| {
                item.get("path")
                    .and_then(|p| p.as_str())
                    .map(|p| p.starts_with(scope))
                    .unwrap_or(true)
            });
        }
        return serde_json::to_string_pretty(&obj).unwrap_or_else(|_| json_str.to_string());
    }

    json_str.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config-ranking note is computed in core (for find_files and
    /// find_symbol, for the scope itself); a demoted in-scope config match is
    /// typically below the cut, so the note must survive even when no config
    /// item is left on the page.
    #[test]
    fn scope_filter_keeps_the_config_ranking_note() {
        let note = serde_json::json!({ "status": "penalized", "factor": 0.6, "hint": "h" });
        let results = serde_json::json!({
            "results": [{ "path": "src/a.rs" }, { "path": "other/b.rs" }],
            "config_ranking": note,
        })
        .to_string();
        let scoped: serde_json::Value =
            serde_json::from_str(&apply_scope(&results, Some("src/"))).unwrap();
        assert_eq!(scoped["results"].as_array().unwrap().len(), 1);
        assert_eq!(scoped["config_ranking"], note);
    }
}
