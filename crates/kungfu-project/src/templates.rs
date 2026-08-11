//! Single source of truth for the Claude Code integration templates written by
//! `kungfu init --agent claude`.
//!
//! Deliberately kept as plain constants plus tiny render helpers so the Claude
//! Code plugin packaging can reuse the exact same content without pulling in
//! any of the init/merge logic. If you change the rules body, bump
//! [`RULES_VERSION`] so existing blocks get replaced in place on re-run.

use serde_json::{json, Value};

/// Version of the CLAUDE.md rules block. Embedded in the start marker;
/// a version bump makes `init --agent claude` replace stale blocks in place.
pub const RULES_VERSION: &str = "v1";

/// Prefix shared by every versioned start marker — used to locate an existing
/// block regardless of which version wrote it.
pub const RULES_MARKER_START_PREFIX: &str = "<!-- kungfu:rules:start";

/// End marker of the CLAUDE.md rules block (not versioned).
pub const RULES_MARKER_END: &str = "<!-- kungfu:rules:end -->";

/// Routing rules appended to the project's CLAUDE.md. Same content as the
/// "Agent rules" block in the README — keep the two in sync.
pub const CLAUDE_RULES_BODY: &str = r#"## kungfu — context retrieval (use BEFORE Read / grep / find)

Default to kungfu; raw file reads are the fallback, not the first move — it returns
ranked, scoped packets instead of whole files. Start every task with
`ask_context("<task>", budget: "tiny")` and escalate the budget only if the packet
is clearly insufficient. Open a raw file only once kungfu points you at it.

Route by situation:

| Situation | First call |
|---|---|
| New task / "figure out X" | `ask_context` (or `investigate`) |
| Where is a named symbol defined/used? | `find_symbol` → `explore_symbol`, then `callers` / `callees` |
| Concept with no known name ("where does rate limiting live") | `semantic_search` |
| Understand a file > 50 lines | `file_outline` / `explore_file`, then a targeted Read of that range |
| About to edit a symbol | `edit_context` (full verbatim body + contracts — no follow-up Read) |
| "Why is it like this / what changed?" | `file_history` / `symbol_history` / `change_timeline` |
| Refactor touching > 1 file | `affected` + `coupling` + `smart_test` before editing |
| Bug with no clear file | `hotspots`, then `debug_trace` on the stack trace |

After edits: `reindex` the changed paths, then `verify_change` for the blast radius
and minimal test set. `memory_search` before implementing (there may already be a
decision or warning); `memory_add` to persist new ones — pin sparingly.

Skip kungfu only for: a one-line edit in a file already open this session; a file
< 50 lines whose exact path you know; pure shell ops; reading a config/lock file by
exact path. Otherwise, if you reach for Read / grep / find — stop and route above."#;

/// Current versioned start marker, e.g. `<!-- kungfu:rules:start v1 -->`.
pub fn rules_marker_start() -> String {
    format!("{RULES_MARKER_START_PREFIX} {RULES_VERSION} -->")
}

/// Full marked rules block, ready to insert into CLAUDE.md.
pub fn render_claude_rules_block() -> String {
    format!(
        "{}\n{}\n{}",
        rules_marker_start(),
        CLAUDE_RULES_BODY.trim_end(),
        RULES_MARKER_END
    )
}

/// `.mcp.json` server entry for the kungfu MCP server (Claude Code format).
pub fn mcp_server_entry() -> Value {
    json!({ "command": "kungfu", "args": ["mcp"] })
}

/// Shell command of the auto-reindex hook — same shape as documented in the
/// README ("Auto-reindex on edit").
pub const REINDEX_HOOK_COMMAND: &str =
    "jq -r '.tool_input.file_path // empty' | xargs -I{} kungfu index --only {} >/dev/null 2>&1 || true";

/// PostToolUse matcher for the auto-reindex hook.
pub const REINDEX_HOOK_MATCHER: &str = "Edit|Write|MultiEdit|NotebookEdit";

/// Substring identifying any kungfu auto-reindex hook, however the user may
/// have customized it. Used to avoid installing a duplicate.
pub const REINDEX_HOOK_FINGERPRINT: &str = "kungfu index --only";

/// Shell command of the SessionStart release check. Prints one line only when a
/// newer kungfu exists (cached 24h, silent on network failure), so a stale binary
/// is noticed at the moment restarting the session is still cheap.
pub const UPDATE_CHECK_HOOK_COMMAND: &str = "kungfu update --check --quiet 2>/dev/null || true";

/// Full SessionStart hook entry for the update check.
pub fn update_check_hook_entry() -> Value {
    json!({
        "hooks": [
            { "type": "command", "command": UPDATE_CHECK_HOOK_COMMAND }
        ]
    })
}

/// Full PostToolUse hook entry for `.claude/settings.json`.
pub fn reindex_hook_entry() -> Value {
    json!({
        "matcher": REINDEX_HOOK_MATCHER,
        "hooks": [
            { "type": "command", "command": REINDEX_HOOK_COMMAND }
        ]
    })
}
