use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "import_declaration" {
            let mut inner_cursor = child.walk();
            for spec in child.children(&mut inner_cursor) {
                if spec.kind() == "import_spec" || spec.kind() == "interpreted_string_literal" {
                    let text = node_text(spec, source);
                    let path = text.trim_matches('"').to_string();
                    if !path.is_empty() {
                        imports.push(RawImport {
                            path,
                            names: Vec::new(),
                            line: spec.start_position().row + 1,
                        });
                    }
                } else if spec.kind() == "import_spec_list" {
                    let mut list_cursor = spec.walk();
                    for item in spec.children(&mut list_cursor) {
                        if item.kind() == "import_spec" {
                            if let Some(path_node) = item.child_by_field_name("path") {
                                let path =
                                    node_text(path_node, source).trim_matches('"').to_string();
                                imports.push(RawImport {
                                    path,
                                    names: Vec::new(),
                                    line: item.start_position().row + 1,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    imports
}

pub fn extract(root: Node, source: &str, file_id: &str, file_path: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        match child.kind() {
            "function_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    let name_str = node_text(name, source);
                    let span = node_span(&child);
                    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
                    symbols.push(Symbol {
                        id,
                        file_id: file_id.to_string(),
                        name: name_str.clone(),
                        kind: SymbolKind::Function,
                        language: "go".to_string(),
                        path: file_path.to_string(),
                        signature: extract_func_sig(&child, source),
                        span,
                        parent_symbol_id: None,
                        exported: name_str.chars().next().is_some_and(|c| c.is_uppercase()),
                        visibility: None,
                        doc_summary: None,
                    });
                }
            }
            "method_declaration" => {
                if let Some(name) = child.child_by_field_name("name") {
                    let name_str = node_text(name, source);
                    let span = node_span(&child);
                    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
                    symbols.push(Symbol {
                        id,
                        file_id: file_id.to_string(),
                        name: name_str.clone(),
                        kind: SymbolKind::Method,
                        language: "go".to_string(),
                        path: file_path.to_string(),
                        signature: extract_func_sig(&child, source),
                        span,
                        parent_symbol_id: None,
                        exported: name_str.chars().next().is_some_and(|c| c.is_uppercase()),
                        visibility: None,
                        doc_summary: None,
                    });
                }
            }
            "type_declaration" => {
                let mut inner_cursor = child.walk();
                for spec in child.children(&mut inner_cursor) {
                    if spec.kind() == "type_spec" {
                        if let Some(name) = spec.child_by_field_name("name") {
                            let name_str = node_text(name, source);
                            let type_node = spec.child_by_field_name("type");
                            let sk = match type_node.map(|t| t.kind()) {
                                Some("struct_type") => SymbolKind::Struct,
                                Some("interface_type") => SymbolKind::Interface,
                                _ => SymbolKind::TypeAlias,
                            };
                            let span = node_span(&spec);
                            let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
                            symbols.push(Symbol {
                                id,
                                file_id: file_id.to_string(),
                                name: name_str.clone(),
                                kind: sk,
                                language: "go".to_string(),
                                path: file_path.to_string(),
                                signature: Some(format!(
                                    "type {}",
                                    node_text(spec, source).lines().next().unwrap_or("")
                                )),
                                span,
                                parent_symbol_id: None,
                                exported: name_str.chars().next().is_some_and(|c| c.is_uppercase()),
                                visibility: None,
                                doc_summary: None,
                            });
                        }
                    }
                }
            }
            _ => {}
        }
    }

    symbols
}

fn extract_func_sig(node: &Node, source: &str) -> Option<String> {
    let start = node.start_byte();
    if let Some(body) = node.child_by_field_name("body") {
        let end = body.start_byte();
        Some(source[start..end].trim().to_string())
    } else {
        let end = node.end_byte().min(start + 200);
        let text = &source[start..safe_char_boundary(source, end)];
        Some(text.lines().next().unwrap_or(text).trim().to_string())
    }
}

fn node_text(node: Node, source: &str) -> String {
    source[node.start_byte()..node.end_byte()].to_string()
}

fn safe_char_boundary(s: &str, index: usize) -> usize {
    if index >= s.len() {
        return s.len();
    }
    let mut i = index;
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn node_span(node: &Node) -> Span {
    Span {
        start_line: node.start_position().row + 1,
        end_line: node.end_position().row + 1,
        start_col: node.start_position().column,
        end_col: node.end_position().column,
    }
}

pub fn extract_calls(root: Node, source: &str) -> Vec<crate::RawCall> {
    static SYNTAX: crate::calls::CallSyntax = crate::calls::CallSyntax {
        caller_kinds: &["function_declaration", "method_declaration"],
        call_kinds: &["call_expression"],
        callee: callee_name,
    };
    crate::calls::extract_calls(root, source, &SYNTAX)
}

fn callee_name(node: Node, source: &str) -> Option<(String, bool)> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some((crate::calls::node_text(func, source), false)),
        // pkg.Func() / recv.Method() — both look the same syntactically
        "selector_expression" => func
            .child_by_field_name("field")
            .map(|f| (crate::calls::node_text(f, source), true)),
        _ => None,
    }
}
