use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    let mut imports = Vec::new();
    let mut cursor = root.walk();

    for child in root.children(&mut cursor) {
        if child.kind() == "preproc_include" {
            // #include "file.h" or #include <stdio.h>
            let mut inner = child.walk();
            for c in child.children(&mut inner) {
                if c.kind() == "string_literal" || c.kind() == "system_lib_string" {
                    let text = node_text(c, source);
                    let path = text
                        .trim_matches('"')
                        .trim_matches('<')
                        .trim_matches('>')
                        .to_string();
                    if !path.is_empty() {
                        let names = path
                            .rsplit('/')
                            .next()
                            .map(|n| {
                                vec![n
                                    .trim_end_matches(".h")
                                    .trim_end_matches(".hpp")
                                    .to_string()]
                            })
                            .unwrap_or_default();
                        imports.push(RawImport {
                            path,
                            names,
                            line: child.start_position().row + 1,
                        });
                    }
                }
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
            "function_definition" | "function_declarator"
                if node.kind() != "function_definition" =>
            {
                // Skip declarators inside function_definition (handled by the definition itself)
            }
            "function_definition" => {
                extract_function(child, source, file_id, file_path, parent_id, symbols);
            }
            "declaration" => {
                extract_declaration(child, source, file_id, file_path, parent_id, symbols);
            }
            "struct_specifier" | "union_specifier" => {
                extract_struct(child, source, file_id, file_path, parent_id, symbols);
            }
            "enum_specifier" => {
                extract_enum(child, source, file_id, file_path, parent_id, symbols);
            }
            "type_definition" => {
                extract_typedef(child, source, file_id, file_path, parent_id, symbols);
            }
            "preproc_function_def" | "preproc_def" => {
                extract_macro(child, source, file_id, file_path, symbols);
            }
            _ => {
                collect_symbols(&child, source, file_id, file_path, parent_id, symbols);
            }
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
    let name = find_function_name(&node, source);
    let name = match name {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
    let sig = extract_sig_before_body(&node, source);

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::Function,
        language: "c".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported: true,
        visibility: None,
        doc_summary: None,
    });
}

fn extract_declaration(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    // Check if this is a function declaration (prototype)
    let text = node_text(node, source);
    let first_line = text.lines().next().unwrap_or("");

    // Skip simple variable declarations at file scope
    // Look for function declarations: they have a declarator with parameter_list
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            if let Some(name) = find_identifier(&child, source) {
                let span = node_span(&node);
                let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
                symbols.push(Symbol {
                    id,
                    file_id: file_id.to_string(),
                    name,
                    kind: SymbolKind::Function,
                    language: "c".to_string(),
                    path: file_path.to_string(),
                    signature: Some(first_line.trim().trim_end_matches(';').to_string()),
                    span,
                    parent_symbol_id: parent_id.map(|s| s.to_string()),
                    exported: true,
                    visibility: None,
                    doc_summary: None,
                });
            }
            return;
        }
        // Global variable with type specifier
        if child.kind() == "init_declarator" || child.kind() == "identifier" {
            // Skip — not interesting for symbol index
        }
    }
}

fn extract_struct(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match find_identifier(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
    let kind = SymbolKind::Struct;

    let sig = node_text(node, source)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    symbols.push(Symbol {
        id: id.clone(),
        file_id: file_id.to_string(),
        name,
        kind,
        language: "c".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported: true,
        visibility: None,
        doc_summary: None,
    });

    // Recurse into field_declaration_list for nested structs
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "field_declaration_list" {
            collect_symbols(&child, source, file_id, file_path, Some(&id), symbols);
        }
    }
}

fn extract_enum(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match find_identifier(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
    let sig = node_text(node, source)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::Enum,
        language: "c".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported: true,
        visibility: None,
        doc_summary: None,
    });
}

fn extract_typedef(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    // typedef <type> <name>;
    // The name is usually the last identifier before ';'
    let text = node_text(node, source);
    let first_line = text.lines().next().unwrap_or("").trim();

    // Find the type_identifier child (the alias name)
    let mut cursor = node.walk();
    let mut name = None;
    for child in node.children(&mut cursor) {
        if child.kind() == "type_identifier" || child.kind() == "identifier" {
            name = Some(node_text(child, source));
        }
    }

    let name = match name {
        Some(n) if !n.is_empty() => n,
        _ => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::TypeAlias,
        language: "c".to_string(),
        path: file_path.to_string(),
        signature: Some(first_line.trim_end_matches(';').to_string()),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported: true,
        visibility: None,
        doc_summary: None,
    });
}

fn extract_macro(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    symbols: &mut Vec<Symbol>,
) {
    let name = match find_macro_name(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
    let sig = node_text(node, source)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::Constant,
        language: "c".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: None,
        exported: true,
        visibility: None,
        doc_summary: None,
    });
}

fn find_function_name(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" || child.kind() == "pointer_declarator" {
            return find_function_name(&child, source);
        }
        if child.kind() == "identifier" {
            let text = node_text(child, source);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn find_identifier(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" || child.kind() == "type_identifier" {
            let text = node_text(child, source);
            if !text.is_empty()
                && text
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_alphabetic() || c == '_')
            {
                return Some(text);
            }
        }
    }
    None
}

fn find_macro_name(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            let text = node_text(child, source);
            if !text.is_empty() {
                return Some(text);
            }
        }
    }
    None
}

fn extract_sig_before_body(node: &Node, source: &str) -> String {
    let start = node.start_byte();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "compound_statement" {
            let end = child.start_byte();
            return source[start..end].trim().to_string();
        }
    }
    node_text(*node, source)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
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
