use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "import_statement" {
            let line = child.start_position().row + 1;
            // Extract the source string (the "from" path)
            if let Some(source_node) = child.child_by_field_name("source") {
                let path = node_text(source_node, source)
                    .trim_matches('\'')
                    .trim_matches('"')
                    .to_string();

                // Extract imported names
                let mut names = Vec::new();
                let mut inner_cursor = child.walk();
                for c in child.children(&mut inner_cursor) {
                    if c.kind() == "import_clause" {
                        collect_import_names(c, source, &mut names);
                    }
                }

                imports.push(RawImport { path, names, line });
            }
        }
        // Handle require() calls: const x = require('...')
        if child.kind() == "lexical_declaration" || child.kind() == "variable_declaration" {
            let text = &source[child.start_byte()..child.end_byte()];
            if let Some(start) = text.find("require(") {
                let after = &text[start + 8..];
                if let Some(end) = after.find(')') {
                    let path = after[..end]
                        .trim_matches('\'')
                        .trim_matches('"')
                        .to_string();
                    imports.push(RawImport {
                        path,
                        names: Vec::new(),
                        line: child.start_position().row + 1,
                    });
                }
            }
        }
    }

    imports
}

fn collect_import_names(node: Node, source: &str, names: &mut Vec<String>) {
    let kind = node.kind();
    if kind == "identifier" {
        names.push(node_text(node, source));
        return;
    }
    if kind == "import_specifier" {
        if let Some(name) = node.child_by_field_name("name") {
            names.push(node_text(name, source));
        }
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_import_names(child, source, names);
    }
}

pub fn extract(root: Node, source: &str, file_id: &str, file_path: &str) -> Vec<Symbol> {
    let mut symbols = Vec::new();
    collect_symbols(root, source, file_id, file_path, None, &mut symbols);
    symbols
}

fn collect_symbols(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let kind = node.kind();

    let symbol_kind = match kind {
        "function_declaration" => Some(SymbolKind::Function),
        "method_definition" => Some(SymbolKind::Method),
        "class_declaration" => Some(SymbolKind::Class),
        "interface_declaration" => Some(SymbolKind::Interface),
        "type_alias_declaration" => Some(SymbolKind::TypeAlias),
        "enum_declaration" => Some(SymbolKind::Enum),
        "lexical_declaration" | "variable_declaration" => {
            // Check if it's a const with arrow function
            if let Some(_declarator) = node.child_by_field_name("declarations") {
                // handled below
                None
            } else {
                None
            }
        }
        "export_statement" => {
            // Recurse into the exported declaration
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                collect_symbols(child, source, file_id, file_path, parent_id, symbols);
            }
            // Mark last added symbol as exported
            if let Some(last) = symbols.last_mut() {
                last.exported = true;
            }
            return;
        }
        _ => None,
    };

    if let Some(sk) = symbol_kind {
        if let Some(name) = find_name(&node, source) {
            let span = node_span(&node);
            let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
            let signature = extract_signature(&node, source);

            let sym = Symbol {
                id: id.clone(),
                file_id: file_id.to_string(),
                name,
                kind: sk,
                language: detect_lang(file_path),
                path: file_path.to_string(),
                signature,
                span,
                parent_symbol_id: parent_id.map(String::from),
                exported: false,
                visibility: None,
                doc_summary: None,
            };
            symbols.push(sym);

            // Recurse into class body for methods
            if sk == SymbolKind::Class {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut cursor = body.walk();
                    for child in body.children(&mut cursor) {
                        collect_symbols(child, source, file_id, file_path, Some(&id), symbols);
                    }
                }
                return;
            }
        }
    }

    // Handle const declarations (arrow functions, etc.)
    if kind == "lexical_declaration" || kind == "variable_declaration" {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "variable_declarator" {
                if let Some(name_node) = child.child_by_field_name("name") {
                    let name = node_text(name_node, source);
                    if let Some(value) = child.child_by_field_name("value") {
                        let vk = value.kind();
                        if vk == "arrow_function" || vk == "function" {
                            let span = node_span(&node);
                            let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
                            symbols.push(Symbol {
                                id,
                                file_id: file_id.to_string(),
                                name,
                                kind: SymbolKind::Function,
                                language: detect_lang(file_path),
                                path: file_path.to_string(),
                                signature: extract_signature(&node, source),
                                span,
                                parent_symbol_id: parent_id.map(String::from),
                                exported: false,
                                visibility: None,
                                doc_summary: None,
                            });
                        } else {
                            let span = node_span(&node);
                            let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
                            symbols.push(Symbol {
                                id,
                                file_id: file_id.to_string(),
                                name,
                                kind: SymbolKind::Variable,
                                language: detect_lang(file_path),
                                path: file_path.to_string(),
                                signature: None,
                                span,
                                parent_symbol_id: parent_id.map(String::from),
                                exported: false,
                                visibility: None,
                                doc_summary: None,
                            });
                        }
                    }
                }
            }
        }
        return;
    }

    // Recurse into children for other nodes
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, source, file_id, file_path, parent_id, symbols);
    }
}

fn find_name(node: &Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| node_text(n, source))
}

fn extract_signature(node: &Node, source: &str) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(start + 300);
    let text = &source[start..safe_char_boundary(source, end)];
    let sig = text
        .lines()
        .next()
        .unwrap_or(text)
        .trim()
        .trim_end_matches('{')
        .trim();
    Some(sig.to_string())
}

fn detect_lang(path: &str) -> String {
    if path.ends_with(".ts") || path.ends_with(".tsx") {
        "typescript".to_string()
    } else {
        "javascript".to_string()
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
        caller_kinds: &[
            "function_declaration",
            "generator_function_declaration",
            "method_definition",
            "arrow_function",
            "function_expression",
            "function", // older grammar name for function expressions
        ],
        call_kinds: &["call_expression"],
        callee: callee_name,
    };
    crate::calls::extract_calls(root, source, &SYNTAX)
}

fn callee_name(node: Node, source: &str) -> Option<(String, bool)> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some((crate::calls::node_text(func, source), false)),
        // obj.method() — the receiver type is unknown, mark as method
        "member_expression" => func
            .child_by_field_name("property")
            .map(|p| (crate::calls::node_text(p, source), true)),
        _ => None,
    }
}
