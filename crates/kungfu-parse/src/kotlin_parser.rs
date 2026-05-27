use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "import" {
            // Extract qualified_identifier from import node
            let text = node_text(child, source);
            let path = text.trim_start_matches("import ").trim().to_string();
            if !path.is_empty() {
                let names = if path.ends_with('*') {
                    Vec::new()
                } else {
                    path.rsplit('.')
                        .next()
                        .map(|n| vec![n.to_string()])
                        .unwrap_or_default()
                };
                imports.push(RawImport {
                    path,
                    names,
                    line: child.start_position().row + 1,
                });
            }
        }
    }

    imports
}

pub fn extract(root: Node, source: &str, file_id: &str, file_path: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    collect_symbols(&root, source, file_id, file_path, None, &mut symbols);
    symbols
}

fn collect_symbols(
    node: &Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "class_declaration" => {
                extract_type(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Class,
                    symbols,
                );
            }
            "object_declaration" => {
                extract_type(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Class,
                    symbols,
                );
            }
            "interface_declaration" => {
                extract_type(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Interface,
                    symbols,
                );
            }
            "function_declaration" => {
                extract_function(child, source, file_id, file_path, parent_id, symbols);
            }
            "property_declaration" => {
                extract_property(child, source, file_id, file_path, parent_id, symbols);
            }
            // Recurse into class bodies, companion objects, etc.
            "class_body" => {
                collect_symbols(&child, source, file_id, file_path, parent_id, symbols);
            }
            _ => {}
        }
    }
}

fn extract_type(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
) {
    let name_str = match find_identifier(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
    let visibility = detect_visibility(&node, source);
    let exported = !matches!(visibility.as_deref(), Some("private") | Some("internal"));

    // Check if it's an enum class or data class
    let actual_kind = {
        let text = node_text(node, source);
        let first_line = text.lines().next().unwrap_or("");
        if first_line.contains("enum class") || first_line.contains("enum ") {
            SymbolKind::Enum
        } else if first_line.contains("data class") {
            SymbolKind::Struct
        } else if first_line.contains("interface ") {
            SymbolKind::Interface
        } else {
            kind
        }
    };

    let sig = {
        let text = node_text(node, source);
        text.lines().next().unwrap_or("").trim().to_string()
    };

    symbols.push(Symbol {
        id: id.clone(),
        file_id: file_id.to_string(),
        name: name_str,
        kind: actual_kind,
        language: "kotlin".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported,
        visibility,
        doc_summary: None,
    });

    // Recurse into body
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "class_body" || child.kind() == "enum_class_body" {
            collect_symbols(&child, source, file_id, file_path, Some(&id), symbols);
        }
    }
}

fn extract_function(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name_str = match find_identifier(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
    let visibility = detect_visibility(&node, source);
    let exported = !matches!(visibility.as_deref(), Some("private") | Some("internal"));

    let kind = if parent_id.is_some() {
        SymbolKind::Method
    } else {
        SymbolKind::Function
    };

    let sig = extract_sig_before_body(&node, source);

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name: name_str,
        kind,
        language: "kotlin".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported,
        visibility,
        doc_summary: None,
    });
}

fn extract_property(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name_str = match find_identifier(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
    let visibility = detect_visibility(&node, source);
    let exported = !matches!(visibility.as_deref(), Some("private") | Some("internal"));

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name: name_str,
        kind: SymbolKind::Variable,
        language: "kotlin".to_string(),
        path: file_path.to_string(),
        signature: Some(
            node_text(node, source)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string(),
        ),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported,
        visibility,
        doc_summary: None,
    });
}

fn find_identifier(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            || child.kind() == "type_identifier"
            || child.kind() == "simple_identifier"
        {
            let text = node_text(child, source);
            if !text.is_empty() && text.chars().next().is_some_and(|c| c.is_alphabetic()) {
                return Some(text);
            }
        }
    }
    None
}

fn extract_sig_before_body(node: &Node, source: &str) -> String {
    let start = node.start_byte();
    // Look for function_body
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_body" {
            let end = child.start_byte();
            return source[start..end].trim().to_string();
        }
    }
    let text = node_text(*node, source);
    text.lines().next().unwrap_or("").trim().to_string()
}

fn detect_visibility(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" || child.kind() == "visibility_modifier" {
            let text = node_text(child, source);
            if text.contains("public") {
                return Some("public".to_string());
            } else if text.contains("private") {
                return Some("private".to_string());
            } else if text.contains("protected") {
                return Some("protected".to_string());
            } else if text.contains("internal") {
                return Some("internal".to_string());
            }
        }
    }
    None
}

fn node_text(node: Node, source: &str) -> String {
    source[node.start_byte()..node.end_byte()].to_string()
}

fn node_span(node: &Node) -> Span {
    Span {
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_col: node.start_position().column,
        end_col: node.end_position().column,
    }
}
