use kungfu_types::context::Intent;
use kungfu_types::file::FileEntry;
use std::collections::HashMap;

pub(crate) fn detect_primary_language(files: &[FileEntry]) -> Option<String> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for f in files {
        if let Some(ref lang) = f.language {
            if is_code_language(lang) {
                *counts.entry(lang.clone()).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(lang, _)| lang)
}

pub(crate) fn truncate_text(text: &str, max_len: usize) -> String {
    if text.chars().count() <= max_len {
        text.to_string()
    } else {
        let head: String = text.chars().take(max_len).collect();
        format!("{}...", head)
    }
}

pub(crate) fn is_code_language(lang: &str) -> bool {
    matches!(
        lang,
        "rust"
            | "typescript"
            | "javascript"
            | "python"
            | "go"
            | "java"
            | "csharp"
            | "kotlin"
            | "c"
            | "cpp"
    )
}

pub(crate) fn detect_intent(words: &[&str]) -> Intent {
    for w in words {
        match *w {
            "find" | "where" | "locate" | "show" | "get" | "lookup" | "search" => {
                return Intent::Lookup
            }
            "bug" | "fix" | "error" | "crash" | "broken" | "fail" | "debug" | "wrong" | "issue"
            | "panic" => return Intent::Debug,
            "how" | "explain" | "understand" | "what" | "why" | "does" | "works" | "overview" => {
                return Intent::Understand
            }
            "impact" | "affects" | "uses" | "calls" | "callers" | "consumers" | "depends"
            | "dependents" | "change" | "refactor" | "rename" | "remove" | "delete" => {
                return Intent::Impact
            }
            _ => {}
        }
    }
    Intent::Lookup
}

pub(crate) fn is_stop_word(word: &str) -> bool {
    matches!(
        word,
        // English stop words
        "the" | "a" | "an" | "is" | "are" | "was" | "were" | "in" | "on" | "at" | "to"
            | "for" | "of" | "with" | "by" | "from" | "and" | "or" | "not" | "it" | "this"
            | "that" | "be" | "has" | "have" | "do" | "does" | "did" | "will" | "would"
            | "could" | "should" | "can" | "may" | "i" | "me" | "my" | "we"
            // Intent trigger words (already captured by detect_intent, noise in search)
            | "find" | "where" | "locate" | "show" | "get" | "lookup" | "search"
            | "bug" | "fix" | "crash" | "broken" | "debug" | "wrong" | "issue"
            | "how" | "explain" | "understand" | "what" | "why" | "works" | "overview"
            | "impact" | "affects" | "uses" | "calls" | "callers" | "consumers"
            | "depends" | "dependents" | "change" | "refactor" | "rename"
            | "remove" | "delete" | "implemented" | "work" | "system" | "break"
            | "new" | "add" | "create" | "make" | "build" | "implement" | "support" | "need"
            | "want" | "like" | "also" | "just" | "all" | "every" | "each" | "some" | "any"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intent_lookup() {
        assert_eq!(detect_intent(&["find", "budget"]), Intent::Lookup);
        assert_eq!(detect_intent(&["where", "is", "config"]), Intent::Lookup);
        assert_eq!(detect_intent(&["show", "symbols"]), Intent::Lookup);
    }

    #[test]
    fn intent_debug() {
        assert_eq!(detect_intent(&["fix", "crash"]), Intent::Debug);
        assert_eq!(detect_intent(&["error", "parsing"]), Intent::Debug);
        assert_eq!(detect_intent(&["bug", "in", "indexer"]), Intent::Debug);
    }

    #[test]
    fn intent_understand() {
        assert_eq!(
            detect_intent(&["how", "does", "ranking"]),
            Intent::Understand
        );
        assert_eq!(detect_intent(&["explain", "budget"]), Intent::Understand);
        assert_eq!(
            detect_intent(&["what", "is", "context"]),
            Intent::Understand
        );
    }

    #[test]
    fn intent_impact() {
        assert_eq!(detect_intent(&["impact", "of", "change"]), Intent::Impact);
        assert_eq!(detect_intent(&["refactor", "budget"]), Intent::Impact);
        assert_eq!(detect_intent(&["rename", "symbol"]), Intent::Impact);
    }

    #[test]
    fn intent_default_is_lookup() {
        assert_eq!(detect_intent(&["foobar", "baz"]), Intent::Lookup);
    }

    #[test]
    fn stop_words_filtered() {
        assert!(is_stop_word("the"));
        assert!(is_stop_word("find"));
        assert!(is_stop_word("add"));
        assert!(!is_stop_word("budget"));
        assert!(!is_stop_word("parser"));
        assert!(!is_stop_word("language"));
    }

    #[test]
    fn primary_language_detection() {
        let files = vec![
            FileEntry {
                id: "1".into(),
                path: "a.rs".into(),
                extension: Some("rs".into()),
                language: Some("rust".into()),
                size: 100,
                hash: "h1".into(),
                indexed_at: Default::default(),
                tags: vec![],
            },
            FileEntry {
                id: "2".into(),
                path: "b.rs".into(),
                extension: Some("rs".into()),
                language: Some("rust".into()),
                size: 100,
                hash: "h2".into(),
                indexed_at: Default::default(),
                tags: vec![],
            },
            FileEntry {
                id: "3".into(),
                path: "c.py".into(),
                extension: Some("py".into()),
                language: Some("python".into()),
                size: 100,
                hash: "h3".into(),
                indexed_at: Default::default(),
                tags: vec![],
            },
        ];
        assert_eq!(detect_primary_language(&files), Some("rust".to_string()));
    }
}
