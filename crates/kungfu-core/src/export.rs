use crate::KungfuService;
use anyhow::{Context, Result};
use std::io::Write;

impl KungfuService {
    /// Dump the project index (files, symbols, relations) and active project memory as
    /// newline-delimited JSON. Each line carries a `"type"` discriminator:
    /// `file` | `symbol` | `relation` | `memory`.
    ///
    /// Designed for external consumption (graph viewers, custom agents, analytics) without
    /// going through the MCP transport.
    pub fn export_jsonl(&self, w: &mut impl Write) -> Result<ExportStats> {
        self.ensure_fresh_index()?;
        let store = self.store();
        let mut stats = ExportStats::default();

        for f in store.load_files()? {
            let v = serde_json::json!({
                "type": "file",
                "id": f.id,
                "path": f.path,
                "language": f.language,
                "size": f.size,
                "hash": f.hash,
            });
            writeln!(w, "{}", serde_json::to_string(&v)?).context("write file line")?;
            stats.files += 1;
        }

        for s in store.load_symbols()? {
            let v = serde_json::json!({
                "type": "symbol",
                "id": s.id,
                "file_id": s.file_id,
                "name": s.name,
                "kind": s.kind.to_string(),
                "language": s.language,
                "path": s.path,
                "signature": s.signature,
                "start_line": s.span.start_line,
                "end_line": s.span.end_line,
                "exported": s.exported,
                "doc_summary": s.doc_summary,
            });
            writeln!(w, "{}", serde_json::to_string(&v)?).context("write symbol line")?;
            stats.symbols += 1;
        }

        for r in store.load_relations()? {
            let v = serde_json::json!({
                "type": "relation",
                "kind": format!("{:?}", r.kind),
                "source_id": r.source_id,
                "target_id": r.target_id,
                "weight": r.weight,
            });
            writeln!(w, "{}", serde_json::to_string(&v)?).context("write relation line")?;
            stats.relations += 1;
        }

        for m in store.load_project_memories().unwrap_or_default() {
            if m.status != kungfu_types::memory::MemoryStatus::Active {
                continue;
            }
            let v = serde_json::json!({
                "type": "memory",
                "id": m.id,
                "kind": m.kind.to_string(),
                "title": m.title,
                "content": m.content,
                "tags": m.tags,
                "related_files": m.related_files,
                "related_symbols": m.related_symbols,
                "pinned": m.pinned,
                "created_at": m.created_at,
                "updated_at": m.updated_at,
            });
            writeln!(w, "{}", serde_json::to_string(&v)?).context("write memory line")?;
            stats.memories += 1;
        }

        w.flush().context("flush export stream")?;
        Ok(stats)
    }
}

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ExportStats {
    pub files: usize,
    pub symbols: usize,
    pub relations: usize,
    pub memories: usize,
}
