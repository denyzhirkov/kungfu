use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    Implementation, ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo,
};
use rmcp::{tool, tool_handler, tool_router, ServerHandler, ServiceExt};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{debug, info, warn};

mod cache;
mod params;
mod scope;
mod tools;

use crate::cache::CacheState;
use crate::params::*;

#[derive(Clone)]
pub struct KungfuMcp {
    pub(crate) project_root: PathBuf,
    tool_router: ToolRouter<Self>,
    pub(crate) cache: Arc<Mutex<CacheState>>,
}

impl KungfuMcp {
    pub fn new(project_root: PathBuf) -> Self {
        Self {
            project_root,
            tool_router: Self::tool_router(),
            cache: Arc::new(Mutex::new(CacheState::new())),
        }
    }
}

#[tool_router]
impl KungfuMcp {
    #[tool(
        description = "Show project status: file count, symbol count, languages, git status, update check, and an `embeddings` block (ready, feature, model, vectors, job_running, one-line `hint` with the next step). Cheap — safe to poll"
    )]
    fn project_status(&self) -> Result<String, String> {
        tools::project::project_status(self)
    }

    #[tool(
        description = "Reindex specific files so subsequent queries see fresh symbols immediately. Not needed after Edit/Write when kungfu's edit hook is installed (plugin or `kungfu init --agent claude`) — those files are reindexed automatically; without the hook, pass the paths you changed. Always call it after shell edits, codegen or git checkout: the automatic staleness check is only a coarse safety net"
    )]
    fn reindex(&self, Parameters(params): Parameters<ReindexParam>) -> Result<String, String> {
        tools::project::reindex(self, params)
    }

    #[tool(
        description = "Return compact repo map: top directories, language distribution, entrypoints. Set include_onboard=true to add the onboarding summary (architecture, key symbols, naming, test layout, glossary with provenance) — useful for system prompts and CLAUDE.md"
    )]
    fn repo_outline(
        &self,
        Parameters(params): Parameters<RepoOutlineParam>,
    ) -> Result<String, String> {
        tools::project::repo_outline(self, params)
    }

    #[tool(
        description = "Return compact file structure: symbols, signatures, exports. Budget above tiny (default) also returns related files (imports, proximity, naming) and key symbols"
    )]
    fn file_outline(
        &self,
        Parameters(params): Parameters<FileOutlineParam>,
    ) -> Result<String, String> {
        tools::project::file_outline(self, params)
    }

    #[tool(
        description = "Search symbols by exact and fuzzy name match. Use this when you know (or can guess) the symbol's name. For conceptual queries without a known name, prefer `semantic_search`."
    )]
    fn find_symbol(&self, Parameters(params): Parameters<QueryParam>) -> Result<String, String> {
        tools::search::find_symbol(self, params)
    }

    #[tool(
        description = "Find files by path or keywords, ranked by the better of path match and the best-matching symbol each file defines — so a keyword also finds files by what they contain. Each hit carries `match`: \"path\" or \"symbol: <name>\". Not a literal content grep"
    )]
    fn find_files(&self, Parameters(params): Parameters<QueryParam>) -> Result<String, String> {
        tools::search::find_files(self, params)
    }

    #[tool(
        description = "Smart context retrieval: parse task intent, run multi-strategy search (symbols, text, related files, import chains), return ranked context packet. Diff-aware: lists uncommitted `changed_files` and marks items in them \"recently changed\" in `why`. Use 'include' to select layers: code (default), rationale (design decisions, TODOs), history (evolution timeline)"
    )]
    fn ask_context(
        &self,
        Parameters(params): Parameters<AskContextParam>,
    ) -> Result<String, String> {
        tools::context::ask_context(self, params)
    }

    #[tool(
        description = "Composite: explore a symbol in one call — find + detail + related symbols in same file + code snippet. Replaces find_symbol → get_symbol → find_related_symbols chain"
    )]
    fn explore_symbol(
        &self,
        Parameters(params): Parameters<SymbolBudgetParam>,
    ) -> Result<String, String> {
        tools::context::explore_symbol(self, params)
    }

    #[tool(
        description = "Edit-ready context for a symbol: FULL verbatim body (never truncated — usable directly as Edit old_string) plus sibling signatures, callees, callers, and attached rationale. Use this instead of explore_symbol + Read when you are about to modify the symbol"
    )]
    fn edit_context(
        &self,
        Parameters(params): Parameters<EditContextParam>,
    ) -> Result<String, String> {
        tools::context::edit_context(self, params)
    }

    #[tool(
        description = "Parse a stack trace, panic, or traceback and return a context packet of the involved symbols plus their siblings. Recognises Rust panic + backtrace, JS stack, Python traceback, Go panic"
    )]
    fn debug_trace(
        &self,
        Parameters(params): Parameters<DebugTraceParam>,
    ) -> Result<String, String> {
        tools::context::debug_trace(self, params)
    }

    #[tool(
        description = "Find all symbols that call the given symbol (callers / 'who calls this?')"
    )]
    fn callers(&self, Parameters(params): Parameters<SymbolBudgetParam>) -> Result<String, String> {
        tools::graph::callers(self, params)
    }

    #[tool(
        description = "Find all symbols that the given symbol calls (callees / 'what does this call?')"
    )]
    fn callees(&self, Parameters(params): Parameters<SymbolBudgetParam>) -> Result<String, String> {
        tools::graph::callees(self, params)
    }

    #[tool(
        description = "Find symbols by concept rather than name. When local embeddings are built (`project_status` shows embeddings.ready), runs vector cosine top-K; otherwise falls back to keyword expansion. Use this when you do NOT know a likely symbol name — for known names, use `find_symbol`."
    )]
    fn semantic_search(
        &self,
        Parameters(params): Parameters<QueryParam>,
    ) -> Result<String, String> {
        tools::search::semantic_search(self, params)
    }

    #[tool(
        description = "Why is it like this / what changed: git history + blame + evolution timeline (introduced, churn, linked decisions) for a file (`path`) or a symbol (`symbol`) — pass exactly one"
    )]
    fn history(&self, Parameters(params): Parameters<HistoryParam>) -> Result<String, String> {
        tools::history::history(self, params)
    }

    #[tool(
        description = "Find largest symbols or files (hotspots), optionally weighted by git churn frequency. Use to identify complex code, refactoring candidates, and bug-prone areas"
    )]
    fn hotspots(&self, Parameters(params): Parameters<HotspotsParam>) -> Result<String, String> {
        tools::review::hotspots(self, params)
    }

    #[tool(
        description = "Blast radius analysis: find all transitive callers and dependents of a symbol. Shows affected code, test files, and risk level (LOW/MEDIUM/HIGH)"
    )]
    fn affected(&self, Parameters(params): Parameters<AffectedParam>) -> Result<String, String> {
        tools::review::affected(self, params)
    }

    #[tool(
        description = "The post-edit check and code review in one call: changed symbols in the working-tree diff (incl. untracked/deleted files), blast radius (transitive callers), the minimal test set, touched public contracts, missing co-changes (files that usually change together per git history but are not in the diff), untested changed functions, and an overall risk with reasons. Call this after finishing a series of edits, before declaring the work done or reviewing a diff"
    )]
    fn verify_change(
        &self,
        Parameters(params): Parameters<VerifyChangeParam>,
    ) -> Result<String, String> {
        tools::review::verify_change(self, params)
    }

    #[tool(
        description = "Analyze module coupling: fan-in (who depends on this), fan-out (what this depends on), co-change frequency. Identifies fragile modules with high risk"
    )]
    fn coupling(&self, Parameters(params): Parameters<CouplingParam>) -> Result<String, String> {
        tools::review::coupling(self, params)
    }

    #[tool(
        description = "Record a one-line purpose (and optional glossary terms) for a file you now understand. Stored durably, merged into the index with purpose_source=agent (an authored module doc keeps precedence), and picked up by the file's search vector"
    )]
    fn annotate_file(
        &self,
        Parameters(params): Parameters<AnnotateFileParam>,
    ) -> Result<String, String> {
        tools::annotate::annotate_file(self, params)
    }

    #[tool(
        description = "Add a project memory entry. Kinds: fact, decision, warning, session_summary. Use to preserve important project knowledge for future sessions"
    )]
    fn memory_add(&self, Parameters(params): Parameters<MemoryAddParam>) -> Result<String, String> {
        tools::memory::memory_add(self, params)
    }

    #[tool(
        description = "Search project memory by query, or list entries when query is empty/omitted (filter with kind/tag/pinned). Returns {results,total} (facts, decisions, warnings, session summaries), capped by budget with a truncation note"
    )]
    fn memory_search(
        &self,
        Parameters(params): Parameters<MemorySearchParam>,
    ) -> Result<String, String> {
        tools::memory::memory_search(self, params)
    }

    #[tool(description = "Get a single project memory entry by ID")]
    fn memory_get(&self, Parameters(params): Parameters<MemoryIdParam>) -> Result<String, String> {
        tools::memory::memory_get(self, params)
    }

    #[tool(
        description = "Update a project memory entry: content, title, tags, pinned state, or status (status \"archived\" archives it — removed from normal retrieval but kept for history; \"active\" restores)"
    )]
    fn memory_update(
        &self,
        Parameters(params): Parameters<MemoryUpdateParam>,
    ) -> Result<String, String> {
        tools::memory::memory_update(self, params)
    }
}

#[tool_handler]
impl ServerHandler for KungfuMcp {
    fn get_info(&self) -> ServerInfo {
        // rmcp's default server_info is built from rmcp's own build env ("rmcp" + its
        // version); clients show this in /mcp, so report kungfu's name and version.
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("kungfu", env!("CARGO_PKG_VERSION")))
            .with_instructions(kungfu_project::templates::MCP_INSTRUCTIONS)
    }
}

pub async fn run_stdio_server(project_root: PathBuf) -> Result<()> {
    info!("starting kungfu MCP server (stdio)");

    // The one long-lived process is the right place to refresh the release check:
    // once at startup, on a background thread, cached 24h machine-wide, silent on
    // failure. Tool handlers only ever read the cache.
    kungfu_update::spawn_background_check(
        kungfu_config::KungfuConfig::load_merged(Some(
            &project_root.join(".kungfu").join("config.toml"),
        ))
        .unwrap_or_default()
        .update,
    );

    let server = KungfuMcp::new(project_root.clone());
    let transport = rmcp::transport::io::stdio();
    let service = server.serve(transport).await?;

    // Best-effort: locate the project root and config for the watcher.
    // The watcher only runs when explicitly enabled (`watch = true`); otherwise
    // freshness is handled lazily by `ensure_fresh_index` on each tool call, which
    // avoids a background re-index loop pinning CPU/memory.
    match kungfu_core::KungfuService::open(&project_root) {
        Ok(svc) if svc.config().watch => {
            let peer = service.peer().clone();
            // Bridge filesystem changes to MCP notifications so connected clients
            // can invalidate any cached assumptions about the index.
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let config = svc.config().clone();
            let watcher_root = project_root.clone();
            let index_dir = project_root.join(".kungfu").join("index");
            std::thread::spawn(move || {
                if let Err(e) = kungfu_index::watcher::watch_and_index(
                    &watcher_root,
                    config,
                    &index_dir,
                    move |_stats| {
                        let _ = tx.send(());
                    },
                ) {
                    warn!("kungfu watcher exited: {}", e);
                }
            });

            tokio::spawn(async move {
                while rx.recv().await.is_some() {
                    if let Err(e) = peer
                        .notify_resource_updated(ResourceUpdatedNotificationParam::new(
                            "kungfu://index",
                        ))
                        .await
                    {
                        warn!("notify_resource_updated failed: {}", e);
                    }
                }
            });
        }
        Ok(_) => {
            debug!("kungfu MCP: watch disabled, relying on lazy ensure_fresh_index");
        }
        Err(_) => {
            warn!("kungfu MCP: index missing, push-notifications disabled");
        }
    }

    service.waiting().await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_info_reports_kungfu_version() {
        let info = KungfuMcp::new(PathBuf::from(".")).get_info();
        assert_eq!(info.server_info.name, "kungfu");
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
    }

    /// The MCP surface is a deliberate contract (v2.7.5: 40 → 23 tools). Adding or
    /// removing a tool must update this list, MCP_INSTRUCTIONS and the README.
    #[test]
    fn mcp_tool_set_is_the_consolidated_surface() {
        let mut names: Vec<String> = KungfuMcp::tool_router()
            .list_all()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        names.sort();
        let mut expected = vec![
            "affected",
            "annotate_file",
            "ask_context",
            "callees",
            "callers",
            "coupling",
            "debug_trace",
            "edit_context",
            "explore_symbol",
            "file_outline",
            "find_files",
            "find_symbol",
            "history",
            "hotspots",
            "memory_add",
            "memory_get",
            "memory_search",
            "memory_update",
            "project_status",
            "reindex",
            "repo_outline",
            "semantic_search",
            "verify_change",
        ];
        expected.sort();
        assert_eq!(names, expected);
    }

    #[test]
    fn history_requires_exactly_one_target() {
        let mcp = KungfuMcp::new(PathBuf::from("."));
        for (path, symbol) in [(None, None), (Some("a.rs"), Some("f"))] {
            let err = tools::history::history(
                &mcp,
                HistoryParam {
                    path: path.map(String::from),
                    symbol: symbol.map(String::from),
                    budget: None,
                },
            )
            .unwrap_err();
            assert!(err.contains("`path`") && err.contains("`symbol`"));
        }
    }

    fn temp_project(tag: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!("kungfu_mcp_{tag}_{}", std::process::id()));
        std::fs::remove_dir_all(&tmp).ok();
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(
            tmp.join("src/engine.rs"),
            "pub fn compute_widget_total() -> u32 { 1 }\n",
        )
        .unwrap();
        std::fs::write(
            tmp.join("src/widget.rs"),
            "//! Widget helpers.\n\npub fn make() {}\n",
        )
        .unwrap();
        std::fs::write(tmp.join("src/bare.rs"), "pub fn bare() {}\n").unwrap();
        kungfu_project::init_project(&tmp).unwrap();
        kungfu_core::KungfuService::open(&tmp)
            .unwrap()
            .index_full()
            .unwrap();
        tmp
    }

    fn json(s: Result<String, String>) -> serde_json::Value {
        serde_json::from_str(&s.unwrap()).unwrap()
    }

    #[test]
    fn project_status_has_embeddings_block() {
        let tmp = temp_project("status");
        let mcp = KungfuMcp::new(tmp.clone());
        let v = json(tools::project::project_status(&mcp));
        let emb = &v["embeddings"];
        for field in [
            "ready",
            "feature",
            "model",
            "vectors",
            "job_running",
            "hint",
        ] {
            assert!(!emb[field].is_null(), "embeddings.{field} missing: {emb}");
        }
        // Nothing was built in the fixture, so it cannot claim readiness.
        assert_eq!(emb["ready"], false);
        assert_eq!(emb["vectors"], 0);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn repo_outline_onboard_is_opt_in() {
        let tmp = temp_project("outline");
        let mcp = KungfuMcp::new(tmp.clone());
        let plain = json(tools::project::repo_outline(
            &mcp,
            RepoOutlineParam {
                budget: None,
                include_onboard: None,
            },
        ));
        assert!(plain.get("onboard").is_none());
        assert!(plain["directories"].is_array());

        let full = json(tools::project::repo_outline(
            &mcp,
            RepoOutlineParam {
                budget: None,
                include_onboard: Some(true),
            },
        ));
        let onboard = &full["onboard"];
        for field in [
            "architecture",
            "key_symbols",
            "naming_style",
            "test_pattern",
            "glossary",
        ] {
            assert!(!onboard[field].is_null(), "onboard.{field} missing");
        }
        assert!(full["directories"].is_array());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn file_outline_budget_adds_related_files() {
        let tmp = temp_project("file");
        let mcp = KungfuMcp::new(tmp.clone());
        let tiny = json(tools::project::file_outline(
            &mcp,
            FileOutlineParam {
                path: "src/bare.rs".into(),
                budget: None,
            },
        ));
        assert!(tiny["symbols"].is_array());
        assert!(tiny.get("related_files").is_none());
        assert!(tiny.get("key_symbols").is_none());
        // explore_file's drip annotation survives the merge.
        assert!(tiny["annotation_hint"].is_string());

        let small = json(tools::project::file_outline(
            &mcp,
            FileOutlineParam {
                path: "src/bare.rs".into(),
                budget: Some("small".into()),
            },
        ));
        assert!(small["symbols"].is_array());
        assert_eq!(small["key_symbols"], serde_json::json!(["bare"]));
        let related = small["related_files"].as_array().unwrap();
        assert!(
            related.iter().any(|r| r["path"] == "src/engine.rs"),
            "same-dir sibling expected in related_files: {related:?}"
        );
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn find_files_matches_by_path_and_by_contained_symbol() {
        let tmp = temp_project("find");
        let mcp = KungfuMcp::new(tmp.clone());
        let query = |q: &str| {
            json(tools::search::find_files(
                &mcp,
                QueryParam {
                    query: q.into(),
                    budget: None,
                    scope: None,
                },
            ))
        };

        let by_symbol = query("compute_widget_total");
        let top = &by_symbol[0];
        assert_eq!(top["path"], "src/engine.rs");
        assert_eq!(top["match"], "symbol: compute_widget_total");

        let by_path = query("widget");
        let top = &by_path[0];
        assert_eq!(top["path"], "src/widget.rs");
        assert_eq!(top["match"], "path");

        let none = query("zzqx");
        assert_eq!(none["results"], serde_json::json!([]));
        assert!(none["hint"].is_string());
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn scoped_find_files_filters_before_budget_trim() {
        let tmp = temp_project("find_scope");
        // Four path matches outside the scope outrank the in-scope symbol match,
        // filling the tiny (3) page if scope were applied after the trim.
        for name in ["widget_a", "widget_b", "widget_c", "widget_d"] {
            std::fs::write(tmp.join(format!("src/{name}.rs")), "pub fn x() {}\n").unwrap();
        }
        std::fs::create_dir_all(tmp.join("other")).unwrap();
        std::fs::write(tmp.join("other/misc.rs"), "pub fn widget_thing() {}\n").unwrap();
        kungfu_core::KungfuService::open(&tmp)
            .unwrap()
            .index_full()
            .unwrap();
        let mcp = KungfuMcp::new(tmp.clone());
        let query = |scope: &str| {
            json(tools::search::find_files(
                &mcp,
                QueryParam {
                    query: "widget".into(),
                    budget: Some("tiny".into()),
                    scope: Some(scope.into()),
                },
            ))
        };

        let scoped = query("other/");
        assert_eq!(scoped[0]["path"], "other/misc.rs");
        assert_eq!(scoped[0]["match"], "symbol: widget_thing");

        let empty = query("nowhere/");
        assert_eq!(empty["results"], serde_json::json!([]));
        assert!(empty["hint"]
            .as_str()
            .unwrap()
            .contains("in scope 'nowhere/'"));
        std::fs::remove_dir_all(&tmp).ok();
    }
}
