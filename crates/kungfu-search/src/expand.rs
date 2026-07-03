//! Candidate-level query expansion: identifier splitting, light stemming,
//! and a curated abbreviation/synonym table.
//!
//! All expansion happens on the *query* side, once per query
//! ([`ExpandedQuery::new`]). Per-symbol work stays O(name length): split the
//! symbol name into words and compare against the precomputed token variants.
//!
//! Match strengths are graded so exact matches always outrank expanded ones:
//! exact (1.0) > stem (0.95) > synonym (0.72).

/// Match strength for a direct token hit (exact word or substring).
pub const STRENGTH_EXACT: f64 = 1.0;
/// Match strength for a stem-level hit ("migrations" ~ "migration").
pub const STRENGTH_STEM: f64 = 0.95;
/// Match strength for a synonym/abbreviation hit ("ws" ~ "websocket").
/// Deliberately penalized so exact vocabulary still ranks first.
pub const STRENGTH_SYNONYM: f64 = 0.72;

/// Hard cap on query tokens after splitting — bounded work invariant.
const MAX_TOKENS: usize = 10;
/// Hard cap on synonym variants per token.
const MAX_SYNONYMS: usize = 16;

/// Curated groups of interchangeable dev vocabulary. Every member of a group
/// expands to every other member. Entries were chosen from (a) the vocabulary
/// gaps in the failing benchmark cases (concept-recall / identifier-mismatch)
/// and (b) ubiquitous programming abbreviations, kept deliberately small so
/// expansion stays precise.
static SYNONYM_GROUPS: &[&[&str]] = &[
    // Networking / transport
    &["ws", "websocket", "websockets"],
    &["conn", "connection", "connect"],
    &[
        "mux",
        "multiplex",
        "multiplexer",
        "multiplexing",
        "poll",
        "epoll",
        "kqueue",
        "eventloop",
    ],
    &["req", "request"],
    &["res", "resp", "response"],
    &["recv", "receive", "receiver"],
    &["msg", "message"],
    &["pkt", "packet"],
    &["hdr", "header"],
    &["addr", "address"],
    &["url", "uri"],
    // Auth / security
    &[
        "auth",
        "authentication",
        "authenticate",
        "login",
        "signin",
        "credential",
    ],
    &[
        "authz",
        "authorization",
        "authorize",
        "permission",
        "perm",
        "acl",
    ],
    &["pwd", "passwd", "password"],
    &["cert", "certificate"],
    &["sig", "signature"],
    &["sess", "session"],
    // Configuration / environment
    &[
        "config",
        "configuration",
        "configs",
        "settings",
        "cfg",
        "conf",
        "options",
    ],
    &["env", "environment"],
    &[
        "init",
        "initialize",
        "initialise",
        "initialization",
        "setup",
        "bootstrap",
    ],
    // Storage / memory
    &["db", "database"],
    &["mem", "memory"],
    &["alloc", "allocate", "allocation", "allocator"],
    &["free", "release", "dealloc", "deallocate"],
    &["evict", "eviction", "expire", "expiration", "expiry", "ttl"],
    &["buf", "buffer"],
    &["tx", "transaction"],
    &["repo", "repository"],
    // Scheduling / execution
    &["sched", "scheduler", "schedule", "scheduling"],
    &["pending", "queued", "scheduled"],
    &["async", "asynchronous"],
    &["sync", "synchronize", "synchronization", "synchronous"],
    &["exec", "execute", "execution"],
    &["eval", "evaluate", "evaluation"],
    &["proc", "process"],
    &["cmd", "command"],
    &["choose", "select", "pick", "resolve", "resolution"],
    // Code structure
    &["fn", "func", "function"],
    &["arg", "argument", "param", "parameter"],
    &["var", "variable"],
    &["val", "value"],
    &["str", "string"],
    &["num", "number"],
    &["int", "integer"],
    &["bool", "boolean"],
    &["obj", "object"],
    &["arr", "array"],
    &["dict", "dictionary", "hashmap"],
    &["ctx", "context"],
    &["err", "error"],
    &["exc", "exception"],
    &["impl", "implement", "implementation"],
    &["decl", "declaration", "declare"],
    &["def", "definition", "define"],
    &["stmt", "statement"],
    &["expr", "expression"],
    &["attr", "attribute"],
    &["prop", "property", "properties"],
    &["elem", "element"],
    &["ev", "evt", "event"],
    &["cb", "callback"],
    &["iter", "iterator", "iteration"],
    &["idx", "index"],
    &["ref", "reference"],
    &["ptr", "pointer"],
    &["gen", "generate", "generator"],
    &["mw", "middleware"],
    &["io", "input", "output"],
    // Project layout
    &["dir", "directory", "folder"],
    &["doc", "document", "documentation"],
    &["src", "source"],
    &["dst", "dest", "destination"],
    &["dep", "dependency", "dependencies"],
    &["ver", "version"],
    &["pkg", "package"],
    &["lib", "library"],
    &["mod", "module"],
    &["svc", "service"],
    &["util", "utility", "utils", "helper", "helpers"],
    // Misc common
    &["len", "length"],
    &["max", "maximum"],
    &["min", "minimum"],
    &["tmp", "temp", "temporary"],
    &["prev", "previous"],
    &["cur", "curr", "current"],
    &["del", "delete", "remove"],
    &["fetch", "retrieve", "get"],
    &["sub", "subscribe", "subscription"],
    &["info", "information"],
    &["stats", "statistics"],
    &["calc", "calculate", "calculation"],
    &["regex", "regexp"],
];

/// Split an identifier into lowercase words on camelCase / PascalCase /
/// snake_case / kebab-case / digit boundaries. Acronym runs stay together:
/// `HTTPServer` -> ["http", "server"], `getUserByID` -> ["get", "user", "by", "id"].
pub fn split_identifier(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut cur = String::new();

    for i in 0..chars.len() {
        let c = chars[i];
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                words.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if !cur.is_empty() {
            let prev = chars[i - 1];
            let acronym_end = c.is_uppercase()
                && prev.is_uppercase()
                && chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let case_boundary = c.is_uppercase() && prev.is_lowercase();
            let digit_boundary = c.is_ascii_digit() != prev.is_ascii_digit();
            if case_boundary || acronym_end || digit_boundary {
                words.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        words.push(cur);
    }
    words
}

/// Light suffix-stripping stemmer. Returns candidate stems for a word,
/// most specific first, excluding the word itself. Empty when the word is
/// too short or no rule applies. This is the single stemming mechanism in
/// kungfu-search — [`crate::simple_stem`] is its first candidate.
pub fn stem_variants(word: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if word.len() < 4 || !word.is_ascii() {
        return out;
    }
    let push = |out: &mut Vec<String>, s: String| {
        if s.len() >= 3 && s != word && !out.contains(&s) {
            out.push(s);
        }
    };

    // Rules are checked longest-suffix-first; the first matching family wins
    // and may emit several candidates (e.g. "caching" -> "cach", "cache").
    if let Some(stem) = word
        .strip_suffix("ization")
        .or_else(|| word.strip_suffix("isation"))
    {
        push(&mut out, format!("{stem}ize"));
        push(&mut out, format!("{stem}e"));
        push(&mut out, stem.to_string());
    } else if let Some(stem) = word.strip_suffix("ication") {
        push(&mut out, format!("{stem}icate"));
        push(&mut out, format!("{stem}y"));
        push(&mut out, format!("{stem}ic"));
        push(&mut out, stem.to_string());
    } else if let Some(stem) = word.strip_suffix("ations") {
        // Plural of an -ation word: the singular is the primary stem
        push(&mut out, format!("{stem}ation"));
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
        push(&mut out, format!("{stem}ate"));
    } else if let Some(stem) = word.strip_suffix("ation") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
        push(&mut out, format!("{stem}ate"));
    } else if let Some(stem) = word.strip_suffix("ies") {
        push(&mut out, format!("{stem}y"));
        push(&mut out, stem.to_string());
    } else if let Some(stem) = word.strip_suffix("ied") {
        push(&mut out, format!("{stem}y"));
    } else if let Some(stem) = word.strip_suffix("tion") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}te"));
        push(&mut out, format!("{stem}e"));
    } else if let Some(stem) = word.strip_suffix("sion") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}t"));
        push(&mut out, format!("{stem}e"));
    } else if let Some(stem) = word.strip_suffix("ment") {
        push(&mut out, stem.to_string());
    } else if let Some(stem) = word.strip_suffix("ness") {
        push(&mut out, stem.to_string());
        if let Some(base) = stem.strip_suffix('i') {
            push(&mut out, format!("{base}y"));
        }
    } else if let Some(stem) = word.strip_suffix("ing") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
        if ends_with_double_consonant(stem) {
            push(&mut out, stem[..stem.len() - 1].to_string());
        }
    } else if word.ends_with("eed") {
        push(&mut out, word[..word.len() - 1].to_string());
    } else if let Some(stem) = word.strip_suffix("ed") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
        if ends_with_double_consonant(stem) {
            push(&mut out, stem[..stem.len() - 1].to_string());
        }
    } else if let Some(stem) = word
        .strip_suffix("ers")
        .or_else(|| word.strip_suffix("ors"))
    {
        // Plural of an agent noun: the singular is the primary stem
        push(
            &mut out,
            format!("{}{}", stem, &word[word.len() - 3..word.len() - 1]),
        );
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
        if ends_with_double_consonant(stem) {
            push(&mut out, stem[..stem.len() - 1].to_string());
        }
    } else if let Some(stem) = word.strip_suffix("er").or_else(|| word.strip_suffix("or")) {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
        if ends_with_double_consonant(stem) {
            push(&mut out, stem[..stem.len() - 1].to_string());
        }
    } else if let Some(stem) = word.strip_suffix("ity") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
    } else if let Some(stem) = word
        .strip_suffix("able")
        .or_else(|| word.strip_suffix("ible"))
    {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
    } else if let Some(stem) = word
        .strip_suffix("less")
        .or_else(|| word.strip_suffix("ful"))
        .or_else(|| word.strip_suffix("ous"))
        .or_else(|| word.strip_suffix("ive"))
        .or_else(|| word.strip_suffix("ize"))
        .or_else(|| word.strip_suffix("ise"))
        .or_else(|| word.strip_suffix("ly"))
        .or_else(|| word.strip_suffix("al"))
    {
        push(&mut out, stem.to_string());
    } else if let Some(stem) = word.strip_suffix("es") {
        push(&mut out, stem.to_string());
        push(&mut out, format!("{stem}e"));
    } else if word.ends_with('s')
        && !word.ends_with("ss")
        && !word.ends_with("us")
        && !word.ends_with("is")
    {
        push(&mut out, word[..word.len() - 1].to_string());
    }
    out
}

fn ends_with_double_consonant(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2
        && b[b.len() - 1] == b[b.len() - 2]
        && b[b.len() - 1].is_ascii_alphabetic()
        && !matches!(b[b.len() - 1], b'a' | b'e' | b'i' | b'o' | b'u')
}

/// Synonym/abbreviation variants for a token: all other members of every
/// group containing the token (or one of its stems).
fn synonyms_for(token: &str, stems: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for group in SYNONYM_GROUPS {
        let hit = group
            .iter()
            .any(|m| *m == token || stems.iter().any(|s| s == m));
        if !hit {
            continue;
        }
        for m in *group {
            if *m != token && !stems.iter().any(|s| s == m) && !out.iter().any(|e| e == m) {
                out.push((*m).to_string());
            }
        }
    }
    // Include stems of the synonyms so e.g. "pending" -> "scheduled" also
    // reaches "scheduler" at synonym strength.
    let base_len = out.len();
    for i in 0..base_len {
        let syn = out[i].clone();
        for sv in stem_variants(&syn) {
            if sv.len() >= 4 && sv != token && !out.contains(&sv) {
                out.push(sv);
            }
        }
    }
    out.truncate(MAX_SYNONYMS);
    out
}

/// Whole-word containment: `word` appears in `text` with non-alphanumeric
/// (or string-edge) boundaries on both sides.
fn contains_word(text: &str, word: &str) -> bool {
    let mut start = 0;
    while let Some(pos) = text[start..].find(word) {
        let abs = start + pos;
        let before_ok = abs == 0
            || !text[..abs]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after = abs + word.len();
        let after_ok = after >= text.len()
            || !text[after..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        start = abs + word.len().max(1);
    }
    false
}

/// One query token with its precomputed expansion variants.
pub struct ExpandedToken {
    /// The lowercase token as it appeared in the query (after splitting).
    pub exact: String,
    /// Stem variants of the token (matched at [`STRENGTH_STEM`]).
    stems: Vec<String>,
    /// Synonym/abbreviation variants (matched at [`STRENGTH_SYNONYM`]).
    synonyms: Vec<String>,
}

impl ExpandedToken {
    fn new(exact: String) -> Self {
        let mut stems = stem_variants(&exact);
        // short_root is part of the same stem tier: long words match their
        // abbreviated prefix ("authentication" -> "auth").
        if let Some(root) = crate::short_root(&exact) {
            if !stems.contains(&root) {
                stems.push(root);
            }
        }
        let synonyms = synonyms_for(&exact, &stems);
        Self {
            exact,
            stems,
            synonyms,
        }
    }

    /// Match this token against a symbol name (lowercased full text plus its
    /// split word set and per-word stems). Returns 0.0 or a strength.
    /// Short tokens (< 4 chars) only match whole words — "ws" must not hit
    /// the "ws" inside "Windows".
    pub fn match_in_name(
        &self,
        name_lower: &str,
        name_words: &[String],
        name_word_stems: &[Vec<String>],
    ) -> f64 {
        if name_words.contains(&self.exact)
            || (self.exact.len() >= 4 && name_lower.contains(self.exact.as_str()))
        {
            return STRENGTH_EXACT;
        }
        for v in &self.stems {
            if name_words.iter().any(|w| w == v)
                || (v.len() >= 4 && name_lower.contains(v.as_str()))
            {
                return STRENGTH_STEM;
            }
        }
        // Both ways: a stemmed name word equals the query token.
        if name_word_stems
            .iter()
            .any(|stems| stems.contains(&self.exact))
        {
            return STRENGTH_STEM;
        }
        for syn in &self.synonyms {
            if name_words.iter().any(|w| w == syn)
                || (syn.len() >= 4 && name_lower.contains(syn.as_str()))
            {
                return STRENGTH_SYNONYM;
            }
        }
        0.0
    }

    /// Match this token against free text (signature, path). Returns 0.0 or a strength.
    /// Short tokens (< 4 chars) require word boundaries in the text.
    pub fn match_in_text(&self, text: &str) -> f64 {
        let exact_hit = if self.exact.len() >= 4 {
            text.contains(self.exact.as_str())
        } else {
            contains_word(text, &self.exact)
        };
        if exact_hit {
            return STRENGTH_EXACT;
        }
        if self
            .stems
            .iter()
            .any(|v| v.len() >= 4 && text.contains(v.as_str()))
        {
            return STRENGTH_STEM;
        }
        if self
            .synonyms
            .iter()
            .any(|s| s.len() >= 4 && text.contains(s.as_str()))
        {
            return STRENGTH_SYNONYM;
        }
        0.0
    }

    /// Best synonym substring hit in a name, scaled by how much of the name it
    /// covers. Used by the single-word scorer as a fallback tier below stems.
    pub fn synonym_name_score(&self, name_lower: &str, name_words: &[String]) -> f64 {
        let mut best = 0.0f64;
        for syn in &self.synonyms {
            let hit = name_words.iter().any(|w| w == syn)
                || (syn.len() >= 4 && name_lower.contains(syn.as_str()));
            if hit {
                let coverage = (syn.len() as f64 / name_lower.len().max(1) as f64).min(1.0);
                best = best.max(0.3 + coverage * 0.35);
            }
        }
        best
    }
}

/// A query expanded once per search: whitespace words are further split on
/// identifier boundaries ("ask-context" -> "ask", "context"), then each token
/// gets stem + synonym variants.
pub struct ExpandedQuery {
    pub tokens: Vec<ExpandedToken>,
}

impl ExpandedQuery {
    pub fn new(words: &[&str]) -> Self {
        let mut tokens: Vec<ExpandedToken> = Vec::new();
        for word in words {
            for part in split_identifier(word) {
                if part.len() < 2 || part.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if tokens.iter().any(|t| t.exact == part) {
                    continue;
                }
                tokens.push(ExpandedToken::new(part));
                if tokens.len() >= MAX_TOKENS {
                    return Self { tokens };
                }
            }
        }
        if tokens.is_empty() {
            if let Some(first) = words.first() {
                tokens.push(ExpandedToken::new(first.to_lowercase()));
            }
        }
        Self { tokens }
    }

    /// The exact (post-split) token strings.
    pub fn exact_words(&self) -> Vec<&str> {
        self.tokens.iter().map(|t| t.exact.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- split_identifier ---

    #[test]
    fn split_camel_case() {
        assert_eq!(
            split_identifier("webSocketMiddleware"),
            ["web", "socket", "middleware"]
        );
    }

    #[test]
    fn split_pascal_case() {
        assert_eq!(
            split_identifier("WebSocketMiddleware"),
            ["web", "socket", "middleware"]
        );
    }

    #[test]
    fn split_snake_case() {
        assert_eq!(
            split_identifier("build_context_packet"),
            ["build", "context", "packet"]
        );
    }

    #[test]
    fn split_kebab_case() {
        assert_eq!(split_identifier("ask-context"), ["ask", "context"]);
    }

    #[test]
    fn split_acronym_run() {
        assert_eq!(split_identifier("HTTPServer"), ["http", "server"]);
        assert_eq!(split_identifier("getUserByID"), ["get", "user", "by", "id"]);
    }

    #[test]
    fn split_digits() {
        assert_eq!(split_identifier("utf8Parser"), ["utf", "8", "parser"]);
        assert_eq!(split_identifier("base64_encode"), ["base", "64", "encode"]);
    }

    #[test]
    fn split_empty_and_symbols() {
        assert!(split_identifier("").is_empty());
        assert!(split_identifier("__").is_empty());
    }

    // --- stem_variants ---

    #[test]
    fn stem_plural_s() {
        assert!(stem_variants("migrations").contains(&"migration".to_string()));
        assert!(stem_variants("keys").contains(&"key".to_string()));
        assert!(stem_variants("triggers").iter().any(|s| s == "trigger"));
    }

    #[test]
    fn stem_plural_guards() {
        // "ss"/"us"/"is" endings are not plurals
        assert!(!stem_variants("class").contains(&"clas".to_string()));
        assert!(stem_variants("basis").is_empty());
    }

    #[test]
    fn stem_ing_restores_e() {
        let v = stem_variants("caching");
        assert!(v.contains(&"cach".to_string()));
        assert!(v.contains(&"cache".to_string()));
    }

    #[test]
    fn stem_ed_and_eed() {
        assert!(stem_variants("dispatched").contains(&"dispatch".to_string()));
        assert!(stem_variants("freed").contains(&"free".to_string()));
        assert!(stem_variants("invoked").contains(&"invoke".to_string()));
    }

    #[test]
    fn stem_ied_to_y() {
        assert!(stem_variants("applied").contains(&"apply".to_string()));
    }

    #[test]
    fn stem_ies_to_y() {
        assert!(stem_variants("dependencies").contains(&"dependency".to_string()));
    }

    #[test]
    fn stem_ation_restores_e() {
        let v = stem_variants("expiration");
        assert!(v.contains(&"expire".to_string()), "{v:?}");
        let v2 = stem_variants("configuration");
        assert!(v2.contains(&"configure".to_string()), "{v2:?}");
    }

    #[test]
    fn stem_er_or() {
        assert!(stem_variants("scheduler").contains(&"schedule".to_string()));
        assert!(stem_variants("executor").contains(&"execute".to_string()));
        assert!(stem_variants("runner").contains(&"run".to_string()));
    }

    #[test]
    fn stem_doubled_consonant_ing() {
        assert!(stem_variants("running").contains(&"run".to_string()));
    }

    #[test]
    fn stem_short_words_skipped() {
        assert!(stem_variants("abc").is_empty());
        assert!(stem_variants("io").is_empty());
    }

    // --- synonyms ---

    #[test]
    fn synonyms_ws_to_websocket() {
        let tok = ExpandedToken::new("ws".to_string());
        assert!(tok.synonyms.iter().any(|s| s == "websocket"));
    }

    #[test]
    fn synonyms_bidirectional() {
        let tok = ExpandedToken::new("websocket".to_string());
        assert!(tok.synonyms.iter().any(|s| s == "ws"));
    }

    #[test]
    fn synonyms_via_stem_lookup() {
        // "connections" is not in the table, but its stem "connection" is;
        // the group is found and its remaining members become synonyms
        let tok = ExpandedToken::new("connections".to_string());
        assert!(
            tok.synonyms.iter().any(|s| s == "connect"),
            "{:?}",
            tok.synonyms
        );
        // "conn" itself is already reachable at stem strength via short_root
        let (lower, words, stems) = name_ctx("conn_pool");
        assert!(tok.match_in_name(&lower, &words, &stems) > 0.0);
    }

    #[test]
    fn synonyms_unknown_token_empty() {
        let tok = ExpandedToken::new("frobnicate".to_string());
        assert!(tok.synonyms.is_empty());
    }

    // --- match strengths / penalty ordering ---

    #[test]
    fn exact_beats_stem_beats_synonym() {
        const { assert!(STRENGTH_EXACT > STRENGTH_STEM) };
        const { assert!(STRENGTH_STEM > STRENGTH_SYNONYM) };
    }

    fn name_ctx(name: &str) -> (String, Vec<String>, Vec<Vec<String>>) {
        let lower = name.to_lowercase();
        let words = split_identifier(name);
        let stems = words.iter().map(|w| stem_variants(w)).collect();
        (lower, words, stems)
    }

    #[test]
    fn match_exact_word_in_split_name() {
        let (lower, words, stems) = name_ctx("WebSocketMiddleware");
        let tok = ExpandedToken::new("middleware".to_string());
        assert_eq!(tok.match_in_name(&lower, &words, &stems), STRENGTH_EXACT);
    }

    #[test]
    fn match_synonym_ws_in_websocket_name() {
        let (lower, words, stems) = name_ctx("WebSocketMiddleware");
        let tok = ExpandedToken::new("ws".to_string());
        assert_eq!(tok.match_in_name(&lower, &words, &stems), STRENGTH_SYNONYM);
    }

    #[test]
    fn match_stem_both_ways() {
        // query "migrations" vs name word "migration"
        let (lower, words, stems) = name_ctx("MigrationExecutor");
        let tok = ExpandedToken::new("migrations".to_string());
        assert_eq!(tok.match_in_name(&lower, &words, &stems), STRENGTH_STEM);

        // query "rank" vs name "ranking" (name-side stem)
        let (lower2, words2, stems2) = name_ctx("ranking");
        let tok2 = ExpandedToken::new("rank".to_string());
        assert!(tok2.match_in_name(&lower2, &words2, &stems2) >= STRENGTH_STEM);
    }

    #[test]
    fn short_token_needs_word_boundary() {
        // "ws" must not match the "ws" inside "Windows"
        let (lower, words, stems) = name_ctx("IsWindowsVersionIncompatible");
        let tok = ExpandedToken::new("ws".to_string());
        assert_eq!(tok.match_in_name(&lower, &words, &stems), 0.0);
        assert_eq!(tok.match_in_text("windowsversion"), 0.0);
        assert_eq!(tok.match_in_text("fn send(ws: socket)"), STRENGTH_EXACT);
    }

    #[test]
    fn contains_word_boundaries() {
        assert!(contains_word("src/ae.c", "ae"));
        assert!(contains_word("io_uring", "io"));
        assert!(!contains_word("station", "io"));
        assert!(!contains_word("windows", "ws"));
        assert!(contains_word("ws", "ws"));
    }

    #[test]
    fn match_no_relation_is_zero() {
        let (lower, words, stems) = name_ctx("KungfuService");
        let tok = ExpandedToken::new("database".to_string());
        assert_eq!(tok.match_in_name(&lower, &words, &stems), 0.0);
    }

    #[test]
    fn expanded_query_splits_hyphenated_words() {
        let q = ExpandedQuery::new(&["ask-context", "flow"]);
        assert_eq!(q.exact_words(), ["ask", "context", "flow"]);
    }

    #[test]
    fn expanded_query_drops_digits_and_dedups() {
        let q = ExpandedQuery::new(&["utf8", "utf8"]);
        assert_eq!(q.exact_words(), ["utf"]);
    }

    #[test]
    fn expanded_query_bounded() {
        let words: Vec<String> = (0..40).map(|i| format!("word{i}x")).collect();
        let refs: Vec<&str> = words.iter().map(|s| s.as_str()).collect();
        let q = ExpandedQuery::new(&refs);
        assert!(q.tokens.len() <= 10);
    }
}
