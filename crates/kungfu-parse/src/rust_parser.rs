use crate::{RawCall, RawImport};
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

/// Extract function/method call sites, each tagged with the enclosing function's start line.
/// Calls outside any function (e.g. in a `const` initializer) are ignored — there is no caller
/// symbol to attribute them to.
pub fn extract_calls(root: Node, source: &str) -> Vec<RawCall> {
    let mut calls = Vec::new();
    walk_calls(root, source, None, &mut calls);
    calls
}

fn walk_calls(node: Node, source: &str, enclosing_fn: Option<usize>, calls: &mut Vec<RawCall>) {
    // A `function_item` opens a new caller scope; nested fns/closures inherit until overridden.
    let enclosing = if node.kind() == "function_item" {
        Some(node.start_position().row + 1)
    } else {
        enclosing_fn
    };

    if node.kind() == "call_expression" {
        if let (Some(line), Some(func)) = (enclosing, node.child_by_field_name("function")) {
            if let Some(callee) = callee_name(func, source) {
                calls.push(RawCall {
                    caller_line: line,
                    callee,
                    is_method: func.kind() == "field_expression",
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk_calls(child, source, enclosing, calls);
    }
}

/// Resolve the simple callee name from a `call_expression`'s `function` child.
/// Handles free calls (`foo()`), path calls (`a::b::foo()`, `Type::foo()`), method calls
/// (`x.foo()`), and turbofish (`foo::<T>()`). Returns `None` for shapes we can't name.
fn callee_name(func: Node, source: &str) -> Option<String> {
    match func.kind() {
        "identifier" => Some(node_text(func, source)),
        // a::b::foo  → last segment
        "scoped_identifier" => func
            .child_by_field_name("name")
            .map(|n| node_text(n, source)),
        // x.foo()  → method name
        "field_expression" => func
            .child_by_field_name("field")
            .map(|n| node_text(n, source)),
        // foo::<T>()  → unwrap to the inner function node
        "generic_function" => func
            .child_by_field_name("function")
            .and_then(|inner| callee_name(inner, source)),
        _ => None,
    }
}

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "use_declaration" {
            let line = child.start_position().row + 1;
            let text = &source[child.start_byte()..child.end_byte()];
            // Parse "use path::to::module;" or "use path::to::{A, B};"
            let trimmed = text.trim_start_matches("use ").trim_end_matches(';').trim();

            if let Some(brace_pos) = trimmed.find('{') {
                // use path::{A, B}
                let base = trimmed[..brace_pos]
                    .trim_end_matches(':')
                    .trim_end_matches(':');
                let names_str = &trimmed[brace_pos + 1..trimmed.len().saturating_sub(1)];
                let names: Vec<String> = names_str
                    .split(',')
                    .map(|n| n.trim().to_string())
                    .filter(|n| !n.is_empty())
                    .collect();
                imports.push(RawImport {
                    path: base.to_string(),
                    names,
                    line,
                });
            } else {
                // use path::to::Thing or use path::to::*
                let parts: Vec<&str> = trimmed.rsplitn(2, "::").collect();
                if parts.len() == 2 {
                    imports.push(RawImport {
                        path: parts[1].to_string(),
                        names: vec![parts[0].to_string()],
                        line,
                    });
                } else {
                    imports.push(RawImport {
                        path: trimmed.to_string(),
                        names: Vec::new(),
                        line,
                    });
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
    let kind = node.kind();

    let symbol_kind = match kind {
        "function_item" => Some(SymbolKind::Function),
        "struct_item" => Some(SymbolKind::Struct),
        "enum_item" => Some(SymbolKind::Enum),
        "trait_item" => Some(SymbolKind::Trait),
        "impl_item" => Some(SymbolKind::Impl),
        "type_item" => Some(SymbolKind::TypeAlias),
        "const_item" => Some(SymbolKind::Constant),
        "static_item" => Some(SymbolKind::Constant),
        "mod_item" => Some(SymbolKind::Module),
        _ => None,
    };

    if let Some(sk) = symbol_kind {
        let name = find_name(&node, source, kind);
        if let Some(name) = name {
            let signature = extract_signature(&node, source, kind);
            let visibility = detect_visibility(&node, source);
            let span = node_span(&node);
            let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);

            let sym = Symbol {
                id: id.clone(),
                file_id: file_id.to_string(),
                name,
                kind: sk,
                language: "rust".to_string(),
                path: file_path.to_string(),
                signature,
                span,
                parent_symbol_id: parent_id.map(String::from),
                exported: visibility.as_deref() == Some("pub"),
                visibility,
                doc_summary: None,
            };
            symbols.push(sym);

            // Extract children (e.g., methods in impl blocks)
            if kind == "impl_item" {
                let mut child_cursor = node.walk();
                for child in node.children(&mut child_cursor) {
                    if child.kind() == "declaration_list" {
                        let mut inner_cursor = child.walk();
                        for inner in child.children(&mut inner_cursor) {
                            if inner.kind() == "function_item" {
                                let method_name = find_name(&inner, source, "function_item");
                                if let Some(mname) = method_name {
                                    let mspan = node_span(&inner);
                                    let mid =
                                        format!("s:{}:{}:{}", file_id, mspan.start_line, &mname);
                                    let msig = extract_signature(&inner, source, "function_item");
                                    let mvis = detect_visibility(&inner, source);
                                    symbols.push(Symbol {
                                        id: mid,
                                        file_id: file_id.to_string(),
                                        name: mname,
                                        kind: SymbolKind::Method,
                                        language: "rust".to_string(),
                                        path: file_path.to_string(),
                                        signature: msig,
                                        span: mspan,
                                        parent_symbol_id: Some(id.clone()),
                                        exported: mvis.as_deref() == Some("pub"),
                                        visibility: mvis,
                                        doc_summary: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }

            // Descend into module bodies so symbols inside `mod foo { ... }` (including
            // `#[cfg(test)] mod tests { ... }`) get indexed. Without this, every test
            // function in the project is invisible to find_symbol / smart_test / test_subjects.
            if kind == "mod_item" {
                if let Some(body) = node.child_by_field_name("body") {
                    let mut child_cursor = body.walk();
                    for inner in body.children(&mut child_cursor) {
                        extract_node(inner, source, file_id, file_path, Some(&id), symbols);
                    }
                }
            }
        }
    }
}

fn find_name(node: &Node, source: &str, kind: &str) -> Option<String> {
    match kind {
        "impl_item" => {
            // For impl, get the type name
            node.child_by_field_name("type")
                .map(|n| node_text(n, source))
        }
        _ => node
            .child_by_field_name("name")
            .map(|n| node_text(n, source)),
    }
}

fn extract_signature(node: &Node, source: &str, kind: &str) -> Option<String> {
    match kind {
        "function_item" => {
            // Get everything up to the body block
            let start = node.start_byte();
            if let Some(body) = node.child_by_field_name("body") {
                let end = body.start_byte();
                let sig = &source[start..end];
                Some(sig.trim().to_string())
            } else {
                Some(node_text(*node, source))
            }
        }
        "struct_item" | "enum_item" | "trait_item" | "type_item" => {
            let start = node.start_byte();
            let end = node.end_byte().min(start + 200);
            let text = &source[start..safe_char_boundary(source, end)];
            // Take first line or up to opening brace
            let sig = text
                .lines()
                .next()
                .unwrap_or(text)
                .trim()
                .trim_end_matches('{')
                .trim();
            Some(sig.to_string())
        }
        _ => None,
    }
}

fn detect_visibility(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "visibility_modifier" {
            return Some(node_text(child, source));
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_imports(source: &str) -> Vec<RawImport> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        extract_imports(tree.root_node(), source)
    }

    #[test]
    fn simple_use() {
        let imports = parse_imports("use std::path::Path;");
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, "std::path");
        assert_eq!(imports[0].names, vec!["Path"]);
    }

    #[test]
    fn grouped_use() {
        let imports = parse_imports("use std::collections::{HashMap, HashSet};");
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, "std::collections");
        assert!(imports[0].names.contains(&"HashMap".to_string()));
        assert!(imports[0].names.contains(&"HashSet".to_string()));
    }

    #[test]
    fn crate_use() {
        let imports = parse_imports("use crate::scanner;");
        assert_eq!(imports.len(), 1);
        assert_eq!(imports[0].path, "crate");
        assert_eq!(imports[0].names, vec!["scanner"]);
    }

    #[test]
    fn multiple_uses() {
        let source = "use std::path::Path;\nuse std::io::Result;\n";
        let imports = parse_imports(source);
        assert_eq!(imports.len(), 2);
    }

    #[test]
    fn extracts_symbols() {
        let source = "pub fn hello() {}\nstruct Foo {}";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let symbols = extract(tree.root_node(), source, "f:test", "test.rs");
        assert!(symbols.iter().any(|s| s.name == "hello"));
        assert!(symbols.iter().any(|s| s.name == "Foo"));
    }

    fn parse_calls(source: &str) -> Vec<RawCall> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        extract_calls(tree.root_node(), source)
    }

    #[test]
    fn extracts_call_shapes_with_enclosing_fn() {
        let source = "\
fn caller() {
    free_fn();
    a::b::path_fn();
    value.method_call();
    generic_fn::<u8>();
}
";
        let calls = parse_calls(source);
        let names: Vec<&str> = calls.iter().map(|c| c.callee.as_str()).collect();
        assert!(names.contains(&"free_fn"));
        assert!(names.contains(&"path_fn"));
        assert!(names.contains(&"method_call"));
        assert!(names.contains(&"generic_fn"));
        // Every call is attributed to `caller` (line 1).
        assert!(calls.iter().all(|c| c.caller_line == 1));
    }

    #[test]
    fn ignores_calls_outside_a_function() {
        // A call in a const initializer has no enclosing function — skip it.
        let calls = parse_calls("const N: usize = compute();");
        assert!(calls.is_empty());
    }
}
