use crate::KungfuService;
use anyhow::Result;
use kungfu_rank::{build_context_packet_full, ScoredSymbol};
use kungfu_types::budget::Budget;
use kungfu_types::context::{ContextPacket, Intent};
use kungfu_types::symbol::Symbol;
use serde::Serialize;
use std::collections::HashSet;

#[derive(Debug, Clone, Serialize)]
pub struct TraceFrame {
    /// Path as written in the trace (may be absolute or repo-relative).
    pub raw_path: String,
    /// Path resolved against the index (if matched).
    pub resolved_path: Option<String>,
    pub line: usize,
    /// Containing symbol if any.
    pub symbol: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DebugTraceResult {
    pub frames: Vec<TraceFrame>,
    pub packet: ContextPacket,
}

impl KungfuService {
    /// Build a context packet from a stack trace / panic / traceback.
    /// Recognised formats: Rust panic + backtrace, JS stack, Python traceback, Go panic.
    pub fn debug_trace(&self, trace: &str, budget: Budget) -> Result<DebugTraceResult> {
        self.ensure_fresh_index()?;
        let budget = self.resolve_budget(budget);
        let raw_frames = parse_frames(trace);

        let store = self.store();
        let files = store.load_files()?;
        let symbols = store.load_symbols()?;

        // Resolve each raw frame against the index.
        let mut frames: Vec<TraceFrame> = Vec::with_capacity(raw_frames.len());
        let mut seed: Vec<ScoredSymbol> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();
        let mut seed_file_ids: HashSet<String> = HashSet::new();

        for (idx, (raw_path, line)) in raw_frames.iter().enumerate() {
            // Match file by suffix (longest match wins).
            let matched = files
                .iter()
                .filter(|f| raw_path.ends_with(&f.path) || f.path.ends_with(raw_path))
                .max_by_key(|f| f.path.len());

            let (resolved_path, symbol_name) = if let Some(f) = matched {
                // Find innermost symbol covering this line.
                let containing = symbols
                    .iter()
                    .filter(|s| s.file_id == f.id)
                    .filter(|s| s.span.start_line <= *line && s.span.end_line >= *line)
                    .min_by_key(|s| s.span.end_line - s.span.start_line);

                if let Some(sym) = containing {
                    if seen_ids.insert(sym.id.clone()) {
                        // Decay score by frame index — top of trace usually most relevant.
                        let score = 1.0 - (idx as f64 * 0.05).min(0.5);
                        seed.push(ScoredSymbol {
                            symbol: sym.clone(),
                            score,
                            reason: format!("trace frame {} ({}:{})", idx, f.path, line),
                        });
                    }
                    seed_file_ids.insert(f.id.clone());
                    (Some(f.path.clone()), Some(sym.name.clone()))
                } else {
                    seed_file_ids.insert(f.id.clone());
                    (Some(f.path.clone()), None)
                }
            } else {
                (None, None)
            };

            frames.push(TraceFrame {
                raw_path: raw_path.clone(),
                resolved_path,
                line: *line,
                symbol: symbol_name,
            });
        }

        // Expand: add siblings from each seed file (small bonus), capped per file.
        let top_k = budget.top_k();
        if !seed.is_empty() && seed.len() < top_k {
            let other_syms: Vec<&Symbol> = symbols
                .iter()
                .filter(|s| seed_file_ids.contains(&s.file_id) && !seen_ids.contains(&s.id))
                .collect();
            for sym in other_syms
                .into_iter()
                .take(top_k.saturating_sub(seed.len()))
            {
                seen_ids.insert(sym.id.clone());
                seed.push(ScoredSymbol {
                    symbol: sym.clone(),
                    score: 0.4,
                    reason: "same file as trace frame".to_string(),
                });
            }
        }

        let query = format!("debug_trace: {} frames", frames.len());
        let packet = build_context_packet_full(&query, seed, budget, Some(Intent::Debug));

        Ok(DebugTraceResult { frames, packet })
    }
}

/// Parse code locations from stack-trace-like text.
///
/// Returns `(path, line)` pairs preserving order of first appearance and deduplicating.
/// Recognised formats:
/// - Python:  `File "src/foo.py", line 42`
/// - Generic: any `<path>.<ext>:<line>[:<col>]` token (Rust, JS, Go, Java, ...)
pub(crate) fn parse_frames(trace: &str) -> Vec<(String, usize)> {
    const EXTS: &[&str] = &[
        ".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs", ".go", ".java", ".kt", ".cs",
        ".cpp", ".cc", ".c", ".h", ".hpp", ".rb", ".php", ".swift", ".scala",
    ];
    let mut frames: Vec<(String, usize)> = Vec::new();
    let mut seen: HashSet<(String, usize)> = HashSet::new();

    for line in trace.lines() {
        // Python `File "<path>", line <N>`
        if let Some(rest) = line.find("File \"").map(|i| &line[i + 6..]) {
            if let Some(end) = rest.find('"') {
                let path = &rest[..end];
                let tail = &rest[end + 1..];
                if let Some(line_idx) = tail.find("line ") {
                    let num: String = tail[line_idx + 5..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(n) = num.parse::<usize>() {
                        let key = (path.to_string(), n);
                        if seen.insert(key.clone()) {
                            frames.push(key);
                        }
                        continue;
                    }
                }
            }
        }

        // Generic `<path>.<ext>:<line>` scan.
        for ext in EXTS {
            let mut search_from = 0;
            while let Some(local) = line[search_from..].find(ext) {
                let ext_start = search_from + local;
                let ext_end = ext_start + ext.len();
                let after = &line[ext_end..];

                // Must be followed by ':' then a digit (else it's mid-identifier or filename in prose).
                let mut chars = after.chars();
                if chars.next() != Some(':') {
                    search_from = ext_end;
                    continue;
                }
                let num: String = chars.take_while(|c| c.is_ascii_digit()).collect();
                if num.is_empty() {
                    search_from = ext_end + 1;
                    continue;
                }
                let n: usize = match num.parse() {
                    Ok(n) if n > 0 => n,
                    _ => {
                        search_from = ext_end + 1 + num.len();
                        continue;
                    }
                };

                // Walk back from ext_start to find the path token boundary.
                let prefix = &line[..ext_start];
                let path_start = prefix
                    .rfind(|c: char| {
                        c.is_whitespace() || c == '(' || c == '"' || c == '\'' || c == '<'
                    })
                    .map(|i| i + 1)
                    .unwrap_or(0);
                let path = line[path_start..ext_end].to_string();

                // Filter noise: must look like a path (contain / or \) OR have no spaces.
                if !path.contains(' ') {
                    let key = (path, n);
                    if seen.insert(key.clone()) {
                        frames.push(key);
                    }
                }

                search_from = ext_end + 1 + num.len();
            }
        }
    }

    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_rust_panic_and_backtrace() {
        let trace = r#"
thread 'main' panicked at src/foo.rs:42:5:
unwrap on None
note: run with `RUST_BACKTRACE=1`
   0: foo::bar
             at src/foo.rs:42
   1: main::main
             at src/main.rs:10
"#;
        let frames = parse_frames(trace);
        assert!(frames.contains(&("src/foo.rs".to_string(), 42)));
        assert!(frames.contains(&("src/main.rs".to_string(), 10)));
    }

    #[test]
    fn parse_js_stack() {
        let trace = r#"
TypeError: Cannot read property 'x' of undefined
    at Object.handler (/app/src/server.js:23:15)
    at processTicksAndRejections (node:internal/process/task_queues:96:5)
"#;
        let frames = parse_frames(trace);
        assert!(frames
            .iter()
            .any(|(p, n)| p.ends_with("server.js") && *n == 23));
    }

    #[test]
    fn parse_python_traceback() {
        let trace = r#"
Traceback (most recent call last):
  File "src/api.py", line 17, in handle
    do_thing()
  File "src/util.py", line 4, in do_thing
    raise ValueError("nope")
"#;
        let frames = parse_frames(trace);
        assert!(frames.contains(&("src/api.py".to_string(), 17)));
        assert!(frames.contains(&("src/util.py".to_string(), 4)));
    }

    #[test]
    fn parse_go_panic() {
        let trace = r#"
panic: runtime error: index out of range
goroutine 1 [running]:
main.bar()
    /home/u/p/main.go:42 +0x123
main.main()
    /home/u/p/main.go:10 +0x456
"#;
        let frames = parse_frames(trace);
        assert!(frames
            .iter()
            .any(|(p, n)| p.ends_with("main.go") && *n == 42));
        assert!(frames
            .iter()
            .any(|(p, n)| p.ends_with("main.go") && *n == 10));
    }

    #[test]
    fn dedup_repeated_frames() {
        let trace = "at src/x.rs:5\nat src/x.rs:5\n";
        let frames = parse_frames(trace);
        assert_eq!(frames.len(), 1);
    }

    #[test]
    fn ignores_prose_without_line_number() {
        let trace = "I changed file.rs and broke it.\n";
        let frames = parse_frames(trace);
        assert!(frames.is_empty());
    }
}
