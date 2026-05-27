#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod c_parser;
pub mod cpp_parser;
pub mod csharp_parser;
pub mod go_parser;
pub mod java_parser;
pub mod kotlin_parser;
pub mod python_parser;
pub mod rust_parser;
pub mod typescript_parser;

use anyhow::{bail, Result};
use kungfu_types::file::Language;
use kungfu_types::symbol::Symbol;

/// Raw import extracted from source code.
#[derive(Debug, Clone)]
pub struct RawImport {
    /// The import path as written in source (e.g. "crate::scanner", "./bar", "fmt").
    pub path: String,
    /// Specific names imported (e.g. ["Result", "Context"]), empty for wildcard/module imports.
    pub names: Vec<String>,
    /// Line number of the import statement.
    pub line: usize,
}

/// Classification of a source code comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentKind {
    Todo,
    Fixme,
    Note,
    Hack,
    Doc,
    Regular,
}

/// A comment extracted from source code.
#[derive(Debug, Clone)]
pub struct RawComment {
    pub text: String,
    pub line: usize,
    pub end_line: usize,
    pub kind: CommentKind,
    /// ID of the symbol this comment is attached to (directly precedes).
    pub attached_symbol_id: Option<String>,
}

pub struct ParseResult {
    pub symbols: Vec<Symbol>,
    pub imports: Vec<RawImport>,
    pub comments: Vec<RawComment>,
}

/// Classify a comment's text into a CommentKind.
fn classify_comment(text: &str) -> CommentKind {
    let trimmed = text.trim_start_matches('/').trim_start_matches('*').trim();
    let upper = trimmed.to_uppercase();
    if upper.starts_with("TODO") {
        CommentKind::Todo
    } else if upper.starts_with("FIXME") {
        CommentKind::Fixme
    } else if upper.starts_with("NOTE") {
        CommentKind::Note
    } else if upper.starts_with("HACK") || upper.starts_with("XXX") {
        CommentKind::Hack
    } else {
        CommentKind::Regular
    }
}

/// Check if a tree-sitter node is a doc comment based on language conventions.
fn is_doc_comment(text: &str, language: Language) -> bool {
    match language {
        Language::Rust => {
            text.starts_with("///") || text.starts_with("//!") || text.starts_with("/**")
        }
        Language::Java | Language::CSharp | Language::Kotlin | Language::Cpp => {
            text.starts_with("/**")
        }
        Language::TypeScript | Language::JavaScript => text.starts_with("/**"),
        Language::Python => text.starts_with("\"\"\"") || text.starts_with("'''"),
        _ => false,
    }
}

/// Comment node types per language grammar.
fn comment_node_types(language: Language) -> &'static [&'static str] {
    match language {
        Language::Rust => &["line_comment", "block_comment"],
        Language::TypeScript | Language::JavaScript => &["comment"],
        Language::Python => &["comment"],
        Language::Go => &["comment"],
        Language::Java => &["line_comment", "block_comment"],
        Language::CSharp => &["comment"],
        Language::Kotlin => &["line_comment", "multiline_comment"],
        Language::C | Language::Cpp => &["comment"],
        _ => &[],
    }
}

/// Extract all comments from a tree-sitter AST.
fn extract_comments(
    root: tree_sitter::Node,
    source: &str,
    language: Language,
    symbols: &[Symbol],
) -> Vec<RawComment> {
    let node_types = comment_node_types(language);
    if node_types.is_empty() {
        return Vec::new();
    }

    let mut comments = Vec::new();
    let mut cursor = root.walk();
    let mut stack = vec![root];

    while let Some(node) = stack.pop() {
        if node_types.contains(&node.kind()) {
            let text = node.utf8_text(source.as_bytes()).unwrap_or("").to_string();
            if text.trim().is_empty() {
                continue;
            }

            let line = node.start_position().row + 1;
            let end_line = node.end_position().row + 1;

            let kind = if is_doc_comment(&text, language) {
                CommentKind::Doc
            } else {
                classify_comment(&text)
            };

            // Only keep actionable comments (skip Regular)
            if kind == CommentKind::Regular {
                continue;
            }

            // Find symbol attached to this comment (next sibling or first symbol starting right after)
            let attached_symbol_id = symbols
                .iter()
                .find(|s| s.span.start_line == end_line + 1 || s.span.start_line == end_line)
                .map(|s| s.id.clone());

            comments.push(RawComment {
                text: clean_comment_text(&text),
                line,
                end_line,
                kind,
                attached_symbol_id,
            });
        }

        // Push children in reverse order for DFS
        cursor.reset(node);
        if cursor.goto_first_child() {
            let mut children = Vec::new();
            loop {
                children.push(cursor.node());
                if !cursor.goto_next_sibling() {
                    break;
                }
            }
            for child in children.into_iter().rev() {
                stack.push(child);
            }
        }
    }

    comments
}

/// Strip comment delimiters and normalize whitespace.
fn clean_comment_text(text: &str) -> String {
    let text = text.trim();
    // Handle multi-line block comments
    if text.starts_with("/**") || text.starts_with("/*") {
        let inner = text
            .trim_start_matches("/**")
            .trim_start_matches("/*")
            .trim_end_matches("*/")
            .trim();
        return inner
            .lines()
            .map(|l| l.trim().trim_start_matches('*').trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
    }
    // Handle line comments
    if text.starts_with("///") || text.starts_with("//!") {
        return text
            .trim_start_matches('/')
            .trim_start_matches('!')
            .trim()
            .to_string();
    }
    if text.starts_with("//") {
        return text.trim_start_matches('/').trim().to_string();
    }
    // Python docstrings
    if text.starts_with("\"\"\"") || text.starts_with("'''") {
        return text
            .trim_start_matches('"')
            .trim_start_matches('\'')
            .trim_end_matches('"')
            .trim_end_matches('\'')
            .trim()
            .to_string();
    }
    text.to_string()
}

/// Fill `doc_summary` on symbols from their attached Doc comments.
fn fill_doc_summaries(symbols: &mut [Symbol], comments: &[RawComment]) {
    for comment in comments {
        if comment.kind != CommentKind::Doc {
            continue;
        }
        if let Some(ref sym_id) = comment.attached_symbol_id {
            if let Some(sym) = symbols.iter_mut().find(|s| s.id == *sym_id) {
                if sym.doc_summary.is_none() {
                    sym.doc_summary = Some(first_sentence(&comment.text, 120));
                }
            }
        }
    }
}

/// Extract the first sentence from text, truncated to max_len.
fn first_sentence(text: &str, max_len: usize) -> String {
    // Take first line or up to first period
    let first = text.lines().next().unwrap_or(text).trim();
    let sentence = if let Some(dot) = first.find(". ") {
        &first[..=dot]
    } else {
        first
    };
    if sentence.len() <= max_len {
        sentence.to_string()
    } else {
        format!("{}...", &sentence[..max_len])
    }
}

pub struct Parser {
    ts_parser: tree_sitter::Parser,
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

impl Parser {
    pub fn new() -> Self {
        Self {
            ts_parser: tree_sitter::Parser::new(),
        }
    }

    pub fn extract_symbols(
        &mut self,
        source: &str,
        language: Language,
        file_id: &str,
        file_path: &str,
    ) -> Result<Vec<Symbol>> {
        Ok(self.parse(source, language, file_id, file_path)?.symbols)
    }

    pub fn parse(
        &mut self,
        source: &str,
        language: Language,
        file_id: &str,
        file_path: &str,
    ) -> Result<ParseResult> {
        let ts_language = match language {
            Language::Rust => tree_sitter_rust::LANGUAGE.into(),
            Language::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Language::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Language::Python => tree_sitter_python::LANGUAGE.into(),
            Language::Go => tree_sitter_go::LANGUAGE.into(),
            Language::Java => tree_sitter_java::LANGUAGE.into(),
            Language::CSharp => tree_sitter_c_sharp::LANGUAGE.into(),
            Language::Kotlin => tree_sitter_kotlin_ng::LANGUAGE.into(),
            Language::C => tree_sitter_c::LANGUAGE.into(),
            Language::Cpp => tree_sitter_cpp::LANGUAGE.into(),
            _ => bail!("no parser for language: {}", language),
        };

        self.ts_parser.set_language(&ts_language)?;

        let tree = self
            .ts_parser
            .parse(source, None)
            .ok_or_else(|| anyhow::anyhow!("failed to parse {}", file_path))?;

        let root = tree.root_node();

        let (mut symbols, imports) = match language {
            Language::Rust => (
                rust_parser::extract(root, source, file_id, file_path),
                rust_parser::extract_imports(root, source),
            ),
            Language::TypeScript | Language::JavaScript => (
                typescript_parser::extract(root, source, file_id, file_path),
                typescript_parser::extract_imports(root, source),
            ),
            Language::Python => (
                python_parser::extract(root, source, file_id, file_path),
                python_parser::extract_imports(root, source),
            ),
            Language::Go => (
                go_parser::extract(root, source, file_id, file_path),
                go_parser::extract_imports(root, source),
            ),
            Language::Java => (
                java_parser::extract(root, source, file_id, file_path),
                java_parser::extract_imports(root, source),
            ),
            Language::CSharp => (
                csharp_parser::extract(root, source, file_id, file_path),
                csharp_parser::extract_imports(root, source),
            ),
            Language::Kotlin => (
                kotlin_parser::extract(root, source, file_id, file_path),
                kotlin_parser::extract_imports(root, source),
            ),
            Language::C => (
                c_parser::extract(root, source, file_id, file_path),
                c_parser::extract_imports(root, source),
            ),
            Language::Cpp => (
                cpp_parser::extract(root, source, file_id, file_path),
                cpp_parser::extract_imports(root, source),
            ),
            _ => (Vec::new(), Vec::new()),
        };

        let comments = extract_comments(root, source, language, &symbols);

        // Fill doc_summary on symbols from attached Doc comments
        fill_doc_summaries(&mut symbols, &comments);

        Ok(ParseResult {
            symbols,
            imports,
            comments,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kungfu_types::symbol::SymbolKind;

    #[test]
    fn rust_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
use std::path::Path;

pub fn hello() {
    println!("hi");
}

struct Foo {
    x: i32,
}
"#,
                Language::Rust,
                "f:test",
                "test.rs",
            )
            .unwrap();
        assert!(result.symbols.iter().any(|s| s.name == "hello"));
        assert!(result.symbols.iter().any(|s| s.name == "Foo"));
        assert!(!result.imports.is_empty());
    }

    #[test]
    fn java_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.List;
import java.util.Map;

public class UserService {
    private final String name;

    public UserService(String name) {
        this.name = name;
    }

    public List<String> getItems() {
        return List.of();
    }

    public interface Callback {
        void onResult(String result);
    }

    public enum Status {
        ACTIVE, INACTIVE
    }
}
"#,
                Language::Java,
                "f:test",
                "Test.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got: {:?}", names);
        assert!(names.contains(&"getItems"), "got: {:?}", names);
        assert!(names.contains(&"Callback"), "got: {:?}", names);
        assert!(names.contains(&"Status"), "got: {:?}", names);
        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn csharp_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System;
using System.Collections.Generic;

namespace MyApp {
    public class UserService {
        private readonly string _name;

        public UserService(string name) {
            _name = name;
        }

        public List<string> GetItems() {
            return new List<string>();
        }

        public interface ICallback {
            void OnResult(string result);
        }
    }
}
"#,
                Language::CSharp,
                "f:test",
                "Test.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got: {:?}", names);
        assert!(names.contains(&"GetItems"), "got: {:?}", names);
        assert!(names.contains(&"ICallback"), "got: {:?}", names);
        assert_eq!(result.imports.len(), 2);
    }

    #[test]
    fn kotlin_symbols_and_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.List
import kotlin.collections.Map

class UserService(private val name: String) {
    fun getItems(): List<String> {
        return listOf()
    }

    interface Callback {
        fun onResult(result: String)
    }

    enum class Status {
        ACTIVE, INACTIVE
    }
}

fun topLevel(): String = "hello"
"#,
                Language::Kotlin,
                "f:test",
                "Test.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserService"), "got: {:?}", names);
        assert!(names.contains(&"getItems"), "got: {:?}", names);
        assert!(names.contains(&"topLevel"), "got: {:?}", names);
        assert_eq!(result.imports.len(), 2);
    }

    // ── Java advanced ──

    #[test]
    fn java_annotations_and_generics() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.List;
import static java.util.Collections.emptyList;

@Service
public class OrderService<T extends Comparable<T>> {
    @Autowired
    private final OrderRepository repo;

    @Override
    public List<T> findAll() {
        return emptyList();
    }

    public static <U> U identity(U value) {
        return value;
    }
}
"#,
                Language::Java,
                "f:test",
                "OrderService.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"OrderService"),
            "missing OrderService: {:?}",
            names
        );
        assert!(names.contains(&"findAll"), "missing findAll: {:?}", names);
        assert!(names.contains(&"identity"), "missing identity: {:?}", names);
        assert!(names.contains(&"repo"), "missing field repo: {:?}", names);
        // static import
        assert!(result
            .imports
            .iter()
            .any(|i| i.path.contains("Collections")));
    }

    #[test]
    fn java_abstract_and_inner_classes() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
public abstract class BaseHandler {
    public abstract void handle(String input);

    protected void log(String msg) {
        System.out.println(msg);
    }

    public static class Config {
        private int timeout;
    }

    private enum Priority {
        LOW, MEDIUM, HIGH
    }
}
"#,
                Language::Java,
                "f:test",
                "BaseHandler.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"BaseHandler"), "got: {:?}", names);
        assert!(names.contains(&"handle"), "got: {:?}", names);
        assert!(names.contains(&"log"), "got: {:?}", names);
        assert!(
            names.contains(&"Config"),
            "missing inner class: {:?}",
            names
        );
        assert!(
            names.contains(&"Priority"),
            "missing inner enum: {:?}",
            names
        );

        // Check parent relationships
        let config = result.symbols.iter().find(|s| s.name == "Config").unwrap();
        assert!(
            config.parent_symbol_id.is_some(),
            "Config should have parent"
        );
    }

    #[test]
    fn java_interface_with_default_methods() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
public interface Repository<T, ID> {
    T findById(ID id);
    List<T> findAll();

    default void delete(ID id) {
        // default impl
    }
}
"#,
                Language::Java,
                "f:test",
                "Repository.java",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Repository"), "got: {:?}", names);
        assert!(names.contains(&"findById"), "got: {:?}", names);
        assert!(names.contains(&"findAll"), "got: {:?}", names);
        assert!(names.contains(&"delete"), "got: {:?}", names);

        let repo = result
            .symbols
            .iter()
            .find(|s| s.name == "Repository")
            .unwrap();
        assert_eq!(repo.kind, SymbolKind::Interface);
        assert!(repo.exported);
    }

    // ── C# advanced ──

    #[test]
    fn csharp_record_and_file_scoped_namespace() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System;

namespace MyApp;

public record UserDto(string Name, int Age);

public class UserMapper {
    public UserDto ToDto(User user) {
        return new UserDto(user.Name, user.Age);
    }
}
"#,
                Language::CSharp,
                "f:test",
                "UserDto.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"UserDto"), "missing record: {:?}", names);
        assert!(names.contains(&"UserMapper"), "missing class: {:?}", names);
        assert!(names.contains(&"ToDto"), "missing method: {:?}", names);
        assert_eq!(result.imports.len(), 1);
    }

    #[test]
    fn csharp_struct_enum_and_properties() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System.Collections.Generic;

namespace MyApp {
    public struct Point {
        public int X { get; set; }
        public int Y { get; set; }
    }

    public enum Color {
        Red,
        Green,
        Blue
    }

    public class Canvas {
        public List<Point> Points { get; } = new();
        public Color Background { get; set; }

        public void Draw() { }
    }
}
"#,
                Language::CSharp,
                "f:test",
                "Canvas.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"Point"), "missing struct: {:?}", names);
        assert!(names.contains(&"Color"), "missing enum: {:?}", names);
        assert!(names.contains(&"Canvas"), "missing class: {:?}", names);
        assert!(names.contains(&"Draw"), "missing method: {:?}", names);
        assert!(names.contains(&"X"), "missing property X: {:?}", names);

        let point = result.symbols.iter().find(|s| s.name == "Point").unwrap();
        assert_eq!(point.kind, SymbolKind::Struct);
    }

    #[test]
    fn csharp_async_and_generics() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System.Threading.Tasks;

namespace MyApp {
    public interface IRepository<T> where T : class {
        Task<T?> FindByIdAsync(int id);
        Task<IEnumerable<T>> GetAllAsync();
    }

    public class UserRepository : IRepository<User> {
        public async Task<User?> FindByIdAsync(int id) {
            return await _db.FindAsync(id);
        }

        public async Task<IEnumerable<User>> GetAllAsync() {
            return await _db.ToListAsync();
        }
    }
}
"#,
                Language::CSharp,
                "f:test",
                "UserRepository.cs",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"IRepository"),
            "missing interface: {:?}",
            names
        );
        assert!(
            names.contains(&"UserRepository"),
            "missing class: {:?}",
            names
        );
        assert!(
            names.contains(&"FindByIdAsync"),
            "missing async method: {:?}",
            names
        );

        let iface = result
            .symbols
            .iter()
            .find(|s| s.name == "IRepository")
            .unwrap();
        assert_eq!(iface.kind, SymbolKind::Interface);
    }

    // ── Kotlin advanced ──

    #[test]
    fn kotlin_data_class_and_companion() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.UUID

data class User(val name: String, val age: Int) {
    companion object {
        fun create(name: String): User = User(name, 0)
    }
}

sealed class Result<out T> {
    data class Success<T>(val data: T) : Result<T>()
    data class Error(val message: String) : Result<Nothing>()
}
"#,
                Language::Kotlin,
                "f:test",
                "User.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"User"), "missing data class: {:?}", names);
        assert!(
            names.contains(&"Result"),
            "missing sealed class: {:?}",
            names
        );
        assert!(
            names.contains(&"Success"),
            "missing inner data class: {:?}",
            names
        );
        assert!(
            names.contains(&"Error"),
            "missing inner data class: {:?}",
            names
        );
        assert_eq!(result.imports.len(), 1);
    }

    #[test]
    fn kotlin_object_and_extension_functions() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import io.ktor.server.application.*

object AppConfig {
    val defaultPort: Int = 8080
    fun load(): AppConfig = this
}

fun String.toSlug(): String = this.lowercase().replace(" ", "-")

suspend fun fetchData(url: String): ByteArray {
    return byteArrayOf()
}

internal class HttpClient {
    suspend fun get(url: String): String = ""
}
"#,
                Language::Kotlin,
                "f:test",
                "Utils.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"AppConfig"), "missing object: {:?}", names);
        assert!(
            names.contains(&"load"),
            "missing object method: {:?}",
            names
        );
        assert!(
            names.contains(&"toSlug"),
            "missing extension fn: {:?}",
            names
        );
        assert!(
            names.contains(&"fetchData"),
            "missing suspend fn: {:?}",
            names
        );
        assert!(
            names.contains(&"HttpClient"),
            "missing internal class: {:?}",
            names
        );

        let client = result
            .symbols
            .iter()
            .find(|s| s.name == "HttpClient")
            .unwrap();
        assert!(!client.exported, "internal class should not be exported");
    }

    #[test]
    fn kotlin_interface_and_enum_class() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
interface Repository<T> {
    fun findById(id: Long): T?
    fun findAll(): List<T>
    fun save(entity: T): T
}

enum class HttpMethod {
    GET, POST, PUT, DELETE;

    fun isIdempotent(): Boolean = this != POST
}
"#,
                Language::Kotlin,
                "f:test",
                "Repository.kt",
            )
            .unwrap();
        let names: Vec<&str> = result.symbols.iter().map(|s| s.name.as_str()).collect();
        assert!(
            names.contains(&"Repository"),
            "missing interface: {:?}",
            names
        );
        assert!(names.contains(&"findById"), "missing method: {:?}", names);
        assert!(
            names.contains(&"HttpMethod"),
            "missing enum class: {:?}",
            names
        );

        let repo = result
            .symbols
            .iter()
            .find(|s| s.name == "Repository")
            .unwrap();
        assert_eq!(repo.kind, SymbolKind::Interface);
        let http = result
            .symbols
            .iter()
            .find(|s| s.name == "HttpMethod")
            .unwrap();
        assert_eq!(http.kind, SymbolKind::Enum);
    }

    // ── Visibility tests ──

    #[test]
    fn java_visibility_detection() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
public class Api {
    public void publicMethod() {}
    private void privateMethod() {}
    protected void protectedMethod() {}
    void packageMethod() {}
}
"#,
                Language::Java,
                "f:test",
                "Api.java",
            )
            .unwrap();
        let pub_m = result
            .symbols
            .iter()
            .find(|s| s.name == "publicMethod")
            .unwrap();
        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "privateMethod")
            .unwrap();
        let prot_m = result
            .symbols
            .iter()
            .find(|s| s.name == "protectedMethod")
            .unwrap();
        let pkg_m = result
            .symbols
            .iter()
            .find(|s| s.name == "packageMethod")
            .unwrap();
        assert!(pub_m.exported);
        assert!(!priv_m.exported);
        assert!(!prot_m.exported);
        assert!(!pkg_m.exported); // package-private not exported
    }

    #[test]
    fn csharp_visibility_detection() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
namespace Test {
    public class Api {
        public void PublicMethod() {}
        private void PrivateMethod() {}
        internal void InternalMethod() {}
    }
}
"#,
                Language::CSharp,
                "f:test",
                "Api.cs",
            )
            .unwrap();
        let pub_m = result
            .symbols
            .iter()
            .find(|s| s.name == "PublicMethod")
            .unwrap();
        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "PrivateMethod")
            .unwrap();
        let int_m = result
            .symbols
            .iter()
            .find(|s| s.name == "InternalMethod")
            .unwrap();
        assert!(pub_m.exported);
        assert!(!priv_m.exported);
        assert!(!int_m.exported);
    }

    #[test]
    fn kotlin_visibility_detection() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
class Api {
    fun defaultMethod() {}
    private fun privateMethod() {}
    internal fun internalMethod() {}
    protected fun protectedMethod() {}
}
"#,
                Language::Kotlin,
                "f:test",
                "Api.kt",
            )
            .unwrap();
        let def_m = result
            .symbols
            .iter()
            .find(|s| s.name == "defaultMethod")
            .unwrap();
        let priv_m = result
            .symbols
            .iter()
            .find(|s| s.name == "privateMethod")
            .unwrap();
        let int_m = result
            .symbols
            .iter()
            .find(|s| s.name == "internalMethod")
            .unwrap();
        assert!(def_m.exported, "Kotlin default is public");
        assert!(!priv_m.exported);
        assert!(!int_m.exported);
    }

    // ── Import extraction edge cases ──

    #[test]
    fn java_static_and_wildcard_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import java.util.*;
import static org.junit.Assert.assertEquals;
import com.example.model.User;

public class Test {}
"#,
                Language::Java,
                "f:test",
                "Test.java",
            )
            .unwrap();
        assert_eq!(result.imports.len(), 3);
        let wildcard = result
            .imports
            .iter()
            .find(|i| i.path.contains("java.util"))
            .unwrap();
        assert!(
            wildcard.names.is_empty(),
            "wildcard import should have no names"
        );
        let static_imp = result
            .imports
            .iter()
            .find(|i| i.path.contains("assertEquals"))
            .unwrap();
        assert!(static_imp.names.contains(&"assertEquals".to_string()));
    }

    #[test]
    fn csharp_using_variations() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
using System;
using System.Collections.Generic;
using Alias = System.Text.StringBuilder;
using static System.Math;

namespace Test {
    public class Foo {}
}
"#,
                Language::CSharp,
                "f:test",
                "Foo.cs",
            )
            .unwrap();
        // Alias should be skipped (contains '=')
        assert_eq!(
            result.imports.len(),
            3,
            "got: {:?}",
            result.imports.iter().map(|i| &i.path).collect::<Vec<_>>()
        );
    }

    #[test]
    fn kotlin_star_imports() {
        let mut parser = Parser::new();
        let result = parser
            .parse(
                r#"
import io.ktor.server.application.*
import io.ktor.server.routing.Routing
import kotlinx.coroutines.flow.Flow

class App
"#,
                Language::Kotlin,
                "f:test",
                "App.kt",
            )
            .unwrap();
        assert_eq!(
            result.imports.len(),
            3,
            "got: {:?}",
            result.imports.iter().map(|i| &i.path).collect::<Vec<_>>()
        );
    }
}
