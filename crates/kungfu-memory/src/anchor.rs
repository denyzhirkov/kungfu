/// Extract keyword anchors from text for memory matching.
///
/// Splits on non-alphanumeric boundaries, filters stop words,
/// detects identifiers (snake_case, CamelCase, backtick-quoted).
pub fn extract_anchors(text: &str) -> Vec<String> {
    let mut anchors = Vec::new();

    // Extract backtick-quoted identifiers first
    let mut rest = text;
    while let Some(start) = rest.find('`') {
        rest = &rest[start + 1..];
        if let Some(end) = rest.find('`') {
            let ident = &rest[..end];
            if ident.len() >= 2
                && ident
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.')
            {
                anchors.push(ident.to_lowercase());
            }
            rest = &rest[end + 1..];
        } else {
            break;
        }
    }

    // Split remaining text into words
    for word in text.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.len() < 3 {
            continue;
        }
        let lower = word.to_lowercase();
        if is_stop_word(&lower) {
            continue;
        }

        // Split CamelCase into parts and add both whole and parts
        let parts = split_camel_case(word);
        if parts.len() > 1 {
            anchors.push(lower.clone());
            for part in &parts {
                let p = part.to_lowercase();
                if p.len() >= 3 && !is_stop_word(&p) {
                    anchors.push(p);
                }
            }
        } else {
            anchors.push(lower);
        }
    }

    anchors.sort();
    anchors.dedup();
    anchors
}

fn split_camel_case(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for c in s.chars() {
        if c.is_uppercase() && !current.is_empty() {
            parts.push(std::mem::take(&mut current));
        }
        current.push(c);
    }
    if !current.is_empty() {
        parts.push(current);
    }
    parts
}

fn is_stop_word(w: &str) -> bool {
    matches!(
        w,
        "the"
            | "a"
            | "an"
            | "is"
            | "are"
            | "was"
            | "were"
            | "be"
            | "been"
            | "being"
            | "have"
            | "has"
            | "had"
            | "do"
            | "does"
            | "did"
            | "will"
            | "would"
            | "shall"
            | "should"
            | "may"
            | "might"
            | "must"
            | "can"
            | "could"
            | "this"
            | "that"
            | "these"
            | "those"
            | "it"
            | "its"
            | "of"
            | "in"
            | "to"
            | "for"
            | "with"
            | "on"
            | "at"
            | "by"
            | "from"
            | "as"
            | "into"
            | "about"
            | "not"
            | "no"
            | "but"
            | "or"
            | "and"
            | "if"
            | "then"
            | "else"
            | "when"
            | "while"
            | "all"
            | "each"
            | "every"
            | "both"
            | "use"
            | "using"
            | "used"
            | "also"
            | "which"
            | "who"
            | "what"
            | "how"
            | "why"
            | "where"
            | "there"
            | "here"
            | "some"
            | "any"
            | "other"
            | "more"
            | "most"
            | "such"
            | "only"
            | "own"
            | "same"
            | "than"
            | "too"
            | "very"
            | "just"
            | "because"
            | "through"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_basic_words() {
        let anchors = extract_anchors("validate user input before processing");
        assert!(anchors.contains(&"validate".to_string()));
        assert!(anchors.contains(&"user".to_string()));
        assert!(anchors.contains(&"input".to_string()));
        assert!(anchors.contains(&"processing".to_string()));
        assert!(anchors.contains(&"before".to_string())); // not a stop word
    }

    #[test]
    fn extracts_backtick_identifiers() {
        let anchors = extract_anchors("The `KungfuService` handles all requests");
        assert!(anchors.contains(&"kungfuservice".to_string()));
    }

    #[test]
    fn splits_camel_case() {
        let anchors = extract_anchors("AuthService handles validation");
        assert!(anchors.contains(&"authservice".to_string()));
        assert!(anchors.contains(&"auth".to_string()));
        assert!(anchors.contains(&"service".to_string()));
    }

    #[test]
    fn filters_short_and_stop_words() {
        let anchors = extract_anchors("it is a test of the system");
        assert!(!anchors.contains(&"it".to_string()));
        assert!(!anchors.contains(&"is".to_string()));
        assert!(!anchors.contains(&"the".to_string()));
        assert!(anchors.contains(&"test".to_string()));
        assert!(anchors.contains(&"system".to_string()));
    }

    #[test]
    fn deduplicates() {
        let anchors = extract_anchors("parser parser parser");
        assert_eq!(anchors.iter().filter(|a| *a == "parser").count(), 1);
    }
}
