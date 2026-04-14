use kungfu_types::memory::{MemoryEntry, MemoryKind};

use crate::anchor::extract_anchors;

/// Parse a markdown document into MemoryEntry items.
///
/// Splits on headers, detects ADR patterns, extracts anchors.
pub fn parse_doc(path: &str, content: &str) -> Vec<MemoryEntry> {
    let is_adr = is_adr_file(path, content);
    let sections = split_sections(content);

    let mut memories = Vec::new();
    for (i, section) in sections.iter().enumerate() {
        let text = section.body.trim();
        if text.is_empty() || text.len() < 10 {
            continue;
        }

        let kind = if is_adr && is_decision_section(&section.header) {
            MemoryKind::Decision
        } else {
            MemoryKind::DocSection
        };

        let weight = match kind {
            MemoryKind::Decision => 0.9,
            _ => 0.5,
        };

        let mut anchors = extract_anchors(text);
        // Also extract anchors from the header
        let header_anchors = extract_anchors(&section.header);
        anchors.extend(header_anchors);
        anchors.sort();
        anchors.dedup();

        let id = format!("mem:{}:{}", path.replace('/', ":"), i);

        // Truncate text to keep memory entries compact (char-based to handle multibyte)
        let truncated = if text.chars().count() > 500 {
            let head: String = text.chars().take(500).collect();
            format!("{}...", head)
        } else {
            text.to_string()
        };

        memories.push(MemoryEntry {
            id,
            path: path.to_string(),
            kind,
            text: truncated,
            anchors,
            weight,
            symbol_id: None,
            line_range: Some((section.start_line, section.end_line)),
        });
    }

    memories
}

struct Section {
    header: String,
    body: String,
    start_line: usize,
    end_line: usize,
}

fn split_sections(content: &str) -> Vec<Section> {
    let mut sections = Vec::new();
    let mut current_header = String::new();
    let mut current_body = String::new();
    let mut start_line = 1;
    for (i, line) in content.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();

        if trimmed.starts_with('#') {
            // Save previous section
            if !current_body.trim().is_empty() || !current_header.is_empty() {
                sections.push(Section {
                    header: current_header.clone(),
                    body: current_body.trim().to_string(),
                    start_line,
                    end_line: line_num.saturating_sub(1),
                });
            }
            current_header = trimmed.trim_start_matches('#').trim().to_string();
            current_body = String::new();
            start_line = line_num;
        } else {
            if !trimmed.is_empty() {
                if !current_body.is_empty() {
                    current_body.push(' ');
                }
                current_body.push_str(trimmed);
            }
        }
    }

    // Last section
    let total_lines = content.lines().count();
    if !current_body.trim().is_empty() || !current_header.is_empty() {
        sections.push(Section {
            header: current_header,
            body: current_body.trim().to_string(),
            start_line,
            end_line: total_lines,
        });
    }

    sections
}

fn is_adr_file(path: &str, content: &str) -> bool {
    let lower_path = path.to_lowercase();
    if lower_path.contains("adr/")
        || lower_path.contains("decisions/")
        || lower_path.contains("decision-records/")
    {
        return true;
    }

    // Heuristic: check if content has ADR-like sections
    let lower_content = content.to_lowercase();
    let adr_markers = ["## status", "## context", "## decision", "## consequences"];
    let matches = adr_markers.iter().filter(|m| lower_content.contains(*m)).count();
    matches >= 2
}

fn is_decision_section(header: &str) -> bool {
    let lower = header.to_lowercase();
    lower == "decision"
        || lower == "decisions"
        || lower.starts_with("decision:")
        || lower == "chosen alternative"
        || lower == "outcome"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_doc() {
        let content = "# Overview\n\nThis module handles authentication flows.\n\n# Implementation\n\nUses JWT tokens for session management.";
        let memories = parse_doc("docs/auth.md", content);
        assert_eq!(memories.len(), 2);
        assert_eq!(memories[0].kind, MemoryKind::DocSection);
        assert!(memories[0].text.contains("authentication"));
        assert!(memories[1].text.contains("JWT"));
    }

    #[test]
    fn detects_adr_by_path() {
        let content = "# Use JWT for Auth\n\n## Status\n\nAccepted\n\n## Decision\n\nWe will use JWT tokens because they are stateless.\n\n## Consequences\n\nNeed token refresh logic.";
        let memories = parse_doc("docs/adr/001-jwt-auth.md", content);
        let decision = memories.iter().find(|m| m.kind == MemoryKind::Decision);
        assert!(decision.is_some(), "Should detect Decision section");
        assert!(decision.unwrap().text.contains("JWT"));
    }

    #[test]
    fn detects_adr_by_content() {
        let content = "# Use Event Sourcing\n\n## Status\n\nProposed\n\n## Context\n\nWe need audit trails.\n\n## Decision\n\nAdopt event sourcing for order service.";
        let memories = parse_doc("docs/architecture.md", content);
        let decision = memories.iter().find(|m| m.kind == MemoryKind::Decision);
        assert!(decision.is_some());
    }

    #[test]
    fn skips_short_sections() {
        let content = "# Title\n\nOk\n\n# Details\n\nThis section has enough content to be indexed properly.";
        let memories = parse_doc("docs/test.md", content);
        // "Ok" is too short (< 10 chars), should be skipped
        assert_eq!(memories.len(), 1);
    }

    #[test]
    fn empty_doc() {
        let memories = parse_doc("docs/empty.md", "");
        assert!(memories.is_empty());
    }

    #[test]
    fn high_weight_for_decisions() {
        let content = "## Decision\n\nWe chose Rust for performance and safety guarantees.";
        let memories = parse_doc("docs/adr/002.md", content);
        assert!(!memories.is_empty());
        assert_eq!(memories[0].weight, 0.9);
    }

    #[test]
    fn truncates_multibyte_content_without_panic() {
        // Regression: byte-based slicing panicked on Cyrillic content >500 chars
        let long_cyrillic = "# Секция\n\n".to_string() + &"Это длинный текст на русском языке. ".repeat(30);
        let memories = parse_doc("docs/notes.md", &long_cyrillic);
        assert!(!memories.is_empty());
        // Must not panic and must produce a valid utf-8 string
        assert!(memories[0].text.chars().count() <= 503); // 500 + "..."
    }
}
