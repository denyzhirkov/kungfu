//! Anti-drift guard: the static Claude Code plugin files in `plugin/` (and the
//! README's copy-paste blocks) must stay byte-identical to the templates in
//! `kungfu-project::templates` — the single source of truth used by
//! `kungfu init --agent claude`. A content change in one place fails here
//! until both are updated.

use kungfu_project::templates;
use serde_json::Value;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

fn read_json(path: &Path) -> Value {
    serde_json::from_str(&read(path))
        .unwrap_or_else(|e| panic!("invalid JSON in {}: {e}", path.display()))
}

fn workspace_version() -> String {
    let manifest = read(&repo_root().join("Cargo.toml"));
    let mut in_workspace_package = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_workspace_package = line == "[workspace.package]";
            continue;
        }
        if in_workspace_package {
            if let Some(rest) = line.strip_prefix("version") {
                let rest = rest.trim_start().strip_prefix('=').unwrap_or(rest);
                return rest.trim().trim_matches('"').to_string();
            }
        }
    }
    panic!("no version found under [workspace.package] in root Cargo.toml");
}

#[test]
fn plugin_reindex_hook_matches_template() {
    let hooks = read_json(&repo_root().join("plugin/hooks/hooks.json"));
    let post_tool_use = hooks["hooks"]["PostToolUse"]
        .as_array()
        .expect("plugin hooks.json: hooks.PostToolUse must be an array");
    assert_eq!(
        post_tool_use,
        &vec![templates::reindex_hook_entry()],
        "plugin/hooks/hooks.json PostToolUse drifted from templates::reindex_hook_entry()"
    );
}

#[test]
fn plugin_rules_injection_hook_reads_rules_file() {
    let hooks = read_json(&repo_root().join("plugin/hooks/hooks.json"));
    let session_start = hooks["hooks"]["SessionStart"]
        .as_array()
        .expect("plugin hooks.json: hooks.SessionStart must be an array");
    let command = session_start[0]["hooks"][0]["command"]
        .as_str()
        .expect("SessionStart hook must have a command");
    assert!(
        command.contains("${CLAUDE_PLUGIN_ROOT}/rules/kungfu-rules.md"),
        "SessionStart hook must inject rules/kungfu-rules.md, got: {command}"
    );
}

#[test]
fn plugin_update_check_hook_matches_template() {
    let hooks = read_json(&repo_root().join("plugin/hooks/hooks.json"));
    let session_start = hooks["hooks"]["SessionStart"]
        .as_array()
        .expect("plugin hooks.json: hooks.SessionStart must be an array");
    assert!(
        session_start.contains(&templates::update_check_hook_entry()),
        "plugin/hooks/hooks.json SessionStart is missing templates::update_check_hook_entry()"
    );
}

#[test]
fn plugin_rules_file_matches_template() {
    let rules = read(&repo_root().join("plugin/rules/kungfu-rules.md"));
    assert_eq!(
        rules.trim_end(),
        templates::CLAUDE_RULES_BODY.trim_end(),
        "plugin/rules/kungfu-rules.md drifted from templates::CLAUDE_RULES_BODY"
    );
}

#[test]
fn plugin_mcp_config_matches_template() {
    let mcp = read_json(&repo_root().join("plugin/.mcp.json"));
    assert_eq!(
        mcp["mcpServers"]["kungfu"],
        templates::mcp_server_entry(),
        "plugin/.mcp.json kungfu server entry drifted from templates::mcp_server_entry()"
    );
}

#[test]
fn plugin_manifest_version_matches_workspace() {
    let manifest = read_json(&repo_root().join("plugin/.claude-plugin/plugin.json"));
    assert_eq!(
        manifest["version"].as_str(),
        Some(workspace_version().as_str()),
        "plugin/.claude-plugin/plugin.json version drifted from [workspace.package].version"
    );
    assert_eq!(manifest["name"].as_str(), Some("kungfu"));
}

#[test]
fn marketplace_points_at_plugin_dir() {
    let marketplace = read_json(&repo_root().join(".claude-plugin/marketplace.json"));
    let plugins = marketplace["plugins"]
        .as_array()
        .expect("marketplace.json: plugins must be an array");
    let entry = plugins
        .iter()
        .find(|p| p["name"] == "kungfu")
        .expect("marketplace.json must list the kungfu plugin");
    assert_eq!(entry["source"].as_str(), Some("./plugin"));
}

#[test]
fn readme_blocks_match_templates() {
    let readme = read(&repo_root().join("README.md"));
    assert!(
        readme.contains(templates::CLAUDE_RULES_BODY.trim_end()),
        "README.md agent-rules block drifted from templates::CLAUDE_RULES_BODY"
    );
    assert!(
        readme.contains(templates::REINDEX_HOOK_COMMAND),
        "README.md auto-reindex hook command drifted from templates::REINDEX_HOOK_COMMAND"
    );
}
