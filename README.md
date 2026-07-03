# kungfu

Local context retrieval engine for coding agents. Indexes a codebase, resolves its dependency and call graph, and returns the smallest useful context packet — so agents read fewer files and burn fewer tokens.

## Why

Agents waste tokens exploring: globbing, grepping, reading whole files to find three relevant lines. Kungfu replaces that with one call:

```
$ kungfu ask-context "add C++ language support" --budget tiny
Task:   add C++ language support
Intent: lookup
Budget: tiny
Items:  3

  0.50  [crates/kungfu-parse/src/lib.rs] extract_symbols
  0.45  [crates/kungfu-config/src/lib.rs] LanguagesConfig
  0.45  [crates/kungfu-types/src/file.rs] Language
```

**~275 tokens** to point at the right code, versus thousands to read a single file blindly. Every response declares its own limits — which strategy ran, what was truncated, which index layer is missing — so a partial answer never looks like a confident one.

## Install

Single binary, no runtime dependencies:

```sh
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.sh | sh

# Windows (PowerShell)
irm https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.ps1 | iex
```

Prebuilt for macOS (ARM64, x86_64), Linux (ARM64, x86_64), Windows (x86_64). Or build from source:

```sh
git clone https://github.com/denyzhirkov/kungfu.git
cd kungfu && cargo build --release
cp target/release/kungfu ~/.local/bin/
```

## Quick start

```sh
kungfu init      # create .kungfu/ in the project root
kungfu index     # build the index
kungfu doctor    # validate install + index (--fix repairs common issues)
```

## CLI commands

All commands accept `--json` for machine output and `--budget tiny|small|medium|full`.

```sh
# Retrieval — the core commands
kungfu ask-context "where is JWT refresh implemented" --budget small
kungfu edit-context refreshToken            # full verbatim body + contracts, edit-ready
kungfu diff-context --budget small          # context from the current git diff

# Search
kungfu find-symbol AuthService              # symbols by name (exact + fuzzy + stem)
kungfu get-symbol refreshToken              # exact symbol lookup
kungfu search "refresh token"               # text search across files
kungfu search "auth logic" --semantic       # concept search (see Semantic search)

# Structure & analysis
kungfu repo-outline                         # compact repo map
kungfu file-outline src/auth/service.ts     # symbols in a file
kungfu onboard                              # architecture, patterns, key symbols
kungfu callers AuthService / callees ...    # call graph in either direction
kungfu affected AuthService --depth 3       # blast radius (--staged for the current diff)
kungfu verify-change                        # post-edit: changed symbols + blast radius + tests
kungfu smart-test                           # minimal test set for the current diff
kungfu review / coupling / hotspots --churn # review context, module coupling, churn

# Debug & history
cargo run 2>&1 | kungfu debug-trace         # parse a stack trace, get involved symbols
kungfu commit-context <hash> --budget small
kungfu pr-context 42 --budget small         # requires the `gh` CLI
kungfu change-timeline AuthService          # how a symbol evolved: introduced, churn, decisions

# Project memory
kungfu memory add "Use zod for all new validation" --kind decision --tag validation --pin
kungfu memory search "validation" / list / show <id> / pin <id> / archive <id>

# Maintenance
kungfu index --full | --changed | --only src/foo.rs
kungfu watch                                # auto re-index on file changes
kungfu clean / config / export --format jsonl
kungfu embeddings status | build            # vector-search readiness
```

## ask-context

The highest-value command. Given a task, it detects **intent**, runs multiple search strategies, applies contextual bonuses (changed files, test/config proximity, language weighting), and returns a ranked packet with signatures, snippets, and matched design rationale.

| Intent | Triggers | Extra strategies |
|--------|----------|-----------------|
| `lookup` | find, where, show, search | symbol + text search |
| `debug` | bug, fix, error, crash | + related files, error-symbol boost |
| `understand` | how, explain, what, why | + sibling symbols from the same file |
| `impact` | impact, refactor, rename, delete | + import chain, sibling symbols |

| Budget | Items | Snippets | Use case |
|--------|-------|----------|----------|
| `auto` | adaptive | adaptive | **Default** — scales to project size |
| `tiny` | 3 | none | "where to look" pointers |
| `small` | 5 | 20 lines | signatures + context |
| `medium` | 8 | 40 lines | deeper exploration |
| `full` | 12 | 100 lines | complete picture |

Pinned memory entries and warnings are injected into every packet, so agents don't restart from zero each session. When two active entries cover the same topic with different content, a `memory_conflicts` block surfaces instead of silently picking one. Ranking weights are tunable via `KUNGFU_W_*` env vars.

## Semantic search

`kungfu search --semantic` (CLI) / `semantic_search` (MCP) has **two modes** behind one call site; the right one runs automatically.

- **Keyword mode** — works with zero setup. Splits the query into keywords, widens each with a small synonym table, and searches symbol names. Fast, decent when the query shares vocabulary with the code.
- **Vector mode** — with the embedding model installed, runs cosine top-K over 384-dim sentence embeddings. Finds concepts even when query and symbol share no words (*"graceful degradation when index is missing"* → `ensure_fresh_index`).

Inference ships compiled-in by default. Enable vectors with one command:

```sh
kungfu embeddings build   # downloads BAAI/bge-small-en-v1.5 (~130MB, once), then embeds every symbol
```

From then on vectors maintain themselves — the MCP server re-embeds changed symbols in the background after each reindex and prunes deleted ones. Slim builds (`cargo build --no-default-features`) skip inference and always use keyword mode. The output labels which mode ran (`"mode": "vector"` vs `"keyword_fallback"`) with a `hint` for the next step; `embeddings status` reports readiness and whether a background build is in flight.

`ask_context` also uses vectors when available, adding cosine top-K hits alongside its lexical strategies — so it catches concepts the lexical passes miss without displacing them.

## MCP server

```sh
kungfu mcp
```

Add to your agent config (Claude Code, Cursor, etc.):

```json
{
  "mcpServers": {
    "kungfu": { "command": "kungfu", "args": ["mcp"] }
  }
}
```

### Tools (38)

**Retrieval** — `ask_context` (intent + multi-strategy search + rationale + memory), `edit_context` (full verbatim body + contracts, edit-ready), `explore_symbol`, `explore_file`, `investigate`, `debug_trace`, `commit_context`, `pr_context`.

**Search** — `find_symbol`, `search_text`, `find_files`, `semantic_search`, `embeddings_status`, `embeddings_build`.

**Structure** — `project_status`, `repo_outline`, `file_outline`, `onboard`.

**Graph & impact** — `callers`, `callees`, `affected`, `coupling`, `smart_test`, `test_subjects`, `verify_change`, `review`, `hotspots`.

**History** — `file_history`, `symbol_history`, `change_timeline`.

**Memory** — `memory_add`, `memory_search`, `memory_list`, `memory_get`, `memory_update`, `memory_archive`.

**Freshness & stats** — `reindex` (targeted reindex right after an edit), `usage_stats`.

While connected, the server pushes `notifications/resources/updated` (URI `kungfu://index`) whenever the index changes on disk — subscribing agents can invalidate stale assumptions.

## Agent rules

Add to `CLAUDE.md` or the system prompt. Keep it about *policy and routing* — the agent already sees tool descriptions from MCP. The block below is a working minimum; copy it verbatim.

```markdown
## kungfu — context retrieval (use BEFORE Read / grep / find)

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
exact path. Otherwise, if you reach for Read / grep / find — stop and route above.
```

### Auto-reindex on edit (Claude Code hook)

Wire reindex into the harness so every `Edit`/`Write` triggers a targeted reindex of that file (~10 ms) — no reliance on the agent remembering. Add to `.claude/settings.json`:

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          { "type": "command",
            "command": "jq -r '.tool_input.file_path // empty' | xargs -I{} kungfu index --only {} >/dev/null 2>&1 || true" }
        ]
      }
    ]
  }
}
```

The lazy staleness check stays as a safety net for files changed outside the agent (git pull, formatters, codegen).

### Token savings (open-source projects)

kungfu vs. naive grep + read, tokens to answer the same query:

| Project | Lang | Files | Query | kungfu | grep+read | Savings |
|---------|------|------:|-------|-------:|----------:|:-------:|
| [ruff](https://github.com/astral-sh/ruff) | Rust | 9,702 | "how does the linter check rules" | 722 | 137,767 | **190x** |
| [ollama](https://github.com/ollama/ollama) | Go | 1,834 | "how does model loading and inference work" | 1,100 | 110,122 | **100x** |
| [SolidJS](https://github.com/solidjs/solid) | TS | 168 | "how does the reactive signal system work" | 459 | 33,577 | **73x** |
| [leptos](https://github.com/leptos-rs/leptos) | Rust | 1,453 | "how does reactive rendering work" | 593 | 40,772 | **68x** |
| [pydantic](https://github.com/pydantic/pydantic) | Python | 729 | "how does field validation work" | 977 | 53,864 | **55x** |
| [FastAPI](https://github.com/fastapi/fastapi) | Python | 2,882 | "how does dependency injection work" | 624 | 17,089 | **27x** |

## How it works

**Indexing** — scans files respecting `.gitignore`; parses with [tree-sitter](https://tree-sitter.github.io/) (Rust, TypeScript, JavaScript, Python, Go, Java, C#, Kotlin, C, C++); extracts symbols, imports (resolved to real files), a function call graph, and structured comments (TODO/FIXME/NOTE/doc). Incremental via blake3 fingerprints; oversized and binary files are recorded by name only.

**Search & ranking** — exact (1.0) → prefix (0.9) → contains (0.7) → stem (0.6) → fuzzy (0.4), plus `snake_case`/`camelCase` phrase matching and light English stemming.

**Context assembly** — multi-strategy search (symbols, text, related files, import chains, semantic), dedup by (path, name), git changed-file bonus, test/config proximity, language weighting, all trimmed to the budget.

**Memory & rationale** — answers *"why is it like this?"*, not just *"where is it?"*. Doc/ADR markdown and code comments become searchable memory; `ask_context` returns matched decisions and verbatim evidence excerpts alongside code. Manual project memory is stored as one human-readable, git-diffable `.md` file per entry with a derived inverted index.

### Storage

```
.kungfu/
  config.toml            # project configuration
  index/
    files.json           # indexed files + hashes
    symbols.json         # extracted symbols + spans
    relations.json       # imports / test_for / config_for / calls
    fingerprints.json    # blake3 hashes for incremental rebuilds
    memories.json        # derived rationale: comments, doc sections, ADR decisions
  memory/
    mem_NNNN.md          # manual project memory (source of truth)
    manifest.json        # derived metadata + inverted index
```

## Configuration

`.kungfu/config.toml`:

```toml
project_name = "my-project"

[ignore]
paths = ["node_modules", "dist", "build", ".git", "target"]

[languages]
enabled = ["typescript", "javascript", "rust", "go", "python", "java", "csharp", "kotlin", "c", "cpp", "json", "markdown", "yaml", "toml"]

[search]
default_budget = "small"
default_top_k = 5

[index]
incremental = true
max_file_bytes = 2097152   # skip files larger than this (2 MiB)

[git]
enabled = true
```

## Benchmarks

Indexing and query latency across popular projects (Apple Silicon):

| Project | Language | Files | Symbols | Index | ask-context |
|---------|----------|------:|--------:|------:|------------:|
| express | JS       |   201 |   1,948 |  0.5s |       227ms |
| gin     | Go       |   118 |   1,487 |  0.7s |       217ms |
| axum    | Rust     |   474 |   2,771 |  1.3s |       269ms |
| cargo   | Rust     | 2,718 |  12,009 | 17.4s |       783ms |
| django  | Python   | 6,907 |  42,917 | 37.9s |     2,125ms |
| ruff    | Rust     | 9,702 |  42,239 | 67.2s |     2,300ms |
| go      | Go       |14,022 | 105,497 |186.8s |     4,661ms |

See [BENCHMARKS.md](BENCHMARKS.md) for full results.

## License

MIT
