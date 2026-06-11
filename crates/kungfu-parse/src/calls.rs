//! Shared call-site extraction walker. Each language supplies a small syntax
//! table (which nodes open a caller scope, which nodes are calls, how to name
//! the callee); the traversal and caller attribution live here once.

use crate::RawCall;
use tree_sitter::Node;

pub(crate) struct CallSyntax {
    /// Node kinds that open a caller scope (function/method definitions).
    /// Must match the nodes the language's `extract` turns into Function/Method
    /// symbols — caller attribution joins on the symbol's start line.
    pub caller_kinds: &'static [&'static str],
    /// Node kinds that represent a call site.
    pub call_kinds: &'static [&'static str],
    /// Extract (simple callee name, is_method) from a call node.
    /// Return None for shapes that can't be named (computed calls, lambdas).
    pub callee: fn(Node, &str) -> Option<(String, bool)>,
}

pub(crate) fn extract_calls(root: Node, source: &str, syntax: &CallSyntax) -> Vec<RawCall> {
    let mut calls = Vec::new();
    walk(root, source, None, syntax, &mut calls);
    calls
}

fn walk(
    node: Node,
    source: &str,
    enclosing_fn: Option<usize>,
    syntax: &CallSyntax,
    calls: &mut Vec<RawCall>,
) {
    let enclosing = if syntax.caller_kinds.contains(&node.kind()) {
        Some(node.start_position().row + 1)
    } else {
        enclosing_fn
    };

    if syntax.call_kinds.contains(&node.kind()) {
        if let Some(line) = enclosing {
            if let Some((callee, is_method)) = (syntax.callee)(node, source) {
                calls.push(RawCall {
                    caller_line: line,
                    callee,
                    is_method,
                });
            }
        }
    }

    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, enclosing, syntax, calls);
    }
}

pub(crate) fn node_text(node: Node, source: &str) -> String {
    source[node.start_byte()..node.end_byte()].to_string()
}

/// Rightmost descendant of the given kind within `node` (max start byte) —
/// used for grammars where the callee name is nested (e.g. Kotlin `a.b.c()`:
/// the method name is the last identifier of the navigation chain).
pub(crate) fn last_descendant_of_kind<'a>(node: Node<'a>, kind: &str) -> Option<Node<'a>> {
    let mut best: Option<Node<'a>> = None;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == kind && best.is_none_or(|b| n.start_byte() > b.start_byte()) {
            best = Some(n);
        }
        let mut cursor = n.walk();
        for child in n.children(&mut cursor) {
            stack.push(child);
        }
    }
    best
}
