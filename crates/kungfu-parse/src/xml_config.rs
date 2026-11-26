//! XML config symbols: elements two levels deep as `ConfigBlock`s (name = tag path). Follows the `config_symbols` contract;
//! `json_config` is the reference.
//!
//! The root element is level 1 and its child elements level 2; siblings that
//! share a tag are one `ConfigKey` listing how many there are. Attributes,
//! comments, processing instructions and the prolog are not symbols.

use crate::config_symbols::{node_span, ConfigParent, ConfigSymbols, ConfigValue};
use kungfu_types::symbol::SymbolKind;
use std::collections::HashMap;
use tree_sitter::Node;

/// Index the root element and its direct child elements.
pub fn extract(root: Node, source: &str, out: &mut ConfigSymbols) {
    let mut cursor = root.walk();
    let Some(element) = root
        .named_children(&mut cursor)
        .find(|c| c.kind() == "element")
    else {
        return;
    };
    let name = tag_name(element, source);
    let children = child_elements(element);
    let Some(handle) = push_element(element, name, None, source, out) else {
        return;
    };
    // Siblings with one tag collapse into the first of them.
    let mut groups: Vec<(&str, Vec<Node>)> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for child in children {
        let tag = tag_name(child, source);
        match index.get(tag) {
            Some(&at) => groups[at].1.push(child),
            None => {
                index.insert(tag, groups.len());
                groups.push((tag, vec![child]));
            }
        }
    }
    for (tag, members) in groups {
        if members.len() > 1 {
            let list = ConfigValue::List {
                count: members.len(),
                noun: "element",
            };
            out.push(
                Some(&handle),
                tag,
                SymbolKind::ConfigKey,
                list,
                node_span(&members[0]),
            );
        } else {
            push_element(members[0], tag, Some(&handle), source, out);
        }
    }
}

/// A block when the element has child elements, else its text, else nothing.
fn push_element(
    element: Node,
    name: &str,
    parent: Option<&ConfigParent>,
    source: &str,
    out: &mut ConfigSymbols,
) -> Option<ConfigParent> {
    let count = child_elements(element).len();
    let text;
    let (kind, value) = if count > 0 {
        (
            SymbolKind::ConfigBlock,
            ConfigValue::Block {
                count,
                noun: "element",
            },
        )
    } else {
        text = leaf_text(element, source);
        if text.is_empty() {
            (SymbolKind::ConfigKey, ConfigValue::None)
        } else {
            (SymbolKind::ConfigKey, ConfigValue::Scalar(&text))
        }
    };
    out.push(parent, name, kind, value, node_span(&element))
}

/// The tag with its namespace prefix, as written.
fn tag_name<'s>(element: Node, source: &'s str) -> &'s str {
    let mut cursor = element.walk();
    let tag = element
        .named_children(&mut cursor)
        .find(|c| matches!(c.kind(), "STag" | "EmptyElemTag"));
    let Some(tag) = tag else {
        return "";
    };
    let mut inner = tag.walk();
    let name = tag
        .named_children(&mut inner)
        .find(|c| c.kind() == "Name")
        .and_then(|n| source.get(n.byte_range()))
        .unwrap_or("");
    name
}

fn content(element: Node) -> Option<Node> {
    let mut cursor = element.walk();
    let found = element
        .named_children(&mut cursor)
        .find(|c| c.kind() == "content");
    found
}

fn child_elements(element: Node) -> Vec<Node> {
    let Some(content) = content(element) else {
        return Vec::new();
    };
    let mut cursor = content.walk();
    let children = content
        .named_children(&mut cursor)
        .filter(|c| c.kind() == "element")
        .collect();
    children
}

/// Character data of a leaf element, trimmed, comments and processing
/// instructions left out.
fn leaf_text(element: Node, source: &str) -> String {
    let Some(content) = content(element) else {
        return String::new();
    };
    let mut cursor = content.walk();
    let mut text = String::new();
    for part in content.named_children(&mut cursor) {
        if !matches!(part.kind(), "Comment" | "PI") {
            text.push_str(source.get(part.byte_range()).unwrap_or(""));
        }
    }
    text.trim().to_string()
}

#[cfg(test)]
mod tests {
    use crate::{ConfigParseResult, Parser};
    use kungfu_types::file::Language;
    use kungfu_types::symbol::{Symbol, SymbolKind};

    fn parse(src: &str, limit: usize) -> ConfigParseResult {
        Parser::new()
            .parse_config(src, Language::Xml, "f:1", "pom.xml", limit)
            .unwrap()
    }

    fn names(r: &ConfigParseResult) -> Vec<&str> {
        r.symbols.iter().map(|s| s.name.as_str()).collect()
    }

    fn find<'r>(r: &'r ConfigParseResult, name: &str) -> &'r Symbol {
        r.symbols
            .iter()
            .find(|s| s.name == name)
            .unwrap_or_else(|| panic!("no symbol {name} in {:?}", names(r)))
    }

    fn sig<'r>(r: &'r ConfigParseResult, name: &str) -> &'r str {
        find(r, name).signature.as_deref().unwrap()
    }

    fn lines(r: &ConfigParseResult, name: &str) -> (usize, usize) {
        let s = find(r, name);
        (s.span.start_line, s.span.end_line)
    }

    #[test]
    fn pom_shape() {
        let r = parse(include_str!("../tests/fixtures/config/pom.xml"), 100);
        assert_eq!(
            names(&r),
            [
                "project",
                "project.modelVersion",
                "project.groupId",
                "project.artifactId",
                "project.version",
                "project.properties",
                "project.dependencies",
                "project.build",
            ]
        );
        assert_eq!(r.total, r.symbols.len());
        assert!(!r.has_errors);
        assert_eq!(sig(&r, "project"), "project = {7 elements}");
        assert_eq!(find(&r, "project").kind, SymbolKind::ConfigBlock);
        assert_eq!(sig(&r, "project.groupId"), "project.groupId = com.example");
        assert_eq!(find(&r, "project.groupId").kind, SymbolKind::ConfigKey);
        assert_eq!(
            find(&r, "project.groupId").parent_symbol_id.as_deref(),
            Some(find(&r, "project").id.as_str())
        );
        // An element with children is a block; its children are level 3.
        assert_eq!(
            sig(&r, "project.properties"),
            "project.properties = {2 elements}"
        );
        assert_eq!(find(&r, "project.properties").kind, SymbolKind::ConfigBlock);
        assert_eq!(
            sig(&r, "project.dependencies"),
            "project.dependencies = {2 elements}"
        );
        // An empty element has nothing to show.
        assert_eq!(sig(&r, "project.build"), "project.build");
        assert_eq!(find(&r, "project.build").kind, SymbolKind::ConfigKey);
        // 1-based lines of the whole element.
        assert_eq!(lines(&r, "project"), (3, 23));
        assert_eq!(lines(&r, "project.modelVersion"), (4, 4));
        assert_eq!(lines(&r, "project.properties"), (8, 11));
        assert_eq!(lines(&r, "project.dependencies"), (12, 21));
        assert_eq!(find(&r, "project.groupId").id, "s:f:1:5:project.groupId");
    }

    #[test]
    fn android_manifest_collapses_repeated_siblings() {
        let r = parse(
            include_str!("../tests/fixtures/config/AndroidManifest.xml"),
            100,
        );
        assert_eq!(
            names(&r),
            [
                "manifest",
                "manifest.uses-permission",
                "manifest.application"
            ]
        );
        assert_eq!(sig(&r, "manifest"), "manifest = {3 elements}");
        let perm = find(&r, "manifest.uses-permission");
        assert_eq!(
            perm.signature.as_deref(),
            Some("manifest.uses-permission = [2 elements]")
        );
        assert_eq!(perm.kind, SymbolKind::ConfigKey);
        // Collapsed at the first occurrence.
        assert_eq!((perm.span.start_line, perm.span.end_line), (3, 3));
        assert_eq!(
            sig(&r, "manifest.application"),
            "manifest.application = {2 elements}"
        );
        assert_eq!(lines(&r, "manifest.application"), (5, 8));
    }

    #[test]
    fn csproj_shape_with_non_adjacent_siblings() {
        let src = "<Project Sdk=\"Microsoft.NET.Sdk\">\n  <PropertyGroup>\n    <TargetFramework>net8.0</TargetFramework>\n  </PropertyGroup>\n  <ItemGroup>\n    <PackageReference Include=\"A\" />\n  </ItemGroup>\n  <Target Name=\"x\"/>\n  <ItemGroup>\n    <Compile Include=\"b.cs\" />\n  </ItemGroup>\n</Project>\n";
        let r = parse(src, 50);
        assert_eq!(
            names(&r),
            [
                "Project",
                "Project.PropertyGroup",
                "Project.ItemGroup",
                "Project.Target"
            ]
        );
        assert_eq!(sig(&r, "Project"), "Project = {4 elements}");
        assert_eq!(
            sig(&r, "Project.ItemGroup"),
            "Project.ItemGroup = [2 elements]"
        );
        assert_eq!(sig(&r, "Project.Target"), "Project.Target");
        assert_eq!(lines(&r, "Project.ItemGroup"), (5, 7));
    }

    #[test]
    fn namespaced_tags_keep_their_prefix() {
        let r = parse(
            "<ns:root xmlns:ns=\"urn:x\"><ns:a>1</ns:a><b:c/><ns:a>2</ns:a></ns:root>",
            50,
        );
        assert_eq!(names(&r), ["ns:root", "ns:root.ns:a", "ns:root.b:c"]);
        assert_eq!(sig(&r, "ns:root.ns:a"), "ns:root.ns:a = [2 elements]");
    }

    #[test]
    fn text_ignores_comments_attributes_and_whitespace() {
        let r = parse(
            "<?xml version=\"1.0\"?>\n<!DOCTYPE r>\n<!-- c -->\n<r a=\"1\">\n<x a=\"2\">\n  hello &amp; bye <!-- note -->\n</x>\n<y><!-- only a comment --></y>\n<z>   </z>\n<w><![CDATA[raw]]></w>\n</r>\n",
            50,
        );
        assert_eq!(names(&r), ["r", "r.x", "r.y", "r.z", "r.w"]);
        assert_eq!(sig(&r, "r.x"), "r.x = hello &amp; bye");
        assert_eq!(sig(&r, "r.y"), "r.y");
        assert_eq!(sig(&r, "r.z"), "r.z");
        assert_eq!(sig(&r, "r.w"), "r.w = <![CDATA[raw]]>");
        assert_eq!(lines(&r, "r"), (4, 11));
    }

    #[test]
    fn root_with_text_only_or_nothing() {
        let r = parse("<a>text</a>", 50);
        assert_eq!(sig(&r, "a"), "a = text");
        let r = parse("<a/>", 50);
        assert_eq!(sig(&r, "a"), "a");
        assert!(parse("", 50).symbols.is_empty());
        assert!(parse("<?xml version=\"1.0\"?>", 50).symbols.is_empty());
    }

    #[test]
    fn secrets_are_redacted() {
        let r = parse(
            "<settings><password>hunter2</password><server><id>x</id></server><url>http://u:p@h/x</url><name>ok</name></settings>",
            50,
        );
        assert_eq!(
            sig(&r, "settings.password"),
            "settings.password = <redacted>"
        );
        assert_eq!(sig(&r, "settings.url"), "settings.url = <redacted>");
        assert_eq!(sig(&r, "settings.name"), "settings.name = ok");
    }

    #[test]
    fn malformed_xml_does_not_panic_and_yields_what_it_can() {
        let r = parse("<a><b x=>1</b><c>2</c></a>", 50);
        assert!(r.has_errors);
        assert_eq!(names(&r), ["a", "a.b", "a.c"]);
        // An unclosed tag can drop the root; whatever survives is flagged.
        assert!(parse("<a><b>1</b><c>2</c>", 50).has_errors);
        assert!(parse("<a><b>1</b><c>2</a>", 50).has_errors);
        for src in [
            "<",
            "<a",
            "<a><b>",
            "</a>",
            "<a></b>",
            "<<>>",
            "<a b=></a>",
            "text only",
        ] {
            parse(src, 50);
        }
    }

    #[test]
    fn many_distinct_tags_and_deep_nesting_stay_linear() {
        let n = 100_000;
        let wide: String = (0..n).map(|i| format!("<t{i}/>")).collect();
        let r = parse(&format!("<r>{wide}</r>"), 50);
        assert_eq!(r.total, n + 1);
        assert_eq!(r.symbols.len(), 50);
        // The grammar itself is superlinear in nesting depth, so keep this modest.
        let deep = format!("{}x{}", "<a>".repeat(300), "</a>".repeat(300));
        let r = parse(&deep, 50);
        assert_eq!(names(&r), ["a", "a.a"]);
    }

    #[test]
    fn limit_trims_but_total_counts_everything() {
        let r = parse("<r><a><x/></a><b/><c/></r>", 2);
        assert_eq!(names(&r), ["r", "r.a"]);
        assert_eq!(r.total, 4);
    }
}
