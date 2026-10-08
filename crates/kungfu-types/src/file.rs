use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub id: String,
    pub path: String,
    pub extension: Option<String>,
    pub language: Option<String>,
    pub size: u64,
    pub hash: String,
    pub indexed_at: DateTime<Utc>,
    #[serde(default)]
    pub tags: Vec<String>,
    /// One-line file purpose extracted from the module-level doc comment
    /// (Rust `//!`, Python module docstring, Go package comment, `/**` file header).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// Where `purpose` came from: "doc" (authored module doc — the trusted layer),
    /// "agent" (annotation via annotate_file), or "agent-stale" (annotation whose
    /// content hash no longer matches the file). Absent when purpose is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose_source: Option<String>,
}

/// What the last tree scan saw, for the lazy freshness check. Anything whose
/// mtime is at or after its reference time may be missing from the index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TreeState {
    /// Start of the last full/incremental scan — the default reference. The
    /// start, not the end: an edit made while the scan ran must stay visible.
    pub scanned_at: DateTime<Utc>,
    /// Directories the scan walked (including empty ones), relative to the
    /// root; `""` is the root itself.
    pub dirs: Vec<String>,
    /// Later references set by targeted reindexes: when a file was re-read,
    /// or when a directory's listing was checked against the index.
    #[serde(default)]
    pub refreshed: std::collections::HashMap<String, DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    TypeScript,
    JavaScript,
    Python,
    Go,
    Java,
    CSharp,
    Kotlin,
    C,
    Cpp,
    Json,
    Yaml,
    Markdown,
    Toml,
    Unknown,
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "rs" => Language::Rust,
            "ts" | "tsx" => Language::TypeScript,
            "js" | "jsx" | "mjs" | "cjs" => Language::JavaScript,
            "py" | "pyi" => Language::Python,
            "go" => Language::Go,
            "java" => Language::Java,
            "cs" => Language::CSharp,
            "kt" | "kts" => Language::Kotlin,
            "c" => Language::C,
            "cpp" | "cc" | "cxx" | "c++" => Language::Cpp,
            "h" | "hpp" | "hxx" | "hh" => Language::Cpp,
            "json" => Language::Json,
            "yaml" | "yml" => Language::Yaml,
            "md" | "mdx" => Language::Markdown,
            "toml" => Language::Toml,
            _ => Language::Unknown,
        }
    }

    pub fn is_code(&self) -> bool {
        matches!(
            self,
            Language::Rust
                | Language::TypeScript
                | Language::JavaScript
                | Language::Python
                | Language::Go
                | Language::Java
                | Language::CSharp
                | Language::Kotlin
                | Language::C
                | Language::Cpp
        )
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Language::Rust => write!(f, "rust"),
            Language::TypeScript => write!(f, "typescript"),
            Language::JavaScript => write!(f, "javascript"),
            Language::Python => write!(f, "python"),
            Language::Go => write!(f, "go"),
            Language::Java => write!(f, "java"),
            Language::CSharp => write!(f, "csharp"),
            Language::Kotlin => write!(f, "kotlin"),
            Language::C => write!(f, "c"),
            Language::Cpp => write!(f, "cpp"),
            Language::Json => write!(f, "json"),
            Language::Yaml => write!(f, "yaml"),
            Language::Markdown => write!(f, "markdown"),
            Language::Toml => write!(f, "toml"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}
