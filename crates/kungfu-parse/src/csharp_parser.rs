use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    collect_usings(&root, source, &mut imports);
    imports
}

fn collect_usings(node: &Node, source: &str, imports: &mut Vec<RawImport>) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "using_directive" {
            let text = node_text(child, source);
            let path = text
                .trim_start_matches("using ")
                .trim_start_matches("static ")
                .trim_end_matches(';')
                .trim()
                .to_string();
            // Skip aliases like "using Foo = Bar.Baz;"
            if !path.contains('=') && !path.is_empty() {
                let names = path
                    .rsplit('.')
                    .next()
                    .map(|n| vec![n.to_string()])
                    .unwrap_or_default();
                imports.push(RawImport {
                    path,
                    names,
                    line: child.start_position().row + 1,
                });
            }
        }
        // Recurse into namespaces to find nested usings
        if child.kind() == "namespace_declaration"
            || child.kind() == "file_scoped_namespace_declaration"
        {
            collect_usings(&child, source, imports);
        }
    }
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
            "struct_declaration" => {
                extract_type(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Struct,
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
            "enum_declaration" => {
                extract_type(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Enum,
                    symbols,
                );
            }
            "record_declaration" => {
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
            "method_declaration" => {
                extract_method(child, source, file_id, file_path, parent_id, symbols);
            }
            "constructor_declaration" => {
                extract_constructor(child, source, file_id, file_path, parent_id, symbols);
            }
            "property_declaration" => {
                extract_property(child, source, file_id, file_path, parent_id, symbols);
            }
            "namespace_declaration" | "file_scoped_namespace_declaration" | "declaration_list" => {
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
        language: "csharp".to_string(),
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
        collect_symbols(&body, source, file_id, file_path, Some(&id), symbols);
    }
}

fn extract_method(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
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
        kind: SymbolKind::Method,
        language: "csharp".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported,
        visibility,
        doc_summary: None,
    });
}

fn extract_constructor(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
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
        kind: SymbolKind::Function,
        language: "csharp".to_string(),
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
    let name_str = match node.child_by_field_name("name") {
        Some(n) => node_text(n, source),
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name_str);
    let visibility = detect_visibility(&node, source);
    let exported = matches!(visibility.as_deref(), Some("public"));

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name: name_str,
        kind: SymbolKind::Variable,
        language: "csharp".to_string(),
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
        let kind = child.kind();
        if kind == "modifier" {
            let text = node_text(child, source);
            match text.as_str() {
                "public" => return Some("public".to_string()),
                "private" => return Some("private".to_string()),
                "protected" => return Some("protected".to_string()),
                "internal" => return Some("internal".to_string()),
                _ => {}
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
