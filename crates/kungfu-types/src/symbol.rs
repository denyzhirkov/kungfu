use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Class,
    Struct,
    Interface,
    TypeAlias,
    Constant,
    Enum,
    EnumVariant,
    Module,
    Trait,
    Impl,
    Variable,
    Field,
    Import,
    Export,
    /// A config node with a body: HCL block, TOML table, object/mapping-valued
    /// key, Dockerfile stage, Makefile target, XML element.
    ConfigBlock,
    /// Any other config entry: a scalar or array-valued key, a CSV column.
    ConfigKey,
}

impl SymbolKind {
    /// Symbols extracted from config/data files rather than code. Code-only
    /// consumers (call graph, glossary, test heuristics, onboarding) skip them.
    pub fn is_config(&self) -> bool {
        matches!(self, SymbolKind::ConfigBlock | SymbolKind::ConfigKey)
    }
}

impl std::fmt::Display for SymbolKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = serde_json::to_value(self)
            .ok()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| format!("{:?}", self));
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub start_line: usize,
    pub end_line: usize,
    pub start_col: usize,
    pub end_col: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    pub id: String,
    pub file_id: String,
    pub name: String,
    pub kind: SymbolKind,
    pub language: String,
    pub path: String,
    pub signature: Option<String>,
    pub span: Span,
    pub parent_symbol_id: Option<String>,
    pub exported: bool,
    pub visibility: Option<String>,
    pub doc_summary: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_kinds_serialize_snake_case() {
        assert_eq!(SymbolKind::ConfigBlock.to_string(), "config_block");
        assert_eq!(SymbolKind::ConfigKey.to_string(), "config_key");
        assert!(SymbolKind::ConfigKey.is_config());
        assert!(!SymbolKind::Function.is_config());
    }
}
