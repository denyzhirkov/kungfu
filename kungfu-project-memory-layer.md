# Kungfu Project Memory Layer

## Overview

This document describes a **project-scoped memory layer** for `kungfu`.

The goal is **not** to build personal memory, identity memory, or a general-purpose note system.
The goal is to extend `kungfu` with a narrow and useful layer of **project continuity** for coding agents.

In practical terms, this means:

- preserving important project facts;
- preserving technical decisions;
- preserving warnings and pitfalls;
- preserving summaries of work sessions;
- preserving explicit relations between project entities;
- injecting this memory into the context packet returned to the agent.

This keeps `kungfu` aligned with its core purpose:

> help coding agents understand a codebase and its surrounding rationale with fewer tokens and less repeated discovery.

---

## Why this feature exists

A coding agent often starts as a blank slate.

Even if the codebase is indexed, the agent still does not automatically know:

- which decisions have already been made;
- which architectural compromises are intentional;
- which modules are transitional or dangerous;
- what was already investigated in previous sessions;
- which project-specific rules should influence future work.

As a result, the agent repeatedly spends time and tokens re-discovering the same things.

A project memory layer solves this by giving `kungfu` a way to preserve and retrieve **small, explicit, high-value project knowledge**.

This memory is not intended to replace code search, symbol analysis, git history, or documentation retrieval.
It is intended to complement them.

---

## Scope

### In scope

The memory layer should store only project-related knowledge such as:

- technical facts about the project;
- architectural decisions;
- conventions and rules;
- warnings and pitfalls;
- explicit relations between entities;
- summaries of previous investigation or implementation sessions.

### Out of scope

The memory layer should **not** store:

- personal identity information about the user;
- global personal preferences unrelated to the current project;
- cross-project life notes;
- random brainstorming not connected to the repository;
- a universal knowledge base "about everything".

If such functionality is ever needed, it should live in a separate product or separate storage boundary.

---

## Product positioning

This feature should be positioned as:

**project memory for coding agents**

Not as:

- personal memory;
- universal memory;
- note-taking app;
- knowledge graph for everything.

A simple positioning statement:

> `kungfu` preserves project-scoped working memory so coding agents do not restart from zero every session.

---

## Design goals

1. **Project-only**
   All memory entries belong to a specific project.

2. **Explicit and explainable**
   The system should always be able to show:
   - where memory came from;
   - when it was created or updated;
   - why it was included in context.

3. **Small and useful**
   The system should store only high-value knowledge.
   Avoid turning every conversation into long-term memory.

4. **Composable with current kungfu architecture**
   Memory should work alongside:
   - code retrieval;
   - symbol graph / symbol index;
   - docs / ADR / rationale extraction;
   - git-based timeline and review features;
   - MCP tools.

5. **Safe against noise**
   The design should minimize:
   - stale facts;
   - duplicated entries;
   - contradictory entries;
   - session spam.

6. **Good for agent bootstrapping**
   The main payoff is better startup context for a task.

---

## Core concept

The memory layer adds one more source of context into `kungfu`.

Today, `kungfu` can already assemble context from sources like:

- code;
- symbols;
- files;
- git changes;
- docs / markdown / ADRs;
- rationale extraction.

With this feature, `kungfu` should also be able to assemble context from:

- project memory entries;
- recent session summaries;
- pinned decisions and warnings;
- explicit project relations.

This turns `kungfu` from only a retrieval engine into a retrieval engine with continuity.

---

## Memory categories

The memory layer should support a very small set of entry types.

### 1. Fact

A project fact is a stable statement about the repository or architecture.

Examples:

- `backend uses deno + sqlite`
- `telegram bot is only an admin interface`
- `payments go through provider adapters`

Use facts for information that should influence future work.

---

### 2. Decision

A decision records an intentional project choice.

Examples:

- `do not introduce repository pattern here`
- `all new validation should use zod`
- `keep telegram handlers thin and move business logic into services`

Use decisions when something was consciously chosen and should be remembered.

---

### 3. Warning

A warning records a pitfall, trap, or transitional condition.

Examples:

- `legacy auth middleware is still used by production webhook routes`
- `this module appears unused but is loaded dynamically`
- `do not change this response shape until mobile client update ships`

Warnings are especially valuable because they prevent harmful "cleanup" by agents.

---

### 4. Session summary

A session summary records what happened in one work session.

It should answer:

- what was investigated;
- what was discovered;
- what changed;
- what remains unresolved;
- what the next likely focus is.

Session summaries help the next agent continue without replaying the entire past.

---

### 5. Relation

A relation is an explicit link between project entities.

Examples:

- `auth-service depends_on token-store`
- `billing feature touches invoice.ts and payment-adapter.ts`
- `decision:auth-migration relates_to ADR-004`

Relations should not try to model the entire universe.
They should be sparse and useful.

---

## Data model

A memory entry can be represented by the following structure.

```json
{
  "id": "mem_0001",
  "project_id": "kungfu",
  "kind": "decision",
  "title": "Use zod for new validation",
  "content": "All new validation logic should use zod-based schemas. Avoid introducing ad-hoc validators in new modules.",
  "tags": ["validation", "zod", "convention"],
  "source": {
    "type": "manual",
    "ref": null
  },
  "related_files": [
    "src/validation/mod.ts"
  ],
  "related_symbols": [
    "ValidationSchema"
  ],
  "confidence": 0.95,
  "pinned": true,
  "supersedes": null,
  "status": "active",
  "created_at": "2026-04-11T10:00:00Z",
  "updated_at": "2026-04-11T10:00:00Z"
}
```

### Required fields

- `id`
- `project_id`
- `kind`
- `content`
- `created_at`
- `updated_at`

### Recommended fields

- `title`
- `tags`
- `source`
- `related_files`
- `related_symbols`
- `confidence`
- `pinned`
- `status`
- `supersedes`

---

## Entry lifecycle

Memory entries should have a simple lifecycle.

### Active
Normal current memory entry.

### Superseded
The entry was replaced by a newer one.

### Archived
The entry is no longer useful in normal retrieval but should remain available for audit/history.

### Deleted
Optional hard delete. This should be rare.

Recommended default behavior:
- prefer `archive` over hard delete;
- prefer `supersede` over silently overwriting history.

---

## Storage options

### Minimal storage for MVP

A simple local storage is enough:

- SQLite table for memory entries
- SQLite table for session metadata
- SQLite table for relations

Optional:
- FTS search over `title` and `content`

This is enough for the first version.

### Optional future additions

- embeddings for semantic search;
- separate lightweight graph table for relations;
- export/import as markdown or JSON;
- `.kungfu/` project-local files synced into the index.

---

## Recommended filesystem layout

One clean option is to keep project memory visible and portable.

```text
.kungfu/
  memory/
    facts/
    decisions/
    warnings/
    sessions/
    relations/
  state/
    sessions/
    cache/
```

Possible file examples:

```text
.kungfu/memory/facts/backend-runtime.md
.kungfu/memory/decisions/use-zod-for-validation.md
.kungfu/memory/warnings/legacy-auth-middleware.md
.kungfu/memory/sessions/2026-04-11-auth-investigation.md
.kungfu/memory/relations/auth-service.depends_on.token-store.json
```

This has advantages:

- human-readable;
- git-friendly if desired;
- easy to inspect and repair;
- easy to index as part of kungfu's existing document/rationale pipeline.

Alternative:
- store canonically in SQLite and optionally mirror/export to markdown.

---

## CLI command design

Below is the recommended CLI surface.

---

## `kungfu memory add`

Adds a new project memory entry.

### Purpose

Used by the user or agent to explicitly save a useful fact, decision, warning, or session-derived note.

### Example

```bash
kungfu memory add "Auth rewrite is still in progress for admin routes only" --kind warning
```

### Extended example

```bash
kungfu memory add \
  "All new validation should use zod v4 schemas" \
  --kind decision \
  --title "Use zod v4 for new validation" \
  --tag validation \
  --tag zod \
  --file src/validation/mod.ts \
  --symbol ValidationSchema \
  --pin
```

### Behavior

- creates a new entry in project memory;
- attaches metadata such as kind, tags, files, symbols;
- can mark the entry as pinned;
- optionally persists both to DB and `.kungfu/memory/`.

### Notes

This should be explicit and predictable.
Do not auto-save too much on the user's behalf.

---

## `kungfu memory list`

Lists memory entries for the current project.

### Example

```bash
kungfu memory list
```

### Filtering examples

```bash
kungfu memory list --kind decision
kungfu memory list --kind warning --tag auth
kungfu memory list --pinned
```

### Behavior

- prints a concise list;
- supports filters by kind, tag, file, symbol, status, pinned;
- useful for manual inspection and cleanup.

---

## `kungfu memory show <id>`

Shows one memory entry in detail.

### Example

```bash
kungfu memory show mem_0042
```

### Behavior

Displays:

- title;
- full content;
- tags;
- source;
- related files;
- related symbols;
- timestamps;
- status;
- any superseded links.

---

## `kungfu memory search <query>`

Searches project memory.

### Example

```bash
kungfu memory search "auth middleware"
```

### Behavior

- searches across memory entries;
- may use keyword or hybrid ranking;
- returns most relevant facts, decisions, warnings, summaries, and relations;
- should explain why results matched.

### Why this matters

Sometimes the right answer is not in code directly.
It may be in a warning or previous summary.

---

## `kungfu memory update <id>`

Updates an existing memory entry.

### Example

```bash
kungfu memory update mem_0042 --content "Auth rewrite is complete for admin routes, but webhook auth still uses legacy middleware."
```

### Behavior

- updates content and metadata;
- refreshes `updated_at`;
- optionally supports `--supersede` mode to create a new entry that replaces the old one.

Recommended approach:
- for major meaning changes, create a new entry and supersede the old one.

---

## `kungfu memory archive <id>`

Archives a memory entry.

### Example

```bash
kungfu memory archive mem_0042
```

### Behavior

- marks entry as archived;
- removes it from normal context assembly unless explicitly requested;
- preserves it for audit/history.

---

## `kungfu memory remove <id>`

Hard deletes a memory entry.

### Example

```bash
kungfu memory remove mem_0042
```

### Behavior

- permanently deletes entry;
- should require explicit confirmation or `--yes`.

Recommended only for obviously bad or accidental entries.

---

## `kungfu memory pin <id>`

Pins a high-value memory entry.

### Example

```bash
kungfu memory pin mem_0012
```

### Behavior

- increases its priority in context assembly;
- useful for core decisions or critical warnings.

---

## `kungfu memory unpin <id>`

Unpins a memory entry.

### Example

```bash
kungfu memory unpin mem_0012
```

---

## Session commands

Sessions are important because they allow continuity without blindly turning every note into long-term memory.

---

## `kungfu session start`

Starts a work session.

### Example

```bash
kungfu session start --title "Investigate auth flow"
```

### Behavior

- creates a session id;
- records start time;
- optionally records current branch, commit, dirty state;
- session id can be referenced by later commands.

### Why this matters

A session gives structure to temporary investigation.

---

## `kungfu session note`

Adds a note to the current session.

### Example

```bash
kungfu session note "Legacy auth middleware is still used by webhook route"
```

### Behavior

- stores a temporary session observation;
- may optionally attach related files and symbols;
- should not automatically become permanent memory.

### Extended example

```bash
kungfu session note \
  "Found duplicated validation path in admin registration flow" \
  --file src/admin/register.ts \
  --symbol registerAdmin
```

---

## `kungfu session list`

Lists recent sessions.

### Example

```bash
kungfu session list
```

### Behavior

- shows recent sessions with title, start time, status, and summary availability.

---

## `kungfu session show <id>`

Displays one session in detail.

### Example

```bash
kungfu session show ses_0018
```

### Behavior

Shows:

- title;
- timestamps;
- notes;
- touched files;
- touched symbols;
- final summary if available.

---

## `kungfu session summarize`

Summarizes a session and optionally promotes findings to memory.

### Example

```bash
kungfu session summarize
```

### Behavior

Creates a compact summary that includes:

- what was investigated;
- what was found;
- what changed;
- unresolved items;
- suggested memory promotions.

### Optional flags

```bash
kungfu session summarize --promote
kungfu session summarize --promote decisions,warnings
```

### Recommended behavior

The command may suggest:

- `promote this as warning`
- `promote this as decision`
- `keep only as session summary`

This is much safer than auto-writing everything into long-term memory.

---

## Relation commands

Relations make project knowledge more explicit.

---

## `kungfu relation add`

Adds an explicit relation between entities.

### Example

```bash
kungfu relation add --from auth-service --to token-store --type depends_on
```

### Example with decision link

```bash
kungfu relation add --from decision:auth-migration --to ADR-004 --type relates_to
```

### Supported relation examples

- `depends_on`
- `implements`
- `touches`
- `relates_to`
- `replaced_by`
- `guarded_by`

### Behavior

- stores a sparse useful relation;
- can later improve ranking and context assembly.

---

## `kungfu relation list`

Lists relations.

### Example

```bash
kungfu relation list
```

---

## `kungfu relation search <query>`

Searches relations around an entity.

### Example

```bash
kungfu relation search "billing"
```

### Behavior

- shows relations connected to the query entity;
- helps find relevant connected context.

---

## Main context command

This is the most important outcome of the entire feature.

---

## `kungfu context <task>`

Builds a context packet for a task.

### Example

```bash
kungfu context "Add admin authentication for mini app"
```

### Expected behavior

This command should assemble relevant context from multiple sources:

1. relevant code and symbols;
2. relevant docs / ADR / rationale;
3. relevant git history if needed;
4. pinned project memory;
5. recent session summaries related to the task;
6. warnings connected to touched areas;
7. explicit relations connected to the detected entities.

### Output shape

The command should return something like:

- task summary
- relevant files
- relevant symbols
- rationale snippets
- project memory snippets
- active warnings
- recent session continuity
- assembled context pack

### Why this is the core command

The point of memory is not storage.
The point of memory is better context assembly.

If `memory add` exists but `context` does not use memory effectively, the feature has failed.

---

## How memory should influence ranking

Memory should be a ranking signal, not a blunt override.

Possible ranking boosts:

- pinned entries;
- recent session relevance;
- file overlap with retrieved code results;
- symbol overlap with current task;
- tag overlap with task language;
- warning entries near changed or affected files.

Possible ranking penalties:

- archived entries;
- superseded entries;
- low-confidence entries.

This allows memory to improve relevance without dominating retrieval.

---

## Suggested context assembly strategy

When `kungfu context <task>` runs:

### Step 1. Retrieve normal code context
Use current retrieval pipeline to find:
- files;
- symbols;
- docs;
- rationale;
- relevant history.

### Step 2. Detect candidate project entities
Infer candidate terms such as:
- module names;
- features;
- file paths;
- symbol names;
- tags.

### Step 3. Query project memory
Search memory for:
- matching tags;
- file overlap;
- symbol overlap;
- text relevance;
- active pinned decisions and warnings.

### Step 4. Query recent sessions
Search recent summaries for:
- matching files or symbols;
- matching investigation topics;
- unresolved items.

### Step 5. Merge and deduplicate
Assemble a compact packet:
- top code evidence;
- top rationale evidence;
- top memory evidence;
- top continuity evidence.

### Step 6. Budget for tokens
Do not dump everything.
Prefer:
- short snippets;
- concise summaries;
- expandable references.

---

## Recommended MCP surface

If memory is exposed over MCP, keep the tool surface small.

### Suggested tools

- `memory_add`
- `memory_search`
- `memory_get`
- `memory_list`
- `session_start`
- `session_note`
- `session_summarize`
- `context_for_task`

### Most important MCP tool

`context_for_task`

Example conceptual input:

```json
{
  "task": "Add admin authentication for mini app",
  "include": ["code", "rationale", "memory", "sessions"],
  "max_tokens": 4000
}
```

Example conceptual output:

```json
{
  "task": "Add admin authentication for mini app",
  "files": [...],
  "symbols": [...],
  "memory": [...],
  "session_continuity": [...],
  "warnings": [...],
  "assembled_context": "..."
}
```

This keeps the memory feature aligned with agent usage.

---

## Recommended implementation order

The safest rollout is incremental.

### Phase 1 — Minimal project memory
Implement:

- `memory add`
- `memory list`
- `memory show`
- `memory search`
- `memory pin`
- `memory archive`

At this stage, the user can store and inspect explicit project knowledge.

### Phase 2 — Session continuity
Implement:

- `session start`
- `session note`
- `session list`
- `session show`
- `session summarize`

At this stage, the system can preserve recent work continuity.

### Phase 3 — Memory-aware context assembly
Extend:

- `kungfu context`

So it uses:
- pinned memory;
- warnings;
- recent relevant summaries;
- entity relations.

This is the first phase where users feel the actual benefit.

### Phase 4 — Relations
Implement:

- `relation add`
- `relation list`
- `relation search`

This improves connected retrieval.

### Phase 5 — Quality improvements
Possible future upgrades:

- supersede workflow;
- semantic search;
- trust/confidence modeling;
- memory compaction;
- stale entry detection;
- import/export of memory markdown.

---

## Recommended defaults

To keep the feature disciplined:

1. **No aggressive auto-save**
   Prefer explicit commands or post-session promotion.

2. **Archive instead of delete**
   Preserve explainability.

3. **Pin only a small number of entries**
   Keep context clean.

4. **Session notes are temporary by default**
   Permanent memory requires promotion or summary.

5. **Context assembly must respect a budget**
   Memory is useful only if concise.

---

## What not to do

These are important guardrails.

### Do not store everything
A memory layer full of noise is worse than no memory.

### Do not blur project memory with personal memory
That would confuse the product and complicate privacy boundaries.

### Do not auto-promote every session note
Temporary investigation is not the same as durable project truth.

### Do not let memory override code reality
Memory should help interpretation, not replace evidence.

### Do not build a massive graph too early
Sparse useful relations are better than a noisy pseudo-knowledge-graph.

### Do not overcomplicate the first version
The simplest useful feature is:
- a few explicit memory types;
- session summaries;
- context integration.

---

## Example user workflows

## Workflow 1: Save a decision

```bash
kungfu memory add \
  "Keep Telegram bot handlers thin; business logic belongs in services" \
  --kind decision \
  --tag architecture \
  --tag telegram \
  --pin
```

Later, when the agent is asked to modify bot logic, this decision can be injected into context.

---

## Workflow 2: Save a warning during investigation

```bash
kungfu session start --title "Investigate webhook auth"
kungfu session note "Webhook routes still use legacy auth middleware"
kungfu session summarize --promote warnings
```

This creates continuity and preserves a useful warning.

---

## Workflow 3: Build task context

```bash
kungfu context "Refactor admin registration validation"
```

Expected result:
- validation-related code files;
- relevant schemas and symbols;
- rationale from docs/ADR;
- memory entry that says "all new validation should use zod";
- warning if registration still uses legacy flow;
- recent summary from prior investigation.

---

## Workflow 4: Update stale memory

```bash
kungfu memory search "auth rewrite"
kungfu memory update mem_0042 --content "Auth rewrite complete for admin routes; webhook auth still legacy"
```

Or better:

```bash
kungfu memory update mem_0042 --status superseded
kungfu memory add "Auth rewrite complete for admin routes; webhook auth still legacy" --kind warning
```

---

## Suggested internal modules

A possible internal design:

- `memory/store`
  - CRUD for memory entries
- `memory/search`
  - search and ranking
- `memory/context`
  - merge memory into task context
- `session/store`
  - session lifecycle and notes
- `session/summarize`
  - build summary and promotion suggestions
- `relations/store`
  - sparse relation storage
- `context/assembler`
  - final context packet assembly

This should stay modular and optional.

---

## Suggested schema sketch

### `memory_entries`

- `id`
- `project_id`
- `kind`
- `title`
- `content`
- `status`
- `pinned`
- `confidence`
- `source_type`
- `source_ref`
- `created_at`
- `updated_at`
- `supersedes`

### `memory_tags`

- `entry_id`
- `tag`

### `memory_files`

- `entry_id`
- `file_path`

### `memory_symbols`

- `entry_id`
- `symbol_name`

### `sessions`

- `id`
- `project_id`
- `title`
- `status`
- `started_at`
- `ended_at`
- `summary`

### `session_notes`

- `id`
- `session_id`
- `content`
- `created_at`

### `relations`

- `id`
- `project_id`
- `from_entity`
- `to_entity`
- `type`
- `created_at`

---

## Success criteria

This feature is successful if it produces the following effects:

1. Agents repeat less discovery work.
2. Important project decisions are injected into context when relevant.
3. Dangerous areas are easier to detect because warnings are surfaced.
4. Follow-up sessions can continue from recent summaries.
5. The feature remains narrow and does not turn `kungfu` into a generic note app.

---

## Minimal MVP summary

If implementation time is limited, the first useful MVP should include only:

- `kungfu memory add`
- `kungfu memory list`
- `kungfu memory show`
- `kungfu memory search`
- `kungfu memory pin`
- `kungfu session start`
- `kungfu session note`
- `kungfu session summarize`
- memory-aware `kungfu context`

That is enough to eliminate much of the "blank slate" problem.

---

## Final summary

The proposed feature is:

> a project-scoped memory layer for `kungfu` that stores high-value project facts, decisions, warnings, session summaries, and sparse relations, then injects them into task context for coding agents.

This should remain:

- narrow;
- explicit;
- explainable;
- project-only;
- retrieval-oriented.

If built with these constraints, it is not feature bloat.
It is a strong extension of `kungfu`'s existing role as a context engine for coding agents.
