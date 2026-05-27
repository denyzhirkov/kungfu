use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "import_declaration" {
            // import java.util.List; or import java.util.*;
            let text = node_text(child, source);
            let path = text
                .trim_start_matches("import ")
                .trim_start_matches("static ")
                .trim_end_matches(';')
                .trim()
                .to_string();
            if !path.is_empty() {
                let names = if path.ends_with(".*") {
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
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        extract_node(child, source, file_id, file_path, None, &mut symbols);
    }

    symbols
}

fn extract_node(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    match node.kind() {
        "class_declaration" => {
            extract_type(
                node,
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
                node,
                source,
                file_id,
                file_path,
                parent_id,
                SymbolKind::Interface,
                symbols,
            );
        }
        "enum_declaration" => {
            extract_type(
                node,
                source,
                file_id,
                file_path,
                parent_id,
                SymbolKind::Enum,
                symbols,
            );
        }
        "method_declaration" => {
            extract_method(
                node,
                source,
                file_id,
                file_path,
                parent_id,
                SymbolKind::Method,
                symbols,
            );
        }
        "constructor_declaration" => {
            extract_method(
                node,
                source,
                file_id,
                file_path,
                parent_id,
                SymbolKind::Function,
                symbols,
            );
        }
        "field_declaration" => {
            extract_field(node, source, file_id, file_path, parent_id, symbols);
        }
        // Recurse into class/interface/enum bodies
        "class_body" | "interface_body" | "enum_body" | "enum_body_declarations" => {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                extract_node(child, source, file_id, file_path, parent_id, symbols);
            }
        }
        _ => {}
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
    let name_str = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source),
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
    let visibility = detect_visibility(&node, source);
    let exported = matches!(visibility.as_deref(), Some("public"));

    let sig = {
        let text = node_text(node, source);
        text.lines().next().unwrap_or("").trim().to_string()
    };

    symbols.push(Symbol {
        id: id.clone(),
        file_id: file_id.to_string(),
        name: name_str,
        kind,
        language: "java".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported,
        visibility,
        doc_summary: None,
    });

    // Recurse into body
    if let Some(body) = node.child_by_field_name("body") {
        let mut cursor = body.walk();
        for child in body.children(&mut cursor) {
            extract_node(child, source, file_id, file_path, Some(&id), symbols);
        }
    }
}

fn extract_method(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
) {
    let name_str = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source),
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
    let visibility = detect_visibility(&node, source);
    let exported = matches!(visibility.as_deref(), Some("public"));

    let sig = extract_sig_before_body(&node, source);

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name: name_str,
        kind,
        language: "java".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported,
        visibility,
        doc_summary: None,
    });
}

fn extract_field(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    // field_declaration has declarator children with names
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "variable_declarator" {
            if let Some(name_node) = child.child_by_field_name("name") {
                let name_str = node_text(name_node, source);
                let span = node_span(&node);
                let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
                let visibility = detect_visibility(&node, source);
                let exported = matches!(visibility.as_deref(), Some("public"));

                symbols.push(Symbol {
                    id,
                    file_id: file_id.to_string(),
                    name: name_str,
                    kind: SymbolKind::Variable,
                    language: "java".to_string(),
                    path: file_path.to_string(),
                    signature: Some(node_text(node, source).trim().to_string()),
                    span,
                    parent_symbol_id: parent_id.map(|s| s.to_string()),
                    exported,
                    visibility,
                    doc_summary: None,
                });
            }
        }
    }
}

fn extract_sig_before_body(node: &Node, source: &str) -> String {
    let start = node.start_byte();
    if let Some(body) = node.child_by_field_name("body") {
        let end = body.start_byte();
        source[start..end].trim().to_string()
    } else {
        let text = node_text(*node, source);
        text.lines().next().unwrap_or("").trim().to_string()
    }
}

fn detect_visibility(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "modifiers" {
            let text = node_text(child, source);
            if text.contains("public") {
                return Some("public".to_string());
            } else if text.contains("private") {
                return Some("private".to_string());
            } else if text.contains("protected") {
                return Some("protected".to_string());
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
