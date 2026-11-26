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
    /// Set only when `[index] max_config_symbols_per_file` trimmed this config
    /// file's symbols: how many config symbols the file has in total. The index
    /// keeps `max_config_symbols_per_file` of them — level-1 nodes first, then
    /// level-2 nodes in document order — so retrieval can declare
    /// "showing N of M".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_symbols_total: Option<usize>,
    /// The config parser recovered from syntax errors in this file (JSON5 in a
    /// `.json`, a missing comma), so its config symbols may be incomplete.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub config_parse_errors: bool,
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
    Hcl,
    Csv,
    Env,
    Ini,
    Xml,
    Dockerfile,
    Makefile,
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
            "tf" | "tfvars" | "hcl" => Language::Hcl,
            "csv" | "tsv" => Language::Csv,
            "ini" | "cfg" | "properties" => Language::Ini,
            "xml" => Language::Xml,
            "mk" => Language::Makefile,
            "dockerfile" => Language::Dockerfile,
            _ => Language::Unknown,
        }
    }

    /// Detect the language from a path: the file name first (Dockerfile,
    /// Makefile and `.env` files carry no usable extension), then the
    /// extension. Prefer this over [`Language::from_extension`] whenever a
    /// path is available.
    pub fn from_path(path: &std::path::Path) -> Self {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        match name {
            "Dockerfile" | "Containerfile" => return Language::Dockerfile,
            "Makefile" | "makefile" | "GNUmakefile" => return Language::Makefile,
            ".env" => return Language::Env,
            _ => {}
        }
        if name.starts_with(".env.") {
            return Language::Env;
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        // `Dockerfile.dev`, `Dockerfile.prod` — but not `Dockerfile.md`.
        if name.starts_with("Dockerfile.") && Self::from_extension(ext) == Language::Unknown {
            return Language::Dockerfile;
        }
        Self::from_extension(ext)
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

    /// Structured config/data formats whose content is indexed as
    /// `ConfigKey`/`ConfigBlock` symbols. Markdown is prose, not config.
    pub fn is_config(&self) -> bool {
        matches!(
            self,
            Language::Json
                | Language::Yaml
                | Language::Toml
                | Language::Hcl
                | Language::Csv
                | Language::Env
                | Language::Ini
                | Language::Xml
                | Language::Dockerfile
                | Language::Makefile
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
            Language::Hcl => write!(f, "hcl"),
            Language::Csv => write!(f, "csv"),
            Language::Env => write!(f, "env"),
            Language::Ini => write!(f, "ini"),
            Language::Xml => write!(f, "xml"),
            Language::Dockerfile => write!(f, "dockerfile"),
            Language::Makefile => write!(f, "makefile"),
            Language::Unknown => write!(f, "unknown"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn lang(p: &str) -> Language {
        Language::from_path(Path::new(p))
    }

    #[test]
    fn from_path_detects_extensionless_config_files() {
        assert_eq!(lang("Dockerfile"), Language::Dockerfile);
        assert_eq!(lang("deploy/Dockerfile.prod"), Language::Dockerfile);
        assert_eq!(lang("Containerfile"), Language::Dockerfile);
        assert_eq!(lang("web.dockerfile"), Language::Dockerfile);
        assert_eq!(lang("Makefile"), Language::Makefile);
        assert_eq!(lang("sub/GNUmakefile"), Language::Makefile);
        assert_eq!(lang("rules.mk"), Language::Makefile);
        assert_eq!(lang(".env"), Language::Env);
        assert_eq!(lang("app/.env.local"), Language::Env);
    }

    #[test]
    fn from_path_maps_config_extensions() {
        assert_eq!(lang("main.tf"), Language::Hcl);
        assert_eq!(lang("prod.tfvars"), Language::Hcl);
        assert_eq!(lang("job.hcl"), Language::Hcl);
        assert_eq!(lang("data.csv"), Language::Csv);
        assert_eq!(lang("data.tsv"), Language::Csv);
        assert_eq!(lang("setup.cfg"), Language::Ini);
        assert_eq!(lang("app.properties"), Language::Ini);
        assert_eq!(lang("tox.ini"), Language::Ini);
        assert_eq!(lang("pom.xml"), Language::Xml);
        assert_eq!(lang("package.json"), Language::Json);
    }

    #[test]
    fn from_path_keeps_code_and_lookalikes_apart() {
        assert_eq!(lang("src/main.rs"), Language::Rust);
        assert_eq!(lang("Dockerfile.md"), Language::Markdown);
        assert_eq!(lang("environment.rs"), Language::Rust);
        assert_eq!(lang(".envrc"), Language::Unknown);
        // nginx/httpd/redis `.conf` files are not INI.
        assert_eq!(lang("nginx.conf"), Language::Unknown);
        assert_eq!(lang("notes.txt"), Language::Unknown);
    }

    #[test]
    fn config_languages_exclude_code_and_markdown() {
        assert!(Language::Json.is_config());
        assert!(Language::Dockerfile.is_config());
        assert!(!Language::Markdown.is_config());
        assert!(!Language::Rust.is_config());
        assert!(!Language::Unknown.is_config());
        assert!(!Language::Json.is_code());
    }

    #[test]
    fn new_languages_serialize_lowercase() {
        assert_eq!(
            serde_json::to_string(&Language::Dockerfile).unwrap(),
            "\"dockerfile\""
        );
        assert_eq!(Language::Hcl.to_string(), "hcl");
    }
}
