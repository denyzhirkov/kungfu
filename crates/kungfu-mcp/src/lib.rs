use anyhow::Result;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ResourceUpdatedNotificationParam, ServerCapabilities, ServerInfo};
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
    #[tool(description = "Show project status: file count, symbol count, languages, git status")]
    fn project_status(&self) -> Result<String, String> {
        tools::project::project_status(self)
    }

    #[tool(
        description = "Reindex specific files after you create/edit/delete them. Call this with the paths you just touched so subsequent queries see fresh symbols immediately — much faster and more reliable than waiting for the automatic staleness check"
    )]
    fn reindex(&self, Parameters(params): Parameters<ReindexParam>) -> Result<String, String> {
        tools::project::reindex(self, params)
    }

    #[tool(
        description = "Return compact repo map: top directories, language distribution, entrypoints"
    )]
    fn repo_outline(&self, Parameters(params): Parameters<BudgetParam>) -> Result<String, String> {
        tools::project::repo_outline(self, params)
    }

    #[tool(description = "Return compact file structure: symbols, signatures, exports")]
    fn file_outline(
        &self,
        Parameters(params): Parameters<FilePathParam>,
    ) -> Result<String, String> {
        tools::project::file_outline(self, params)
    }

    #[tool(
        description = "Search symbols by exact and fuzzy name match. Use this when you know (or can guess) the symbol's name. For conceptual queries without a known name, prefer `semantic_search`."
    )]
    fn find_symbol(&self, Parameters(params): Parameters<QueryParam>) -> Result<String, String> {
        tools::search::find_symbol(self, params)
    }

    #[tool(description = "Search text across indexed files by path and name matching")]
    fn search_text(&self, Parameters(params): Parameters<QueryParam>) -> Result<String, String> {
        tools::search::search_text(self, params)
    }

    #[tool(description = "Find files by path pattern or keywords")]
    fn find_files(&self, Parameters(params): Parameters<QueryParam>) -> Result<String, String> {
        tools::search::find_files(self, params)
    }

    #[tool(
        description = "Smart context retrieval: parse task intent, run multi-strategy search (symbols, text, related files, import chains), return ranked context packet. Use 'include' to select layers: code (default), rationale (design decisions, TODOs), history (evolution timeline)"
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
        description = "Composite: explore a file in one call — outline + related files + key symbols. Replaces file_outline → find_related_files chain"
    )]
    fn explore_file(
        &self,
        Parameters(params): Parameters<FilePathBudgetParam>,
    ) -> Result<String, String> {
        tools::context::explore_file(self, params)
    }

    #[tool(
        description = "Composite: investigate a query in one call — smart context retrieval + diff awareness + snippets. Replaces ask_context → diff_context chain"
    )]
    fn investigate(&self, Parameters(params): Parameters<QueryParam>) -> Result<String, String> {
        tools::context::investigate(self, params)
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
        description = "Find symbols by concept rather than name. When local embeddings are built (`embeddings_status` shows ready), runs vector cosine top-K; otherwise falls back to keyword expansion. Use this when you do NOT know a likely symbol name — for known names, use `find_symbol`."
    )]
    fn semantic_search(
        &self,
        Parameters(params): Parameters<QueryParam>,
    ) -> Result<String, String> {
        tools::search::semantic_search(self, params)
    }

    #[tool(description = "Get git history for a file: recent commits with date, author, message")]
    fn file_history(
        &self,
        Parameters(params): Parameters<FilePathParam>,
    ) -> Result<String, String> {
        tools::history::file_history(self, params)
    }

    #[tool(description = "Get git blame + recent commits for a symbol: who changed it and why")]
    fn symbol_history(
        &self,
        Parameters(params): Parameters<SymbolNameParam>,
    ) -> Result<String, String> {
        tools::history::symbol_history(self, params)
    }

    #[tool(
        description = "Change timeline: show how a symbol or file evolved — when introduced, churn rate, linked decisions, recent changes"
    )]
    fn change_timeline(
        &self,
        Parameters(params): Parameters<SymbolNameParam>,
    ) -> Result<String, String> {
        tools::history::change_timeline(self, params)
    }

    #[tool(
        description = "Build a context packet focused on a specific git commit: scored symbols overlapping the commit's hunks + commit metadata in history"
    )]
    fn commit_context(
        &self,
        Parameters(params): Parameters<CommitContextParam>,
    ) -> Result<String, String> {
        tools::history::commit_context(self, params)
    }

    #[tool(
        description = "Build a context packet covering all commits in a GitHub PR (requires `gh` CLI). Merges hunks across commits, lists each commit in history"
    )]
    fn pr_context(&self, Parameters(params): Parameters<PrContextParam>) -> Result<String, String> {
        tools::history::pr_context(self, params)
    }

    #[tool(description = "Show usage statistics: token savings, cache hit rate, calls served")]
    fn usage_stats(&self) -> Result<String, String> {
        tools::review::usage_stats(self)
    }

    #[tool(
        description = "Find largest symbols or files (hotspots), optionally weighted by git churn frequency. Use to identify complex code, refactoring candidates, and bug-prone areas"
    )]
    fn hotspots(&self, Parameters(params): Parameters<HotspotsParam>) -> Result<String, String> {
        tools::review::hotspots(self, params)
    }

    #[tool(
        description = "Generate project onboarding summary: architecture, patterns, key symbols, naming conventions, test structure. Perfect for system prompts and CLAUDE.md"
    )]
    fn onboard(&self) -> Result<String, String> {
        tools::review::onboard(self)
    }

    #[tool(
        description = "Blast radius analysis: find all transitive callers and dependents of a symbol. Shows affected code, test files, and risk level (LOW/MEDIUM/HIGH)"
    )]
    fn affected(&self, Parameters(params): Parameters<AffectedParam>) -> Result<String, String> {
        tools::review::affected(self, params)
    }

    #[tool(
        description = "Find minimal set of tests to run based on git diff. Analyzes changed symbols, traces call graph to test functions, returns specific test names"
    )]
    fn smart_test(&self) -> Result<String, String> {
        tools::review::smart_test(self)
    }

    #[tool(
        description = "Reverse of smart_test: given a test function name, return the production code it exercises (callees up to 2 hops via thin test helpers)"
    )]
    fn test_subjects(
        &self,
        Parameters(params): Parameters<SymbolNameParam>,
    ) -> Result<String, String> {
        tools::review::test_subjects(self, params)
    }

    #[tool(
        description = "Code review context for current git diff: changed symbols, missing co-changes (files that usually change together), untested code, risk assessment"
    )]
    fn review(&self) -> Result<String, String> {
        tools::review::review(self)
    }

    #[tool(
        description = "Verify your edits in one call: changed symbols in the working-tree diff, blast radius (transitive callers), the minimal test set covering them, and any touched public contracts. Call this after finishing a series of edits, before declaring the work done"
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
        description = "Report whether semantic vector search is ready end-to-end: feature compiled, model installed, vectors built. Includes a one-line `hint` field with the next step if anything is missing. Always safe to call."
    )]
    fn embeddings_status(&self) -> Result<String, String> {
        tools::review::embeddings_status(self)
    }

    #[tool(
        description = "Build embeddings in the background (downloads model weights on first run). Returns immediately; poll embeddings_status until indexed_vectors catches up. Idempotent. After the first build, vectors auto-sync on every reindex — no need to call this again."
    )]
    fn embeddings_build(&self) -> Result<String, String> {
        tools::review::embeddings_build(self)
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
        description = "Files most worth annotating: no purpose from any source, ranked by how much of the project imports them. Returns items + the expected workflow (understand via file_outline, then annotate_file)"
    )]
    fn annotation_queue(
        &self,
        Parameters(params): Parameters<AnnotationQueueParam>,
    ) -> Result<String, String> {
        tools::annotate::annotation_queue(self, params)
    }

    #[tool(
        description = "Add a project memory entry. Kinds: fact, decision, warning, session_summary. Use to preserve important project knowledge for future sessions"
    )]
    fn memory_add(&self, Parameters(params): Parameters<MemoryAddParam>) -> Result<String, String> {
        tools::memory::memory_add(self, params)
    }

    #[tool(
        description = "Search project memory by query. Returns scored results matching facts, decisions, warnings, and session summaries"
    )]
    fn memory_search(
        &self,
        Parameters(params): Parameters<MemorySearchParam>,
    ) -> Result<String, String> {
        tools::memory::memory_search(self, params)
    }

    #[tool(description = "List project memory entries with optional filters")]
    fn memory_list(
        &self,
        Parameters(params): Parameters<MemoryListParam>,
    ) -> Result<String, String> {
        tools::memory::memory_list(self, params)
    }

    #[tool(description = "Get a single project memory entry by ID")]
    fn memory_get(&self, Parameters(params): Parameters<MemoryIdParam>) -> Result<String, String> {
        tools::memory::memory_get(self, params)
    }

    #[tool(description = "Update a project memory entry: content, title, tags, or pinned state")]
    fn memory_update(
        &self,
        Parameters(params): Parameters<MemoryUpdateParam>,
    ) -> Result<String, String> {
        tools::memory::memory_update(self, params)
    }

    #[tool(
        description = "Archive a project memory entry — removes from normal retrieval but preserves for history"
    )]
    fn memory_archive(
        &self,
        Parameters(params): Parameters<MemoryIdParam>,
    ) -> Result<String, String> {
        tools::memory::memory_archive(self, params)
    }
}

#[tool_handler]
impl ServerHandler for KungfuMcp {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_instructions("Kungfu is a context retrieval and distillation engine for coding agents. Use its tools to explore project structure, find symbols, search code, and get minimal context packets.")
    }
}

pub async fn run_stdio_server(project_root: PathBuf) -> Result<()> {
    info!("starting kungfu MCP server (stdio)");

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
