//! `kungfu init --agent claude` — writes the Claude Code integration files:
//! `.mcp.json` (MCP registration), a marked rules block in `CLAUDE.md`, and
//! the auto-reindex PostToolUse hook in `.claude/settings.json`.
//!
//! All operations are idempotent and merge-safe: existing user content is
//! never clobbered, JSON files are parsed as `serde_json::Value` so unknown
//! fields survive the roundtrip. Template content lives in [`crate::templates`].

use crate::templates;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum AgentInitError {
    #[error("failed to read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{path} is not valid JSON: {source}. Fix the syntax (or remove the file) and re-run `kungfu init --agent claude`")]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("{path}: expected {what} to be {expected}. Fix it manually and re-run `kungfu init --agent claude`")]
    UnexpectedShape {
        path: PathBuf,
        what: String,
        expected: &'static str,
    },
    #[error("{path}: found `{prefix}` without a matching `{end}` marker. Remove or complete the kungfu block and re-run `kungfu init --agent claude`", prefix = templates::RULES_MARKER_START_PREFIX, end = templates::RULES_MARKER_END)]
    UnterminatedRulesBlock { path: PathBuf },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionStatus {
    Created,
    Updated,
    AlreadyCurrent,
}

impl ActionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ActionStatus::Created => "created",
            ActionStatus::Updated => "updated",
            ActionStatus::AlreadyCurrent => "already current",
        }
    }
}

/// One file touched (or verified) by the integration setup.
#[derive(Debug)]
pub struct AgentInitAction {
    /// Path relative to the project root, for display.
    pub path: String,
    pub status: ActionStatus,
    pub detail: String,
}

/// Set up (or refresh) the Claude Code integration under `root`.
///
/// With `dry_run` nothing is written; the returned actions describe what a
/// real run would do.
pub fn init_claude_integration(
    root: &Path,
    dry_run: bool,
) -> Result<Vec<AgentInitAction>, AgentInitError> {
    Ok(vec![
        sync_mcp_json(root, dry_run)?,
        sync_claude_md(root, dry_run)?,
        sync_settings_json(root, dry_run)?,
    ])
}

fn sync_mcp_json(root: &Path, dry_run: bool) -> Result<AgentInitAction, AgentInitError> {
    let path = root.join(".mcp.json");
    let existing = read_optional(&path)?;
    let desired = templates::mcp_server_entry();

    let mut doc: Value = match &existing {
        None => Value::Object(Map::new()),
        Some(text) => serde_json::from_str(text).map_err(|source| AgentInitError::InvalidJson {
            path: path.clone(),
            source,
        })?,
    };

    let servers = object_entry(&mut doc, "mcpServers", &path, "top level")?;
    if servers.get("kungfu") == Some(&desired) {
        return Ok(AgentInitAction {
            path: ".mcp.json".to_string(),
            status: ActionStatus::AlreadyCurrent,
            detail: "kungfu MCP server already registered".to_string(),
        });
    }

    let replacing = servers.contains_key("kungfu");
    servers.insert("kungfu".to_string(), desired);
    let status = if existing.is_some() {
        ActionStatus::Updated
    } else {
        ActionStatus::Created
    };
    let detail = if replacing {
        "kungfu MCP server entry rewritten".to_string()
    } else if existing.is_some() {
        "kungfu MCP server added (existing servers preserved)".to_string()
    } else {
        "kungfu MCP server registered".to_string()
    };

    if !dry_run {
        write_json(&path, &doc)?;
    }
    Ok(AgentInitAction {
        path: ".mcp.json".to_string(),
        status,
        detail,
    })
}

fn sync_claude_md(root: &Path, dry_run: bool) -> Result<AgentInitAction, AgentInitError> {
    let path = root.join("CLAUDE.md");
    let existing = read_optional(&path)?;
    let block = templates::render_claude_rules_block();

    let (new_content, status, detail) = match existing {
        None => (
            format!("{block}\n"),
            ActionStatus::Created,
            format!(
                "created with kungfu rules block {}",
                templates::RULES_VERSION
            ),
        ),
        Some(content) => {
            if let Some(start) = content.find(templates::RULES_MARKER_START_PREFIX) {
                let end_rel = content[start..]
                    .find(templates::RULES_MARKER_END)
                    .ok_or_else(|| AgentInitError::UnterminatedRulesBlock { path: path.clone() })?;
                let end = start + end_rel + templates::RULES_MARKER_END.len();
                if content[start..end] == block {
                    return Ok(AgentInitAction {
                        path: "CLAUDE.md".to_string(),
                        status: ActionStatus::AlreadyCurrent,
                        detail: format!(
                            "kungfu rules block {} already present",
                            templates::RULES_VERSION
                        ),
                    });
                }
                let mut updated = String::with_capacity(content.len() + block.len());
                updated.push_str(&content[..start]);
                updated.push_str(&block);
                updated.push_str(&content[end..]);
                (
                    updated,
                    ActionStatus::Updated,
                    format!(
                        "kungfu rules block replaced in place (now {})",
                        templates::RULES_VERSION
                    ),
                )
            } else {
                let mut updated = content;
                if !updated.is_empty() && !updated.ends_with('\n') {
                    updated.push('\n');
                }
                if !updated.is_empty() {
                    updated.push('\n');
                }
                updated.push_str(&block);
                updated.push('\n');
                (
                    updated,
                    ActionStatus::Updated,
                    format!(
                        "kungfu rules block {} appended (existing content untouched)",
                        templates::RULES_VERSION
                    ),
                )
            }
        }
    };

    if !dry_run {
        write_atomic(&path, &new_content)?;
    }
    Ok(AgentInitAction {
        path: "CLAUDE.md".to_string(),
        status,
        detail,
    })
}

fn sync_settings_json(root: &Path, dry_run: bool) -> Result<AgentInitAction, AgentInitError> {
    let path = root.join(".claude").join("settings.json");
    let display = ".claude/settings.json".to_string();
    let existing = read_optional(&path)?;

    let mut doc: Value = match &existing {
        None => Value::Object(Map::new()),
        Some(text) => serde_json::from_str(text).map_err(|source| AgentInitError::InvalidJson {
            path: path.clone(),
            source,
        })?,
    };

    let hooks = object_entry(&mut doc, "hooks", &path, "top level")?;
    let post = match hooks
        .entry("PostToolUse".to_string())
        .or_insert_with(|| Value::Array(Vec::new()))
    {
        Value::Array(arr) => arr,
        _ => {
            return Err(AgentInitError::UnexpectedShape {
                path,
                what: "\"hooks.PostToolUse\"".to_string(),
                expected: "a JSON array",
            })
        }
    };

    let found = scan_reindex_hooks(post);
    let has_reindex_hook = found.current || found.custom;

    let detail = if found.legacy_template {
        // A custom hook may not fire on Edit/Write, so only a current hook
        // makes the template redundant here.
        *post = replace_legacy_reindex_hooks(post, !found.current);
        "outdated auto-reindex hook (jq + `kungfu index --only`) replaced with `kungfu index --from-hook`"
            .to_string()
    } else if has_reindex_hook {
        let detail = if found.current {
            "auto-reindex hook already present"
        } else {
            "custom kungfu reindex hook present — left as is"
        };
        return Ok(AgentInitAction {
            path: display,
            status: ActionStatus::AlreadyCurrent,
            detail: detail.to_string(),
        });
    } else {
        post.push(templates::reindex_hook_entry());
        if existing.is_some() {
            "auto-reindex hook appended (existing hooks preserved)".to_string()
        } else {
            "auto-reindex hook installed (PostToolUse on Edit/Write)".to_string()
        }
    };
    let status = if existing.is_some() {
        ActionStatus::Updated
    } else {
        ActionStatus::Created
    };

    if !dry_run {
        write_json(&path, &doc)?;
    }
    Ok(AgentInitAction {
        path: display,
        status,
        detail,
    })
}

/// Which kinds of kungfu reindex hook a `PostToolUse` array contains.
#[derive(Debug, Default)]
pub(crate) struct ReindexHooks {
    /// A hook running `kungfu index --from-hook` (the template or a variant).
    pub current: bool,
    /// The exact pre-`--from-hook` template command — safe to replace.
    pub legacy_template: bool,
    /// Any other user-written `kungfu index --only` hook — never touched.
    pub custom: bool,
}

pub(crate) fn scan_reindex_hooks(post: &[Value]) -> ReindexHooks {
    let mut found = ReindexHooks::default();
    let commands = post
        .iter()
        .filter_map(|entry| entry.get("hooks").and_then(Value::as_array))
        .flatten()
        .filter_map(hook_command);
    for cmd in commands {
        if cmd.contains(templates::REINDEX_HOOK_FINGERPRINT) {
            found.current = true;
        } else if is_legacy_template(cmd) {
            found.legacy_template = true;
        } else if cmd.contains(templates::LEGACY_REINDEX_HOOK_FINGERPRINT) {
            found.custom = true;
        }
    }
    found
}

fn hook_command(hook: &Value) -> Option<&str> {
    hook.get("command").and_then(Value::as_str)
}

fn is_legacy_template(cmd: &str) -> bool {
    cmd.trim() == templates::LEGACY_REINDEX_HOOK_COMMAND
}

/// Drop every legacy template hook; an entry left with no hooks is dropped
/// too, so user hooks sharing an entry survive. With `insert_current`, the
/// template entry takes the position of the first legacy entry.
fn replace_legacy_reindex_hooks(post: &[Value], insert_current: bool) -> Vec<Value> {
    let is_legacy = |h: &Value| hook_command(h).is_some_and(is_legacy_template);
    let mut out = Vec::with_capacity(post.len());
    let mut insert_at = None;
    for entry in post {
        let hooks = entry.get("hooks").and_then(Value::as_array);
        if !hooks.is_some_and(|hooks| hooks.iter().any(is_legacy)) {
            out.push(entry.clone());
            continue;
        }
        insert_at.get_or_insert(out.len());
        let mut entry = entry.clone();
        if let Some(hooks) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
            hooks.retain(|h| !is_legacy(h));
            if !hooks.is_empty() {
                out.push(entry);
            }
        }
    }
    if insert_current {
        out.insert(
            insert_at.unwrap_or(out.len()),
            templates::reindex_hook_entry(),
        );
    }
    out
}

/// Get `doc[key]` as a mutable object, inserting an empty object if the key is
/// missing. Errors if `doc` itself or an existing `doc[key]` is not an object.
fn object_entry<'a>(
    doc: &'a mut Value,
    key: &str,
    path: &Path,
    parent: &str,
) -> Result<&'a mut Map<String, Value>, AgentInitError> {
    let obj = doc
        .as_object_mut()
        .ok_or_else(|| AgentInitError::UnexpectedShape {
            path: path.to_path_buf(),
            what: parent.to_string(),
            expected: "a JSON object",
        })?;
    match obj
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Map::new()))
    {
        Value::Object(map) => Ok(map),
        _ => Err(AgentInitError::UnexpectedShape {
            path: path.to_path_buf(),
            what: format!("\"{key}\""),
            expected: "a JSON object",
        }),
    }
}

fn read_optional(path: &Path) -> Result<Option<String>, AgentInitError> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AgentInitError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn write_json(path: &Path, doc: &Value) -> Result<(), AgentInitError> {
    let text = serde_json::to_string_pretty(doc).map_err(|source| AgentInitError::InvalidJson {
        path: path.to_path_buf(),
        source,
    })?;
    write_atomic(path, &format!("{text}\n"))
}

fn write_atomic(path: &Path, contents: &str) -> Result<(), AgentInitError> {
    let write_err = |source| AgentInitError::Write {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(write_err)?;
    }
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("kungfu");
    let tmp = path.with_file_name(format!("{file_name}.kungfu-tmp"));
    std::fs::write(&tmp, contents).map_err(write_err)?;
    std::fs::rename(&tmp, path).map_err(write_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("kungfu-agent-init-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read(root: &Path, rel: &str) -> String {
        std::fs::read_to_string(root.join(rel)).unwrap()
    }

    #[test]
    fn fresh_project_creates_all_three_files() {
        let root = temp_root("fresh");
        let actions = init_claude_integration(&root, false).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(actions.iter().all(|a| a.status == ActionStatus::Created));

        let mcp: Value = serde_json::from_str(&read(&root, ".mcp.json")).unwrap();
        assert_eq!(mcp["mcpServers"]["kungfu"]["command"], "kungfu");
        assert_eq!(mcp["mcpServers"]["kungfu"]["args"][0], "mcp");

        let md = read(&root, "CLAUDE.md");
        assert!(md.starts_with(&templates::rules_marker_start()));
        assert!(md.contains(templates::RULES_MARKER_END));

        let settings: Value = serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
        let post = settings["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert_eq!(post[0]["matcher"], templates::REINDEX_HOOK_MATCHER);
    }

    #[test]
    fn existing_claude_md_content_is_preserved() {
        let root = temp_root("md-append");
        let user_content = "# My project\n\nDo not touch this.\n";
        std::fs::write(root.join("CLAUDE.md"), user_content).unwrap();

        let actions = init_claude_integration(&root, false).unwrap();
        let md_action = actions.iter().find(|a| a.path == "CLAUDE.md").unwrap();
        assert_eq!(md_action.status, ActionStatus::Updated);

        let md = read(&root, "CLAUDE.md");
        assert!(md.starts_with(user_content));
        assert!(md.contains(&templates::rules_marker_start()));
        assert!(md.trim_end().ends_with(templates::RULES_MARKER_END));
    }

    #[test]
    fn rerun_is_idempotent() {
        let root = temp_root("idem");
        init_claude_integration(&root, false).unwrap();
        let first = (
            read(&root, ".mcp.json"),
            read(&root, "CLAUDE.md"),
            read(&root, ".claude/settings.json"),
        );

        let actions = init_claude_integration(&root, false).unwrap();
        assert!(
            actions
                .iter()
                .all(|a| a.status == ActionStatus::AlreadyCurrent),
            "second run must be a no-op: {actions:?}"
        );
        let second = (
            read(&root, ".mcp.json"),
            read(&root, "CLAUDE.md"),
            read(&root, ".claude/settings.json"),
        );
        assert_eq!(first, second);
    }

    #[test]
    fn old_rules_block_version_is_replaced_in_place() {
        let root = temp_root("upgrade");
        let old_block = format!(
            "{} v1 -->\nold stale rules\n{}",
            templates::RULES_MARKER_START_PREFIX,
            templates::RULES_MARKER_END
        );
        let content = format!("# Before\n\n{old_block}\n\n# After\n");
        std::fs::write(root.join("CLAUDE.md"), &content).unwrap();

        let actions = init_claude_integration(&root, false).unwrap();
        let md_action = actions.iter().find(|a| a.path == "CLAUDE.md").unwrap();
        assert_eq!(md_action.status, ActionStatus::Updated);

        let md = read(&root, "CLAUDE.md");
        assert!(md.starts_with("# Before\n"));
        assert!(md.ends_with("\n\n# After\n"));
        assert!(md.contains(&templates::rules_marker_start()));
        assert!(md.contains(templates::CLAUDE_RULES_BODY));
        assert!(!md.contains("old stale rules"));
        assert!(!md.contains(&format!("{} v1 -->", templates::RULES_MARKER_START_PREFIX)));
        assert_eq!(md.matches(templates::RULES_MARKER_END).count(), 1);
    }

    #[test]
    fn unterminated_rules_block_is_an_error() {
        let root = temp_root("unterminated");
        std::fs::write(
            root.join("CLAUDE.md"),
            format!(
                "{} v1 -->\nno end marker\n",
                templates::RULES_MARKER_START_PREFIX
            ),
        )
        .unwrap();
        let err = init_claude_integration(&root, false).unwrap_err();
        assert!(matches!(err, AgentInitError::UnterminatedRulesBlock { .. }));
        assert!(err.to_string().contains("CLAUDE.md"));
    }

    #[test]
    fn existing_settings_hooks_are_preserved() {
        let root = temp_root("settings-merge");
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let existing = serde_json::json!({
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": {
                "PreToolUse": [ { "matcher": "Bash", "hooks": [ { "type": "command", "command": "echo pre" } ] } ],
                "PostToolUse": [ { "matcher": "Write", "hooks": [ { "type": "command", "command": "echo custom" } ] } ]
            }
        });
        std::fs::write(
            root.join(".claude/settings.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let actions = init_claude_integration(&root, false).unwrap();
        let action = actions
            .iter()
            .find(|a| a.path == ".claude/settings.json")
            .unwrap();
        assert_eq!(action.status, ActionStatus::Updated);

        let settings: Value = serde_json::from_str(&read(&root, ".claude/settings.json")).unwrap();
        assert_eq!(settings["permissions"]["allow"][0], "Bash(ls:*)");
        assert_eq!(
            settings["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
            "echo pre"
        );
        let post = settings["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 2);
        assert_eq!(post[0]["hooks"][0]["command"], "echo custom");
        assert!(post[1]["hooks"][0]["command"]
            .as_str()
            .unwrap()
            .contains(templates::REINDEX_HOOK_FINGERPRINT));

        // Equivalent hook already present → no duplicate on re-run.
        let actions = init_claude_integration(&root, false).unwrap();
        let action = actions
            .iter()
            .find(|a| a.path == ".claude/settings.json")
            .unwrap();
        assert_eq!(action.status, ActionStatus::AlreadyCurrent);
    }

    fn legacy_hook_entry() -> Value {
        serde_json::json!({
            "matcher": "Edit|Write|MultiEdit|NotebookEdit",
            "hooks": [ { "type": "command", "command": templates::LEGACY_REINDEX_HOOK_COMMAND } ]
        })
    }

    fn write_post_tool_use(root: &Path, post: Value) {
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let doc = serde_json::json!({ "hooks": { "PostToolUse": post } });
        std::fs::write(
            root.join(".claude/settings.json"),
            serde_json::to_string_pretty(&doc).unwrap(),
        )
        .unwrap();
    }

    fn post_tool_use(root: &Path) -> Vec<Value> {
        let settings: Value = serde_json::from_str(&read(root, ".claude/settings.json")).unwrap();
        settings["hooks"]["PostToolUse"].as_array().unwrap().clone()
    }

    fn kungfu_entries(post: &[Value]) -> usize {
        post.iter()
            .filter(|e| {
                let found = scan_reindex_hooks(std::slice::from_ref(e));
                found.current || found.legacy_template || found.custom
            })
            .count()
    }

    #[test]
    fn legacy_reindex_hook_is_replaced_in_place() {
        let root = temp_root("legacy-hook");
        let custom = |cmd: &str| serde_json::json!({ "matcher": "Write", "hooks": [ { "type": "command", "command": cmd } ] });
        write_post_tool_use(
            &root,
            serde_json::json!([custom("echo a"), legacy_hook_entry(), custom("echo b")]),
        );

        let actions = init_claude_integration(&root, false).unwrap();
        let action = actions
            .iter()
            .find(|a| a.path == ".claude/settings.json")
            .unwrap();
        assert_eq!(action.status, ActionStatus::Updated);
        assert!(action.detail.contains("replaced"));

        let post = post_tool_use(&root);
        assert_eq!(
            post,
            vec![
                custom("echo a"),
                templates::reindex_hook_entry(),
                custom("echo b")
            ]
        );
        assert_eq!(kungfu_entries(&post), 1);

        let actions = init_claude_integration(&root, false).unwrap();
        let action = actions
            .iter()
            .find(|a| a.path == ".claude/settings.json")
            .unwrap();
        assert_eq!(action.status, ActionStatus::AlreadyCurrent);
    }

    #[test]
    fn legacy_hook_sharing_an_entry_keeps_user_hooks() {
        let root = temp_root("legacy-shared");
        write_post_tool_use(
            &root,
            serde_json::json!([{
                "matcher": "Edit|Write",
                "hooks": [
                    { "type": "command", "command": "echo mine" },
                    { "type": "command", "command": templates::LEGACY_REINDEX_HOOK_COMMAND }
                ]
            }]),
        );

        init_claude_integration(&root, false).unwrap();
        let post = post_tool_use(&root);
        assert_eq!(post.len(), 2);
        assert_eq!(post[0], templates::reindex_hook_entry());
        assert_eq!(post[1]["hooks"].as_array().unwrap().len(), 1);
        assert_eq!(post[1]["hooks"][0]["command"], "echo mine");
        assert_eq!(kungfu_entries(&post), 1);
    }

    #[test]
    fn legacy_hook_next_to_current_one_is_removed() {
        let root = temp_root("legacy-and-current");
        write_post_tool_use(
            &root,
            serde_json::json!([templates::reindex_hook_entry(), legacy_hook_entry()]),
        );

        init_claude_integration(&root, false).unwrap();
        assert_eq!(post_tool_use(&root), vec![templates::reindex_hook_entry()]);
    }

    #[test]
    fn custom_only_hooks_are_left_alone() {
        let customs = [
            serde_json::json!({
                "matcher": "Bash",
                "hooks": [ { "type": "command", "command": "git diff --name-only | xargs kungfu index --only" } ]
            }),
            serde_json::json!({
                "matcher": "Edit|Write",
                "hooks": [ { "type": "command", "command": "jq -r '.tool_input.file_path // empty' | xargs -I{} kungfu index --only {} && cargo fmt" } ]
            }),
        ];
        for (i, custom) in customs.into_iter().enumerate() {
            let root = temp_root(&format!("custom-only-{i}"));
            write_post_tool_use(&root, serde_json::json!([custom.clone()]));

            let actions = init_claude_integration(&root, false).unwrap();
            let action = actions
                .iter()
                .find(|a| a.path == ".claude/settings.json")
                .unwrap();
            assert_eq!(action.status, ActionStatus::AlreadyCurrent);
            assert!(action.detail.contains("custom"));
            assert_eq!(post_tool_use(&root), vec![custom]);
        }
    }

    #[test]
    fn legacy_template_next_to_custom_hook_is_replaced_and_custom_kept() {
        let root = temp_root("legacy-and-custom");
        let custom = serde_json::json!({
            "matcher": "Bash",
            "hooks": [ { "type": "command", "command": "git diff --name-only | xargs kungfu index --only" } ]
        });
        write_post_tool_use(
            &root,
            serde_json::json!([legacy_hook_entry(), custom.clone()]),
        );

        init_claude_integration(&root, false).unwrap();
        let post = post_tool_use(&root);
        assert_eq!(post, vec![templates::reindex_hook_entry(), custom]);
        let from_hook = post
            .iter()
            .filter(|e| scan_reindex_hooks(std::slice::from_ref(e)).current)
            .count();
        assert_eq!(from_hook, 1);
    }

    #[test]
    fn existing_mcp_json_servers_and_unknown_fields_are_preserved() {
        let root = temp_root("mcp-merge");
        let existing = serde_json::json!({
            "mcpServers": {
                "other": { "command": "other-server", "args": ["--flag"], "env": { "X": "1" } }
            },
            "customTopLevelField": { "keep": true }
        });
        std::fs::write(
            root.join(".mcp.json"),
            serde_json::to_string_pretty(&existing).unwrap(),
        )
        .unwrap();

        let actions = init_claude_integration(&root, false).unwrap();
        let action = actions.iter().find(|a| a.path == ".mcp.json").unwrap();
        assert_eq!(action.status, ActionStatus::Updated);

        let mcp: Value = serde_json::from_str(&read(&root, ".mcp.json")).unwrap();
        assert_eq!(mcp["mcpServers"]["other"]["command"], "other-server");
        assert_eq!(mcp["mcpServers"]["other"]["env"]["X"], "1");
        assert_eq!(mcp["customTopLevelField"]["keep"], true);
        assert_eq!(mcp["mcpServers"]["kungfu"]["command"], "kungfu");
    }

    #[test]
    fn dry_run_touches_nothing() {
        let root = temp_root("dry");
        let user_md = "# Untouched\n";
        std::fs::write(root.join("CLAUDE.md"), user_md).unwrap();

        let actions = init_claude_integration(&root, true).unwrap();
        assert_eq!(actions.len(), 3);
        assert!(!root.join(".mcp.json").exists());
        assert!(!root.join(".claude").exists());
        assert_eq!(read(&root, "CLAUDE.md"), user_md);
        // The plan still reports what a real run would do.
        assert!(actions
            .iter()
            .any(|a| a.path == "CLAUDE.md" && a.status == ActionStatus::Updated));
    }

    #[test]
    fn invalid_json_error_names_the_file() {
        let root = temp_root("bad-json");
        std::fs::write(root.join(".mcp.json"), "{ not json").unwrap();
        let err = init_claude_integration(&root, false).unwrap_err();
        assert!(matches!(err, AgentInitError::InvalidJson { .. }));
        assert!(err.to_string().contains(".mcp.json"));
    }
}
