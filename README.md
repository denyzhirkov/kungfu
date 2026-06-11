# kungfu

Local context retrieval engine for coding agents. Indexes codebases, resolves dependencies, and delivers the smallest useful context packet — so agents read fewer files and waste fewer tokens.

## Why

Agents burn tokens exploring codebases: reading files, grepping, guessing structure. Kungfu replaces that with one call:

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

**275 tokens** instead of reading files manually (~7,500+ tokens per file).

## Install

Recommended — single binary, no dependencies:

```sh
# macOS / Linux
curl -fsSL https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.sh | sh

# Windows (PowerShell)
irm https://raw.githubusercontent.com/denyzhirkov/kungfu/master/install.ps1 | iex
```

Supports macOS (ARM64, x86_64), Linux (ARM64, x86_64), and Windows (x86_64).

Build from source:

```sh
git clone https://github.com/denyzhirkov/kungfu.git
cd kungfu
cargo build --release
cp target/release/kungfu ~/.local/bin/
```

## Quick start

```sh
kungfu init          # initialize in project root
kungfu index         # build the index
kungfu doctor        # verify everything works
```

## CLI commands

### Core retrieval

```sh
# Smart context — the main command
kungfu ask-context "find where JWT refresh is implemented" --budget small
kungfu ask-context "impact of changing Budget enum" --budget medium
kungfu ask-context "fix crash in python parser" --budget tiny

# Context from git changes
kungfu diff-context --budget small
```

### Search

```sh
kungfu find-symbol AuthService          # search symbols by name
kungfu get-symbol refreshToken          # exact symbol lookup
kungfu search "refresh token"           # text search across files
kungfu search "auth logic" --semantic   # vector search when embeddings are built (see below)
```

### Debug & history

```sh
# Parse a stack trace from stdin, get context for the involved code
cargo run 2>&1 | kungfu debug-trace --budget small

# Build context for a specific commit or PR
kungfu commit-context a0bab6d --budget small
kungfu pr-context 42 --budget small      # requires `gh` CLI
```

### Project memory (v2.0)

```sh
kungfu memory add "Use zod for all new validation" --kind decision --tag validation --pin
kungfu memory add "Legacy webhook uses old auth — do not refactor" --kind warning --tag auth
kungfu memory list                      # show all active entries
kungfu memory search "validation"       # search memory
kungfu memory pin mem_0001              # pin for higher context priority
kungfu memory archive mem_0002          # archive without deleting
```

Pinned entries and warnings are automatically injected into `ask-context` output, so agents don't restart from zero every session. When two active entries cover the same topic with different content, `ask-context` surfaces a `memory_conflicts` block instead of silently picking one.

### Structure

```sh
kungfu repo-outline                     # compact repo map
kungfu file-outline src/auth/service.ts # symbols in a file
```

### Analysis

```sh
kungfu onboard                          # project summary: architecture, patterns, key symbols
kungfu affected AuthService --depth 3   # blast radius: who depends on this symbol?
kungfu affected --staged --depth 3      # blast radius of the current staged diff
kungfu smart-test                       # minimal test set based on git diff
kungfu test-subjects test_login         # reverse: production code exercised by a test
kungfu review                           # code review context: risks, missing co-changes
kungfu coupling --top 10                # module coupling: fan-in, fan-out, co-change
kungfu hotspots --churn                 # largest/most changed code
```

### Interop

```sh
kungfu export --format jsonl > graph.jsonl   # dump files/symbols/relations/memory for external tools
```

### Maintenance

```sh
kungfu index --full                     # full rebuild
kungfu index --changed                  # reindex only git-changed files
kungfu index --only src/foo.rs          # reindex specific files (editor/agent hooks)
kungfu doctor                           # validate installation and index
kungfu watch                            # auto re-index on file changes
kungfu clean                            # wipe index and cache
kungfu config                           # show current configuration
kungfu embeddings status                # vector-search readiness (see below)
```

## Semantic search

`semantic_search` (MCP) / `kungfu search --semantic` (CLI) has **two modes** that share the same call site. The right one runs automatically based on what's available — the agent doesn't need to choose.

### Works out of the box: keyword mode

With a default build and no setup, `--semantic` runs **query expansion**: the query is split into keywords, each keyword is widened with a small synonym table (`auth` → `verify`, `token`, `session`, ...), and we search symbol names for the expansion. Fast, zero install, decent for queries that share vocabulary with the codebase.

### Works better with the model: vector mode

With a real embedding model installed, the same call runs **vector cosine top-K** over 384-dim sentence embeddings. Finds concepts even when the query and the symbol share no words ("graceful degradation when index is missing" → `ensure_fresh_index`).

Inference ships compiled-in by default. One command to enable:

```sh
kungfu embeddings build    # downloads BAAI/bge-small-en-v1.5 (~130MB, one-time), then embeds every symbol
```

From then on vectors maintain themselves: the MCP server re-embeds changed symbols
in the background after every reindex (including the `reindex` tool and the lazy
staleness check) and prunes vectors for deleted symbols. No manual re-runs.
Slim builds (`cargo build --no-default-features`) skip candle entirely and always
use keyword expansion.

### Picking the mode

| You see in the output | What ran | What to do |
|---|---|---|
| `"mode": "vector"` (+ `coverage`) | Real embeddings | Use the results |
| `"mode": "keyword_fallback"` | Query expansion fallback | Use the results; run `kungfu embeddings build` once for concept matching |

`kungfu embeddings status` (CLI) and the `embeddings_status` MCP tool both return the same struct with a one-line `hint` telling you the next step if anything is missing (plus `job_running` while a background build/sync is in flight). The MCP `embeddings_build` tool runs in the background — poll `embeddings_status` until `indexed_vectors` catches up.

### Side by side

Query *"rank symbols by relevance"* against this repo (822 symbols):

| Mode | Top hit | Score | Notes |
|---|---|---|---|
| keyword | `build_context_packet` | 0.40 | matches "rank" in symbol bodies |
| vector | `stem_match_ranking_to_rank` | 0.70 | no shared words with the query — concept-level match |

Same call, different signal density. Keyword is honest about what it can do; vector lifts the ceiling for everything you can't predict in advance.

### `ask_context` uses vectors too (when available)

`ask_context` runs a multi-strategy search; when an embedding store + a real engine are present, it adds **Strategy A2** — cosine top-K against the same vector store, capped at 5 hits, only when the query has at least 3 non-stop-word terms and each hit clears a cosine threshold of 0.55. This augments rather than replaces the existing strategies: vector hits land alongside symbol-name and content matches with a moderate base score, so they show up when lexical strategies miss the concept (e.g. *"graceful degradation when index is missing"* → `ensure_fresh_index`) and stay out of the way when lexical already nails it. Tunable via `KUNGFU_W_VECTOR` and `KUNGFU_W_VECTOR_MIN`.

All commands support `--json` for machine output and `--budget tiny|small|medium|full`.

## ask-context

The highest-value command. Given a task description, it:

1. **Detects intent** — lookup, debug, understand, or impact
2. **Runs multiple search strategies** — symbol search, text search, related files, import chains
3. **Applies contextual bonuses** — changed files (+0.3), test/config proximity (+0.15), language weighting, [tunable weights](#tuning)
4. **Returns a ranked packet** with signatures and code snippets

### Intent detection

| Intent | Triggers | Extra strategies |
|--------|----------|-----------------|
| `lookup` | find, where, show, search | symbol + text search |
| `debug` | bug, fix, error, crash | + related files, error symbol boost |
| `understand` | how, explain, what, why | + sibling symbols from same file |
| `impact` | impact, refactor, rename, delete | + import chain, sibling symbols |

### Budget levels

| Budget | Items | Snippets | Use case |
|--------|-------|----------|----------|
| `auto` | adaptive | adaptive | **Default** — adapts to project size |
| `tiny` | 3 | none | Quick pointers — "where to look" |
| `small` | 5 | 20 lines | Signatures + context |
| `medium` | 8 | 40 lines | Deeper exploration |
| `full` | 12 | 100 lines | Complete picture |

## MCP server

```sh
kungfu mcp
```

Add to your agent config (Claude Code, Cursor, etc.):

```json
{
  "mcpServers": {
    "kungfu": {
      "command": "kungfu",
      "args": ["mcp"]
    }
  }
}
```

### MCP tools

| Tool | Description |
|------|-------------|
| `project_status` | File count, symbol count, languages, git status |
| `reindex` | Reindex specific files right after editing them — agent-driven freshness instead of waiting for the staleness check |
| `repo_outline` | Top directories, language distribution, entrypoints |
| `file_outline` | Symbols, signatures, exports for a file |
| `find_symbol` | Search symbols by name (exact + fuzzy + stem) |
| `search_text` | Text search across indexed files |
| `find_files` | Find files by path pattern |
| `semantic_search` | Concept-level search; vector cosine top-K when embeddings are built, keyword expansion otherwise. Pick this when you don't know the exact symbol name |
| `embeddings_status` | Are vectors wired up? Returns model id, dim, feature/install/index state, and a one-line `hint` for the next step |
| `embeddings_build` | Build vectors in the background (downloads weights on first run). Idempotent; after the first build, vectors auto-sync on every reindex |
| `ask_context` | Smart retrieval: intent + multi-strategy search + rationale + project memory + conflict surfacing |
| `debug_trace` | Parse a stack trace (Rust panic, JS stack, Python traceback, Go panic) and return involved symbols + siblings |
| `commit_context` | Build a packet focused on a specific git commit |
| `pr_context` | Build a packet covering all commits in a GitHub PR (needs `gh`) |
| `explore_symbol` | Composite: find + detail + related + snippet in one call |
| `edit_context` | Edit-ready context: full verbatim symbol body (never truncated) + sibling/callee/caller contracts + rationale. Use before modifying a symbol — no follow-up file read needed |
| `explore_file` | Composite: outline + related files + key symbols |
| `investigate` | Composite: ask_context + diff awareness |
| `callers` | Call graph: who calls this symbol? |
| `callees` | Call graph: what does this symbol call? |
| `file_history` | Git log for a file: recent commits |
| `symbol_history` | Git blame + commits for a symbol |
| `change_timeline` | How code evolved: introduced, churn, decisions, recent changes |
| `memory_add` | Add a project memory entry (fact/decision/warning/session_summary) |
| `memory_search` | Search project memory by query |
| `memory_list` | List project memory entries with filters |
| `memory_get` | Get a single memory entry by ID |
| `memory_update` | Update content, title, tags, or pinned state |
| `memory_archive` | Archive an entry (preserves for history) |
| `onboard` | Project summary: architecture, patterns, key symbols, naming |
| `affected` | Blast radius: transitive callers/dependents of a symbol (use `staged: true` for current diff) |
| `smart_test` | Minimal test set based on git diff |
| `verify_change` | Composite post-edit check: changed symbols, blast radius, minimal test set, touched public contracts. Call after finishing edits |
| `test_subjects` | Reverse of smart_test: production code exercised by a given test |
| `review` | Code review context: risks, missing co-changes, untested code |
| `coupling` | Module coupling: fan-in, fan-out, co-change frequency |
| `usage_stats` | Token savings, cache hit rate, calls served |

While connected, the MCP server pushes `notifications/resources/updated` (URI `kungfu://index`) whenever the project index changes on disk — agents that subscribe can invalidate cached assumptions instead of acting on stale state.

## Agent rules

Add to `CLAUDE.md` or system prompt. Keep it about *policy*, not tool catalogs — the agent already sees tool descriptions from MCP.

```markdown
## kungfu
- Prefer kungfu MCP tools over reading files directly. Only open a file when kungfu confirms you need it.
- Default to tiny/small budget. Escalate only when the packet is clearly insufficient.
- After creating/editing files, call `reindex` with those paths so the next query sees them.
- After finishing a series of edits, call `verify_change` — it returns the blast radius and the minimal test set to run before declaring the work done.
- Use `memory_search` before implementing — project may already have a decision or warning about this.
- Use `memory_add` to preserve facts, decisions, and warnings worth remembering across sessions. Prefer pinning sparingly.
```

### Auto-reindex on edit (Claude Code hook)

Instead of relying on the agent to remember `reindex`, wire it into the harness — every
`Edit`/`Write` triggers a targeted reindex of exactly that file (~10 ms):

```json
{
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write|MultiEdit|NotebookEdit",
        "hooks": [
          {
            "type": "command",
            "command": "jq -r '.tool_input.file_path // empty' | xargs -I{} kungfu index --only {} >/dev/null 2>&1 || true"
          }
        ]
      }
    ]
  }
}
```

Add to the project's `.claude/settings.json`. The lazy staleness check stays as a safety
net for files changed outside the agent (git pull, formatters, codegen).

Four lines is enough. The agent will pick `ask_context`, `explore_symbol`, `affected`, etc. on its own from tool descriptions.

### Recommended workflow

```
Task received
    │
    ▼
ask_context("task description", budget: "tiny")    ← MCP tool or: kungfu ask-context "..." --budget tiny --json
    │
    ├── Got enough? → Start working
    │
    ├── Need more detail? → escalate to budget: "small"
    │
    ├── Need specific symbol? → find_symbol / get_symbol
    │
    ├── Need file structure? → file_outline
    │
    └── Only then → Read full file
```

### How an agent uses kungfu

Agent receives task: *"fix the authentication bug"*

**Without kungfu** — agent explores blindly:
```
Glob("**/*.ts")           → 200 files, which to read?
Grep("auth")              → 30 matches across many files
Read("src/auth/service.ts")    → 500 lines, 2000 tokens
Read("src/auth/middleware.ts") → 300 lines, 1200 tokens
Read("src/auth/controller.ts") → 400 lines, 1600 tokens
... more files, more tokens
```

**With kungfu** — one MCP call:
```json
→ tools/call: ask_context({"query": "authentication bug fix", "budget": "small"})

← {
    "intent": "debug",
    "items": [
      {
        "name": "AuthController",
        "path": "src/modules/auth/auth.controller.ts",
        "signature": "class AuthController",
        "why": "symbol name match",
        "score": 0.50,
        "snippet": "export class AuthController {\n  constructor(\n    private readonly authService: AuthService,\n  ) {}\n\n  @Post('login')\n  async login(@Body() dto: LoginDto) {\n    return this.authService.login(dto);\n  }\n  ..."
      },
      {
        "name": "AuthService",
        "path": "src/modules/auth/auth.service.ts",
        "signature": "class AuthService",
        "why": "symbol name match",
        "score": 0.50,
        "snippet": "export class AuthService {\n  async login(dto: LoginDto) { ... }\n  async validateToken(token: string) { ... }\n  ..."
      }
    ],
    "rationale": [
      {
        "source": "docs/adr/001-auth-jwt.md",
        "kind": "decision",
        "text": "We chose JWT for stateless session management to avoid server-side session storage",
        "relevance": 0.72
      }
    ],
    "evidence": [
      {
        "source": "docs/adr/001-auth-jwt.md",
        "excerpt": "We chose JWT for stateless session management..."
      }
    ]
  }
```

Agent immediately knows where to look *and why it was designed that way*. Then reads only the relevant file.

### Token savings (open-source projects)

| Project | Lang | Files | Query | kungfu | naive grep+read | Savings |
|---------|------|-------|-------|--------|-----------------|---------|
| [ruff](https://github.com/astral-sh/ruff) | Rust | 9,702 | "how does the linter check rules" | 722 | 137,767 | **190x** |
| [ollama](https://github.com/ollama/ollama) | Go | 1,834 | "how does model loading and inference work" | 1,100 | 110,122 | **100x** |
| [SolidJS](https://github.com/solidjs/solid) | TS | 168 | "how does the reactive signal system work" | 459 | 33,577 | **73x** |
| [leptos](https://github.com/leptos-rs/leptos) | Rust | 1,453 | "how does reactive rendering work" | 593 | 40,772 | **68x** |
| [tRPC](https://github.com/trpc/trpc) | TS | 1,290 | "how does the RPC client call server procedures" | 680 | 43,773 | **64x** |
| [pydantic](https://github.com/pydantic/pydantic) | Python | 729 | "how does field validation work" | 977 | 53,864 | **55x** |
| [FastAPI](https://github.com/fastapi/fastapi) | Python | 2,882 | "how does dependency injection work" | 624 | 17,089 | **27x** |

## How it works

### Indexing
- Scans project files respecting `.gitignore` and configurable ignore rules
- Parses code with [tree-sitter](https://tree-sitter.github.io/) — Rust, TypeScript, JavaScript, Python, Go, Java, C#, Kotlin, C, C++
- Extracts symbols: functions, classes, structs, methods, traits, interfaces, types, constants
- Extracts imports from AST and resolves them to actual files in the project
- Builds relations: `imports`, `test_for`, `config_for`, `calls`
- Extracts function call graph from AST (callers/callees)
- Extracts structured comments (TODO, FIXME, NOTE, doc comments) and populates `doc_summary` on symbols
- Parses markdown documentation and ADR files into searchable memory entries
- Incremental re-indexing via blake3 file fingerprints

### Search & ranking
- Exact match (1.0), prefix (0.9), contains (0.7), stem (0.6), fuzzy (0.4)
- Exact phrase matching: `snake_case`, `camelCase`, space-separated
- Simple English stemming: "ranking" finds "rank", "indexing" finds "index"
- Path matching with filename boost (0.9) vs directory match (0.6)

### Context assembly
- Multi-strategy search: symbols, text, related files, import chains, semantic expansion
- Deduplication by (path, name)
- Changed-file bonus from git
- Test/config proximity bonuses based on query intent
- Language importance weighting (primary language prioritized)
- Budget-controlled output with code snippets

### Memory & rationale layer

Kungfu answers not only *"where is this?"* but also *"why is it like this?"*:

- **Comment extraction** — TODO, FIXME, NOTE, and doc comments are extracted from tree-sitter ASTs during indexing
- **Doc & ADR parsing** — markdown files in `docs/`, `adr/`, `decisions/` are parsed into searchable memory entries; ADR "Decision" sections get higher ranking weight
- **Rationale in context** — `ask_context` returns a `rationale[]` field with matched design decisions, doc excerpts, and annotated comments alongside code items
- **Evidence fragments** — verbatim excerpts from source docs/comments are attached as `evidence[]` for agent trust and traceability
- **Layered context** — use `include: ["code", "rationale", "history"]` to control what layers are returned
- **Change timeline** — `change_timeline` shows how code evolved: when introduced, churn rate, linked decisions, recent changes
- **Doc summaries** — `doc_summary` on symbols is auto-populated from preceding doc comments (first sentence, up to 120 chars)

### Storage
```
.kungfu/
  config.toml          # project configuration
  project.json         # project metadata
  index/
    files.json         # indexed files with hashes
    symbols.json       # extracted symbols with spans
    relations.json     # import/test/config relations
    fingerprints.json  # blake3 hashes for incremental rebuilds
    memories.json      # rationale: comments, doc sections, ADR decisions
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

[git]
enabled = true
```

## Benchmarks

Tested across popular open-source projects (Apple Silicon):

| Project | Language | Files | Symbols | Index | ask-context |
|---------|----------|------:|--------:|------:|------------:|
| express | JS       |   201 |   1,948 |  0.5s |       227ms |
| flask   | Python   |   217 |   1,629 |  0.6s |       228ms |
| gin     | Go       |   118 |   1,487 |  0.7s |       217ms |
| axum    | Rust     |   474 |   2,771 |  1.3s |       269ms |
| cargo   | Rust     | 2,718 |  12,009 | 17.4s |       783ms |
| django  | Python   | 6,907 |  42,917 | 37.9s |     2,125ms |
| ruff    | Rust     | 9,702 |  42,239 | 67.2s |     2,300ms |
| go      | Go       |14,022 | 105,497 |186.8s |     4,661ms |

See [BENCHMARKS.md](BENCHMARKS.md) for full results across all 21 tools.

## License

MIT
