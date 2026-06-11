use crate::RawImport;
use kungfu_types::symbol::{Span, Symbol, SymbolKind};
use tree_sitter::Node;

pub fn extract_imports(root: Node, source: &str) -> Vec<RawImport> {
    // Same as C — #include directives
    crate::c_parser::extract_imports(root, source)
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
            "function_definition" => {
                extract_function(child, source, file_id, file_path, parent_id, symbols);
            }
            "class_specifier" => {
                extract_class(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Class,
                    symbols,
                );
            }
            "struct_specifier" => {
                extract_class(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    SymbolKind::Struct,
                    symbols,
                );
            }
            "enum_specifier" => {
                extract_enum(child, source, file_id, file_path, parent_id, symbols);
            }
            "namespace_definition" => {
                extract_namespace(child, source, file_id, file_path, parent_id, symbols);
            }
            "template_declaration" => {
                // Recurse into template to find the actual declaration
                collect_symbols(&child, source, file_id, file_path, parent_id, symbols);
            }
            "declaration" => {
                extract_declaration(child, source, file_id, file_path, parent_id, symbols);
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

fn extract_class(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    kind: SymbolKind,
    symbols: &mut Vec<Symbol>,
) {
    let name = match find_name(&node, source) {
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
        id: id.clone(),
        file_id: file_id.to_string(),
        name,
        kind,
        language: "cpp".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported: true,
        visibility: None,
        doc_summary: None,
    });

    // Recurse into body
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "field_declaration_list" {
            extract_class_members(&child, source, file_id, file_path, &id, symbols);
        }
    }
}

fn extract_class_members(
    node: &Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: &str,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    let mut current_visibility: Option<String> = None;

    for child in node.children(&mut cursor) {
        match child.kind() {
            "access_specifier" => {
                let text = node_text(child, source)
                    .trim_end_matches(':')
                    .trim()
                    .to_string();
                current_visibility = Some(text);
            }
            "function_definition" => {
                extract_method(
                    child,
                    source,
                    file_id,
                    file_path,
                    parent_id,
                    &current_visibility,
                    symbols,
                );
            }
            "declaration" => {
                // Could be a method declaration or field
                let text = node_text(child, source);
                let has_params = text.contains('(');
                if has_params {
                    extract_method_decl(
                        child,
                        source,
                        file_id,
                        file_path,
                        parent_id,
                        &current_visibility,
                        symbols,
                    );
                }
            }
            "field_declaration" => {
                // Skip fields for now — not as useful for agents
            }
            _ => {}
        }
    }
}

fn extract_method(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: &str,
    visibility: &Option<String>,
    symbols: &mut Vec<Symbol>,
) {
    let name = match find_function_name(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
    let sig = extract_sig_before_body(&node, source);
    let exported = !matches!(visibility.as_deref(), Some("private"));

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::Method,
        language: "cpp".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: Some(parent_id.to_string()),
        exported,
        visibility: visibility.clone(),
        doc_summary: None,
    });
}

fn extract_method_decl(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: &str,
    visibility: &Option<String>,
    symbols: &mut Vec<Symbol>,
) {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            if let Some(name) = find_name(&child, source) {
                let span = node_span(&node);
                let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
                let sig = node_text(node, source)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_end_matches(';')
                    .to_string();
                let exported = !matches!(visibility.as_deref(), Some("private"));

                symbols.push(Symbol {
                    id,
                    file_id: file_id.to_string(),
                    name,
                    kind: SymbolKind::Method,
                    language: "cpp".to_string(),
                    path: file_path.to_string(),
                    signature: Some(sig),
                    span,
                    parent_symbol_id: Some(parent_id.to_string()),
                    exported,
                    visibility: visibility.clone(),
                    doc_summary: None,
                });
            }
            return;
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
    let name = match find_function_name(&node, source) {
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
        language: "cpp".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
        span,
        parent_symbol_id: parent_id.map(|s| s.to_string()),
        exported: true,
        visibility: None,
        doc_summary: None,
    });
}

fn extract_namespace(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
    let name = find_name(&node, source).unwrap_or_default();
    if name.is_empty() {
        // Anonymous namespace — just recurse
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "declaration_list" {
                collect_symbols(&child, source, file_id, file_path, parent_id, symbols);
            }
        }
        return;
    }

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);

    symbols.push(Symbol {
        id: id.clone(),
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::Module,
        language: "cpp".to_string(),
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
        exported: true,
        visibility: None,
        doc_summary: None,
    });

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "declaration_list" {
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
    let name = match find_name(&node, source) {
        Some(n) => n,
        None => return,
    };

    let span = node_span(&node);
    let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::Enum,
        language: "cpp".to_string(),
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
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_declarator" {
            if let Some(name) = find_name(&child, source) {
                let span = node_span(&node);
                let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
                let sig = node_text(node, source)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim()
                    .trim_end_matches(';')
                    .to_string();
                symbols.push(Symbol {
                    id,
                    file_id: file_id.to_string(),
                    name,
                    kind: SymbolKind::Function,
                    language: "cpp".to_string(),
                    path: file_path.to_string(),
                    signature: Some(sig),
                    span,
                    parent_symbol_id: parent_id.map(|s| s.to_string()),
                    exported: true,
                    visibility: None,
                    doc_summary: None,
                });
            }
            return;
        }
    }
}

fn extract_typedef(
    node: Node,
    source: &str,
    file_id: &str,
    file_path: &str,
    parent_id: Option<&str>,
    symbols: &mut Vec<Symbol>,
) {
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
    let sig = node_text(node, source)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(';')
        .to_string();

    symbols.push(Symbol {
        id,
        file_id: file_id.to_string(),
        name,
        kind: SymbolKind::TypeAlias,
        language: "cpp".to_string(),
        path: file_path.to_string(),
        signature: Some(sig),
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
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier" {
            let name = node_text(child, source);
            if !name.is_empty() {
                let span = node_span(&node);
                let id = format!("s:{}:{}:{}", file_id, span.start_line, &name);
                symbols.push(Symbol {
                    id,
                    file_id: file_id.to_string(),
                    name,
                    kind: SymbolKind::Constant,
                    language: "cpp".to_string(),
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
                    parent_symbol_id: None,
                    exported: true,
                    visibility: None,
                    doc_summary: None,
                });
            }
            return;
        }
    }
}

fn find_function_name(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "function_declarator" | "pointer_declarator" => {
                return find_function_name(&child, source);
            }
            "qualified_identifier" | "identifier" | "destructor_name" | "operator_name" => {
                let text = node_text(child, source);
                if !text.is_empty() {
                    return Some(text);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_name(node: &Node, source: &str) -> Option<String> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "identifier"
            || child.kind() == "type_identifier"
            || child.kind() == "name"
            || child.kind() == "namespace_identifier"
        {
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

pub fn extract_calls(root: Node, source: &str) -> Vec<crate::RawCall> {
    static SYNTAX: crate::calls::CallSyntax = crate::calls::CallSyntax {
        caller_kinds: &["function_definition"],
        call_kinds: &["call_expression"],
        callee: callee_name,
    };
    crate::calls::extract_calls(root, source, &SYNTAX)
}

fn callee_name(node: Node, source: &str) -> Option<(String, bool)> {
    let func = node.child_by_field_name("function")?;
    resolve_cpp_callee(func, source)
}

fn resolve_cpp_callee(func: Node, source: &str) -> Option<(String, bool)> {
    match func.kind() {
        "identifier" => Some((crate::calls::node_text(func, source), false)),
        // obj.method() / ptr->method() — receiver type unknown at parse time
        "field_expression" => func
            .child_by_field_name("field")
            .map(|f| (crate::calls::node_text(f, source), true)),
        // ns::foo() / Class::static_call() — unwrap to the last segment
        "qualified_identifier" => {
            let name = func.child_by_field_name("name")?;
            if name.kind() == "qualified_identifier" {
                resolve_cpp_callee(name, source)
            } else {
                Some((crate::calls::node_text(name, source), false))
            }
        }
        // foo<T>() — unwrap the template wrapper
        "template_function" => {
            let name = func.child_by_field_name("name")?;
            Some((crate::calls::node_text(name, source), false))
        }
        _ => None,
    }
}
