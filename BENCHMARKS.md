# Kungfu Benchmarks

Measured with **kungfu v2.6.0** across 10 popular open-source projects on Apple Silicon (M-series).

Methodology: release binary, full reindex (`kungfu index --full`), then each tool timed as a
standalone CLI invocation (cold process, warm OS file cache — one warm-up call per repo). A
long-lived MCP server is faster still: the process-scope index cache serves repeat calls from
memory (single-digit milliseconds on a synthetic 100k-symbol / 1M-edge index).

## Indexing

| Project | Language | Files | Symbols | Index v2.5 | Index v2.6 |
|---------|----------|------:|--------:|-----------:|-----------:|
| flask   | Python   |   217 |   1,629 |       0.6s |   **0.2s** |
| gin     | Go       |   118 |   1,487 |       0.7s |   **0.2s** |
| express | JS       |   201 |   1,948 |       0.5s |   **0.2s** |
| axum    | Rust     |   474 |   3,137 |       1.3s |   **0.3s** |
| cargo   | Rust     | 2,718 |  12,422 |      17.4s |   **2.6s** |
| react   | JS/TS    | 6,736 |  58,828 |      23.7s |   **7.2s** |
| django  | Python   | 6,907 |  42,917 |      37.9s |   **4.9s** |
| ruff    | Rust     | 9,702 |  46,195 |      67.2s |   **5.7s** |
| go      | Go       |13,869 | 105,485 |     186.8s |  **16.9s** |
| next.js | JS/TS    |25,110 |  92,072 |     146.2s |  **24.3s** |

The v2.6 speedup comes from call-graph noise filtering at relation-build time: cross-file-only
edges, a per-language stop-list of ubiquitous callables (~460 names), and a frequency cutoff
(callees invoked from >25 distinct files are dropped as utility-noise). The index stores an
order of magnitude fewer, dramatically more useful call edges — go: ~1M raw call edges before
filtering → 51k stored; home-assistant: 440k → 24k.

## Tool response times

| Project | explore-symbol | ask-context | semantic-search | callers v2.5 | callers v2.6 |
|---------|---------------:|------------:|----------------:|-------------:|-------------:|
| flask   |           15ms |        70ms |           365ms |        150ms |     **12ms** |
| gin     |           12ms |       174ms |            88ms |        154ms |     **11ms** |
| express |           14ms |        66ms |            97ms |        134ms |     **11ms** |
| axum    |           16ms |        92ms |           117ms |        292ms |     **12ms** |
| cargo   |           36ms |       287ms |           105ms |      2,539ms |     **23ms** |
| react   |          127ms |       351ms |           158ms |            — |     **59ms** |
| django  |          100ms |       613ms |           429ms |      4,074ms |     **53ms** |
| ruff    |          177ms |       618ms |           212ms |     10,202ms |     **55ms** |
| go      |          189ms |     1,461ms |           247ms |     25,399ms |    **113ms** |
| next.js |          182ms |     1,535ms |           348ms |            — |     **93ms** |

`callers` on go: **25.4s → 0.11s (~230×)** — the combination of the smaller filtered graph, a
HashMap symbol lookup instead of a linear scan, and the process-scope shard cache. Semantic
search here runs in keyword-expansion mode (no embeddings built for the bench repos); with
built embeddings it runs true vector search — see `kungfu embeddings status`.

## Retrieval quality

Quality is tracked by a 56-case suite against pinned checkouts of these repos
(`research/cases/`, commits pinned in `research/bench-repos.lock.json`; run it with
`scripts/eval.sh && scripts/score.sh`). Categories cover concept-recall, identifier
mismatch, over-selection, cross-file context, debugging, orientation, and — new in
v2.6.1 — concept-to-file (file-level purpose vectors in `semantic_search`) and
onboarding (glossary + annotated entrypoints in `onboard`). The current recorded
baseline is `research/baseline-score-v4.json` (v2.6.0, cases 001–050: aggregate 53.03,
37/50 passing — up from 46.41 / 32 before the v2.6.0 retrieval work; vector-layer cases
require `kungfu embeddings build` in the target repos first). v2.6.1 verified parity on
baseline cases over unchanged corpora (bit-identical scores on 7 repos with rebuilt
stores) and passes all six new-surface cases; a full baseline v5 refresh awaits complete
embedding stores. Self-targeting cases drift as this codebase evolves — A/B comparisons
always re-run the BEFORE side at HEAD.

## What's measured elsewhere

- MCP tool list and descriptions: see the [README](README.md#tools-38).
- Precision smoke-suite on kungfu itself: `bench/bench.sh`.
- Token-savings accounting: `kungfu stats` (honest baseline — on-disk bytes of the files a
  result references vs bytes actually returned).

## Key characteristics

- **Process-scope index cache** — parsed shards cached per process, stamp-invalidated on any
  write; a long-lived MCP server never re-parses an unchanged index.
- **Call-graph filtering** — cross-file-only, per-language stop-list, frequency cutoff; empty
  results declare *why* (`provenance`, distinct statuses) instead of returning a silent `[]`.
- **Adaptive budget** — packet size auto-resolves from project size; every path trims to fit.
- **Query expansion** — identifier-aware matching (camelCase/snake_case splitting), per-query
  IDF token weighting, ~80 synonym groups; exact matches always outrank expanded ones.
- **Diversity-aware packets** — marginal-relevance selection keeps one file from crowding out
  the rest of the answer.
- **Two-mode semantic search** — keyword expansion with zero setup, or local vector search
  (bge-small, compiled in) once `kungfu embeddings build` has run.
- **Diff awareness** — git-changed files are boosted in results.
- **Call graph** — AST-based call extraction for all 10 supported languages.
