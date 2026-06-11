use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        let kind = child.kind();
        let line = child.start_position().row + 1;

        if kind == "import_statement" {
            // import foo / import foo.bar
            let text = &source[child.start_byte()..child.end_byte()];
            let path = text.trim_start_matches("import ").trim().to_string();
            imports.push(RawImport {
                path,
                names: Vec::new(),
                line,
            });
        } else if kind == "import_from_statement" {
            // from foo.bar import Baz, Qux
            let mut module = String::new();
            let mut names = Vec::new();
            let mut inner_cursor = child.walk();
            for c in child.children(&mut inner_cursor) {
                match c.kind() {
                    "dotted_name" | "relative_import" if module.is_empty() => {
                        module = node_text(c, source);
                    }
                    _ => {
                        // Look for imported names
                        if c.kind() == "dotted_name" || c.kind() == "aliased_import" {
                            let name = if c.kind() == "aliased_import" {
                                c.child_by_field_name("name")
                                    .map(|n| node_text(n, source))
                                    .unwrap_or_default()
                            } else {
                                node_text(c, source)
                            };
                            if !name.is_empty() && name != module {
                                names.push(name);
                            }
                        }
                    }
                }
            }
            if !module.is_empty() {
                imports.push(RawImport {
                    path: module,
                    names,
                    line,
                });
            }
        }
    }

    imports
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
        "function_definition" => Some(SymbolKind::Function),
        "class_definition" => Some(SymbolKind::Class),
        _ => None,
    };

    if let Some(sk) = symbol_kind {
        if let Some(name_node) = node.child_by_field_name("name") {
            let name = node_text(name_node, source);
            let span = node_span(&node);
            let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
            let signature = extract_signature(&node, source, sk);

            let actual_kind = if sk == SymbolKind::Function && parent_id.is_some() {
                SymbolKind::Method
            } else {
                sk
            };

            let sym = Symbol {
                id: id.clone(),
                file_id: file_id.to_string(),
                name: name.clone(),
                kind: actual_kind,
                language: "python".to_string(),
                path: file_path.to_string(),
                signature,
                span,
                parent_symbol_id: parent_id.map(String::from),
                exported: !name.starts_with('_'),
                visibility: if name.starts_with('_') {
                    Some("private".to_string())
                } else {
                    Some("public".to_string())
                },
                doc_summary: None,
            };
            symbols.push(sym);

            // Recurse into class body
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

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        collect_symbols(child, source, file_id, file_path, parent_id, symbols);
    }
}

fn extract_signature(node: &Node, source: &str, _kind: SymbolKind) -> Option<String> {
    let start = node.start_byte();
    let end = node.end_byte().min(start + 300);
    let text = &source[start..safe_char_boundary(source, end)];
    let sig = text.lines().next().unwrap_or(text).trim();
    Some(sig.trim_end_matches(':').trim().to_string())
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
        caller_kinds: &["function_definition"],
        call_kinds: &["call"],
        callee: callee_name,
    };
    crate::calls::extract_calls(root, source, &SYNTAX)
}

fn callee_name(node: Node, source: &str) -> Option<(String, bool)> {
    let func = node.child_by_field_name("function")?;
    match func.kind() {
        "identifier" => Some((crate::calls::node_text(func, source), false)),
        // obj.method() / module.func() — receiver unknown, mark as method
        "attribute" => func
            .child_by_field_name("attribute")
            .map(|a| (crate::calls::node_text(a, source), true)),
        _ => None,
    }
}
