//! Stop-list of ubiquitous callable names, per language ecosystem.
//!
//! Names that a language's std/runtime provides on countless types (collection ops,
//! string ops, logging, error handling). Even if exactly one such symbol is indexed
//! in a project, a `.len()` / `list.append()` / `console.log()` call almost never
//! targets *that* one — attributing it would forge a high-weight edge. Dropping these
//! keeps the call graph precise at a small recall cost ("precision over recall").
//!
//! Kept as a single data table (`UBIQUITOUS`): name → bitmask of language groups the
//! name is noise in. Lookup is an O(1) map probe built once per process.

use std::collections::HashMap;
use std::sync::OnceLock;

type LangMask = u16;

const RUST: LangMask = 1 << 0;
/// TypeScript + JavaScript share one ecosystem.
const JS: LangMask = 1 << 1;
const PYTHON: LangMask = 1 << 2;
const GO: LangMask = 1 << 3;
/// Java + Kotlin share the JVM std surface.
const JVM: LangMask = 1 << 4;
const CSHARP: LangMask = 1 << 5;
/// C + C++ share libc; C++ adds the STL names.
const C_CPP: LangMask = 1 << 6;
/// Noise in (nearly) every ecosystem.
const ALL: LangMask = RUST | JS | PYTHON | GO | JVM | CSHARP | C_CPP;

/// Map an indexed file's language string (as stored on `FileEntry.language`)
/// to its stop-list group. Unknown/missing language matches every group —
/// filtering conservatively is the safer failure mode for precision.
fn mask_for_language(language: Option<&str>) -> LangMask {
    match language {
        Some("rust") => RUST,
        Some("typescript") | Some("javascript") => JS,
        Some("python") => PYTHON,
        Some("go") => GO,
        Some("java") | Some("kotlin") => JVM,
        Some("csharp") => CSHARP,
        Some("c") | Some("cpp") => C_CPP,
        _ => ALL,
    }
}

#[rustfmt::skip]
const UBIQUITOUS: &[(&str, LangMask)] = &[
    // -- Cross-ecosystem: constructors, conversion, collections, strings, iteration --
    ("new", ALL), ("default", ALL), ("clone", ALL), ("copy", ALL),
    ("len", ALL), ("size", ALL), ("count", ALL), ("is_empty", ALL), ("clear", ALL),
    ("get", ALL), ("set", ALL), ("add", ALL), ("insert", ALL), ("remove", ALL), ("delete", ALL),
    ("push", ALL), ("pop", ALL), ("append", ALL), ("extend", ALL), ("contains", ALL),
    ("keys", ALL), ("values", ALL), ("entries", ALL), ("items", ALL), ("entry", ALL),
    ("iter", ALL), ("next", ALL), ("map", ALL), ("filter", ALL), ("reduce", ALL),
    ("find", ALL), ("any", ALL), ("all", ALL), ("sort", ALL), ("reverse", ALL),
    ("first", ALL), ("last", ALL), ("take", ALL), ("skip", ALL), ("zip", ALL),
    ("min", ALL), ("max", ALL), ("sum", ALL), ("abs", ALL),
    ("join", ALL), ("split", ALL), ("replace", ALL), ("trim", ALL), ("format", ALL),
    ("parse", ALL), ("concat", ALL), ("slice", ALL), ("index", ALL),
    ("starts_with", ALL), ("ends_with", ALL),
    ("to_string", ALL), ("toString", ALL), ("from", ALL), ("into", ALL), ("of", ALL),
    ("read", ALL), ("write", ALL), ("open", ALL), ("close", ALL), ("flush", ALL),
    ("update", ALL), ("reset", ALL), ("cmp", ALL), ("equals", ALL), ("hash", ALL),
    // logging / diagnostics
    ("log", ALL), ("debug", ALL), ("info", ALL), ("warn", ALL), ("error", ALL),
    ("trace", ALL), ("print", ALL), ("assert", ALL), ("exit", ALL), ("abort", ALL),

    // -- Rust: Option/Result, Vec/HashMap/str, iterator adapters --
    ("to_owned", RUST), ("to_vec", RUST), ("try_from", RUST), ("try_into", RUST),
    ("as_str", RUST), ("as_ref", RUST), ("as_mut", RUST), ("as_bytes", RUST),
    ("as_deref", RUST), ("as_slice", RUST), ("deref", RUST), ("borrow", RUST),
    ("borrow_mut", RUST), ("iter_mut", RUST), ("into_iter", RUST),
    ("unwrap", RUST), ("unwrap_or", RUST), ("unwrap_or_else", RUST),
    ("unwrap_or_default", RUST), ("expect", RUST), ("ok", RUST), ("err", RUST),
    ("ok_or", RUST), ("ok_or_else", RUST), ("map_err", RUST), ("and_then", RUST),
    ("or_else", RUST), ("is_some", RUST), ("is_none", RUST), ("is_ok", RUST),
    ("is_err", RUST), ("collect", RUST), ("get_mut", RUST), ("contains_key", RUST),
    ("or_default", RUST), ("or_insert", RUST), ("or_insert_with", RUST),
    ("partial_cmp", RUST), ("fmt", RUST), ("cloned", RUST), ("copied", RUST),
    ("chain", RUST), ("rev", RUST), ("enumerate", RUST), ("position", RUST),
    ("nth", RUST), ("fold", RUST), ("for_each", RUST), ("filter_map", RUST),
    ("flat_map", RUST), ("flatten", RUST), ("take_while", RUST), ("skip_while", RUST),
    ("peekable", RUST), ("peek", RUST), ("drain", RUST), ("retain", RUST),
    ("dedup", RUST), ("sort_by", RUST), ("sort_by_key", RUST), ("sort_unstable", RUST),
    ("truncate", RUST), ("resize", RUST), ("reserve", RUST), ("split_off", RUST),
    ("chars", RUST), ("bytes", RUST), ("lines", RUST), ("splitn", RUST),
    ("split_whitespace", RUST), ("trim_start", RUST), ("trim_end", RUST),
    ("strip_prefix", RUST), ("strip_suffix", RUST), ("to_lowercase", RUST),
    ("to_uppercase", RUST), ("eq", RUST), ("ne", RUST), ("drop", RUST),
    ("lock", RUST), ("read_to_string", RUST), ("write_all", RUST), ("with_capacity", RUST),

    // -- JS / TS: prototypes, Promise, console, JSON, DOM/events --
    ("forEach", JS), ("then", JS), ("catch", JS), ("finally", JS),
    ("resolve", JS), ("reject", JS), ("race", JS), ("splice", JS),
    ("indexOf", JS), ("lastIndexOf", JS), ("includes", JS), ("stringify", JS),
    ("bind", JS), ("apply", JS), ("call", JS), ("require", JS),
    ("assign", JS), ("freeze", JS), ("shift", JS), ("unshift", JS),
    ("some", JS), ("every", JS), ("flat", JS), ("flatMap", JS),
    ("findIndex", JS), ("fill", JS), ("isArray", JS), ("reduceRight", JS),
    ("charAt", JS), ("charCodeAt", JS), ("substring", JS), ("substr", JS),
    ("toLowerCase", JS), ("toUpperCase", JS), ("startsWith", JS), ("endsWith", JS),
    ("padStart", JS), ("padEnd", JS), ("repeat", JS), ("match", JS),
    ("search", JS), ("test", JS), ("exec", JS), ("hasOwnProperty", JS),
    ("setTimeout", JS), ("setInterval", JS), ("clearTimeout", JS), ("clearInterval", JS),
    ("addEventListener", JS), ("removeEventListener", JS), ("preventDefault", JS),
    ("stopPropagation", JS), ("querySelector", JS), ("querySelectorAll", JS),
    ("getElementById", JS), ("fetch", JS), ("json", JS), ("text", JS),
    ("on", JS), ("once", JS), ("off", JS), ("emit", JS), ("table", JS), ("dir", JS),

    // -- Python: builtins, list/dict/str methods, logging, re, os.path, json --
    ("strip", PYTHON), ("lower", PYTHON), ("upper", PYTHON),
    ("startswith", PYTHON), ("endswith", PYTHON), ("isinstance", PYTHON),
    ("range", PYTHON), ("sorted", PYTHON), ("enumerate", PYTHON), ("super", PYTHON),
    ("getattr", PYTHON), ("setattr", PYTHON), ("hasattr", PYTHON),
    ("str", PYTHON), ("int", PYTHON), ("float", PYTHON), ("bool", PYTHON),
    ("list", PYTHON), ("dict", PYTHON), ("tuple", PYTHON), ("frozenset", PYTHON),
    ("setdefault", PYTHON), ("discard", PYTHON), ("union", PYTHON),
    ("rsplit", PYTHON), ("splitlines", PYTHON), ("lstrip", PYTHON), ("rstrip", PYTHON),
    ("rfind", PYTHON), ("capitalize", PYTHON), ("casefold", PYTHON), ("title", PYTHON),
    ("encode", PYTHON), ("decode", PYTHON), ("repr", PYTHON), ("type", PYTHON),
    ("callable", PYTHON), ("issubclass", PYTHON), ("vars", PYTHON), ("id", PYTHON),
    ("round", PYTHON), ("divmod", PYTHON), ("pow", PYTHON), ("reversed", PYTHON),
    ("input", PYTHON), ("eval", PYTHON), ("compile", PYTHON),
    ("globals", PYTHON), ("locals", PYTHON),
    ("warning", PYTHON), ("critical", PYTHON), ("exception", PYTHON),
    ("findall", PYTHON), ("sub", PYTHON), ("group", PYTHON), ("groups", PYTHON),
    ("exists", PYTHON), ("isfile", PYTHON), ("isdir", PYTHON), ("dirname", PYTHON),
    ("basename", PYTHON), ("abspath", PYTHON), ("realpath", PYTHON),
    ("getcwd", PYTHON), ("listdir", PYTHON), ("makedirs", PYTHON), ("mkdir", PYTHON),
    ("walk", PYTHON), ("readline", PYTHON), ("readlines", PYTHON),
    ("writelines", PYTHON), ("seek", PYTHON), ("tell", PYTHON),
    ("dumps", PYTHON), ("loads", PYTHON), ("dump", PYTHON), ("load", PYTHON),

    // -- Go: builtins, fmt/errors/strings/strconv/io/sync/context/time/testing --
    // Method calls on receivers are syntactically indistinguishable from pkg.Func,
    // so both std package functions and common method names live here.
    ("Error", GO | CSHARP), ("String", GO), ("New", GO | CSHARP),
    ("Printf", GO), ("Println", GO), ("Sprintf", GO), ("Errorf", GO),
    ("Fprintf", GO), ("Fprintln", GO), ("Sprint", GO), ("Sprintln", GO),
    ("Fatal", GO), ("Fatalf", GO), ("Print", GO),
    ("Close", GO | CSHARP), ("Read", GO | CSHARP), ("Write", GO | CSHARP),
    ("make", GO), ("cap", GO), ("panic", GO), ("recover", GO),
    ("Is", GO), ("As", GO), ("Unwrap", GO), ("Wrap", GO), ("Wrapf", GO),
    ("Contains", GO | CSHARP), ("HasPrefix", GO), ("HasSuffix", GO),
    ("Split", GO | CSHARP), ("SplitN", GO), ("Join", GO | CSHARP),
    ("Replace", GO | CSHARP), ("ReplaceAll", GO), ("Fields", GO),
    ("TrimSpace", GO), ("Trim", GO | CSHARP), ("TrimPrefix", GO), ("TrimSuffix", GO),
    ("TrimLeft", GO), ("TrimRight", GO),
    ("ToUpper", GO | CSHARP), ("ToLower", GO | CSHARP), ("Index", GO),
    ("EqualFold", GO), ("Repeat", GO), ("Itoa", GO), ("Atoi", GO),
    ("ParseInt", GO), ("ParseFloat", GO), ("ParseBool", GO), ("FormatInt", GO),
    ("Quote", GO), ("Bytes", GO), ("WriteString", GO), ("ReadAll", GO),
    ("ReadFile", GO), ("WriteFile", GO), ("NewReader", GO), ("NewWriter", GO),
    ("NewBuffer", GO), ("NewDecoder", GO), ("NewEncoder", GO),
    ("Decode", GO), ("Encode", GO), ("Marshal", GO), ("Unmarshal", GO),
    ("MarshalJSON", GO), ("UnmarshalJSON", GO),
    ("Lock", GO), ("Unlock", GO), ("RLock", GO), ("RUnlock", GO),
    ("Wait", GO | CSHARP), ("Done", GO), ("Do", GO),
    ("Background", GO), ("TODO", GO), ("WithCancel", GO), ("WithTimeout", GO),
    ("WithValue", GO), ("Value", GO), ("Err", GO), ("Deadline", GO),
    ("Now", GO), ("Since", GO), ("Sleep", GO), ("Unix", GO),
    ("Before", GO), ("After", GO), ("Sub", GO), ("Add", GO | CSHARP),
    ("Parse", GO | CSHARP), ("Format", GO | CSHARP),
    ("Logf", GO), ("Log", GO | CSHARP), ("Helper", GO), ("Run", GO | CSHARP),
    ("Parallel", GO), ("Skip", GO | CSHARP), ("Skipf", GO), ("Cleanup", GO),
    ("Setenv", GO), ("Getenv", GO), ("Sort", GO), ("Slice", GO),
    ("Strings", GO), ("Ints", GO), ("MustCompile", GO), ("MatchString", GO),
    ("FindString", GO), ("ReplaceAllString", GO), ("Len", GO), ("Cap", GO),
    ("Get", GO | CSHARP), ("Set", GO | CSHARP), ("Equal", GO), ("Copy", GO | CSHARP),

    // -- Java / Kotlin: Object/collections/streams/Optional, stdlib scope funcs --
    ("println", JVM), ("valueOf", JVM), ("hashCode", JVM), ("put", JVM),
    ("stream", JVM), ("getClass", JVM), ("let", JVM), ("also", JVM),
    ("run", JVM), ("with", JVM), ("apply", JVM), ("use", JVM),
    ("takeIf", JVM), ("takeUnless", JVM), ("to", JVM),
    ("listOf", JVM), ("mapOf", JVM), ("setOf", JVM), ("arrayListOf", JVM),
    ("mutableListOf", JVM), ("mutableMapOf", JVM), ("mutableSetOf", JVM),
    ("emptyList", JVM), ("emptyMap", JVM), ("emptySet", JVM),
    ("toList", JVM), ("toSet", JVM), ("toMap", JVM), ("toArray", JVM),
    ("toMutableList", JVM), ("firstOrNull", JVM), ("lastOrNull", JVM),
    ("singleOrNull", JVM), ("getOrNull", JVM), ("getOrDefault", JVM),
    ("getOrElse", JVM), ("mapNotNull", JVM), ("filterNotNull", JVM),
    ("associateBy", JVM), ("groupBy", JVM), ("sortedBy", JVM),
    ("joinToString", JVM), ("sumOf", JVM), ("maxByOrNull", JVM), ("minByOrNull", JVM),
    ("isNullOrEmpty", JVM), ("isNullOrBlank", JVM), ("orEmpty", JVM),
    ("require", JVM), ("requireNotNull", JVM), ("check", JVM), ("checkNotNull", JVM),
    ("lazy", JVM), ("launch", JVM), ("async", JVM), ("await", JVM),
    ("withContext", JVM), ("runBlocking", JVM), ("delay", JVM), ("collect", JVM),
    ("emit", JVM), ("buildString", JVM), ("buildList", JVM),
    ("charAt", JVM), ("substring", JVM), ("indexOf", JVM), ("isEmpty", JVM),
    ("isBlank", JVM), ("length", JVM), ("matches", JVM), ("parseInt", JVM),
    ("parseLong", JVM), ("parseDouble", JVM), ("asList", JVM), ("forEach", JVM),
    ("iterator", JVM), ("hasNext", JVM), ("entrySet", JVM), ("keySet", JVM),
    ("containsKey", JVM), ("containsValue", JVM), ("computeIfAbsent", JVM),
    ("putIfAbsent", JVM), ("getKey", JVM), ("getValue", JVM), ("getName", JVM),
    ("getMessage", JVM), ("getCause", JVM), ("printStackTrace", JVM),
    ("currentTimeMillis", JVM), ("nanoTime", JVM), ("getLogger", JVM),
    ("isDebugEnabled", JVM), ("requireNonNull", JVM), ("isPresent", JVM),
    ("ifPresent", JVM), ("orElse", JVM), ("orElseGet", JVM), ("orElseThrow", JVM),
    ("ofNullable", JVM), ("findFirst", JVM), ("anyMatch", JVM), ("allMatch", JVM),
    ("boxed", JVM), ("joining", JVM), ("groupingBy", JVM), ("copyOf", JVM),
    ("singletonList", JVM), ("unmodifiableList", JVM),
    ("assertEquals", JVM), ("assertTrue", JVM), ("assertFalse", JVM),
    ("assertNotNull", JVM), ("assertNull", JVM), ("assertThrows", JVM),
    ("verify", JVM), ("mock", JVM), ("when", JVM), ("thenReturn", JVM),

    // -- C#: LINQ, string/collection/Task, logging, asserts --
    ("Remove", CSHARP), ("Count", CSHARP), ("Clear", CSHARP), ("Insert", CSHARP),
    ("IndexOf", CSHARP), ("ToArray", CSHARP), ("ToList", CSHARP), ("ToDictionary", CSHARP),
    ("Select", CSHARP), ("SelectMany", CSHARP), ("Where", CSHARP),
    ("First", CSHARP), ("FirstOrDefault", CSHARP), ("Single", CSHARP),
    ("SingleOrDefault", CSHARP), ("Any", CSHARP), ("All", CSHARP),
    ("OrderBy", CSHARP), ("OrderByDescending", CSHARP), ("ThenBy", CSHARP),
    ("GroupBy", CSHARP), ("Aggregate", CSHARP), ("Sum", CSHARP),
    ("Min", CSHARP), ("Max", CSHARP), ("Average", CSHARP),
    ("Concat", CSHARP), ("Distinct", CSHARP), ("Take", CSHARP), ("Append", CSHARP),
    ("StartsWith", CSHARP), ("EndsWith", CSHARP), ("Substring", CSHARP),
    ("IsNullOrEmpty", CSHARP), ("IsNullOrWhiteSpace", CSHARP), ("TryParse", CSHARP),
    ("ToString", CSHARP), ("Equals", CSHARP), ("GetHashCode", CSHARP),
    ("GetType", CSHARP), ("CompareTo", CSHARP), ("Dispose", CSHARP),
    ("WriteLine", CSHARP), ("ReadLine", CSHARP), ("ReadAllText", CSHARP),
    ("WriteAllText", CSHARP), ("TryGetValue", CSHARP), ("ContainsKey", CSHARP),
    ("Invoke", CSHARP), ("ConfigureAwait", CSHARP), ("GetAwaiter", CSHARP),
    ("Delay", CSHARP), ("FromResult", CSHARP), ("WhenAll", CSHARP), ("WhenAny", CSHARP),
    ("LogInformation", CSHARP), ("LogWarning", CSHARP), ("LogError", CSHARP),
    ("LogDebug", CSHARP), ("AreEqual", CSHARP), ("IsTrue", CSHARP), ("IsFalse", CSHARP),

    // -- C / C++: libc, stdio, string.h, math.h, STL containers/algorithms --
    ("printf", C_CPP), ("fprintf", C_CPP), ("sprintf", C_CPP), ("snprintf", C_CPP),
    ("malloc", C_CPP), ("calloc", C_CPP), ("realloc", C_CPP), ("free", C_CPP),
    ("memcpy", C_CPP), ("memset", C_CPP), ("memmove", C_CPP), ("memcmp", C_CPP),
    ("strcmp", C_CPP), ("strncmp", C_CPP), ("strcpy", C_CPP), ("strncpy", C_CPP),
    ("strlen", C_CPP), ("strcat", C_CPP), ("strncat", C_CPP), ("strchr", C_CPP),
    ("strrchr", C_CPP), ("strstr", C_CPP), ("strtok", C_CPP), ("strdup", C_CPP),
    ("fopen", C_CPP), ("fclose", C_CPP), ("fread", C_CPP), ("fwrite", C_CPP),
    ("fseek", C_CPP), ("ftell", C_CPP), ("fgets", C_CPP), ("fputs", C_CPP),
    ("fgetc", C_CPP), ("fputc", C_CPP), ("getchar", C_CPP), ("putchar", C_CPP),
    ("puts", C_CPP), ("scanf", C_CPP), ("fscanf", C_CPP), ("sscanf", C_CPP),
    ("atoi", C_CPP), ("atof", C_CPP), ("atol", C_CPP), ("strtol", C_CPP),
    ("strtoul", C_CPP), ("strtod", C_CPP), ("qsort", C_CPP), ("bsearch", C_CPP),
    ("rand", C_CPP), ("srand", C_CPP), ("time", C_CPP), ("clock", C_CPP),
    ("isdigit", C_CPP), ("isalpha", C_CPP), ("isspace", C_CPP),
    ("toupper", C_CPP), ("tolower", C_CPP), ("labs", C_CPP),
    ("sqrt", C_CPP), ("floor", C_CPP), ("ceil", C_CPP), ("fabs", C_CPP),
    ("empty", C_CPP), ("begin", C_CPP), ("end", C_CPP), ("rbegin", C_CPP),
    ("rend", C_CPP), ("cbegin", C_CPP), ("cend", C_CPP), ("front", C_CPP),
    ("back", C_CPP), ("push_back", C_CPP), ("pop_back", C_CPP),
    ("push_front", C_CPP), ("pop_front", C_CPP), ("emplace", C_CPP),
    ("emplace_back", C_CPP), ("at", C_CPP), ("data", C_CPP), ("c_str", C_CPP),
    ("substr", C_CPP), ("erase", C_CPP), ("swap", C_CPP), ("capacity", C_CPP),
    ("shrink_to_fit", C_CPP), ("make_pair", C_CPP), ("make_tuple", C_CPP),
    ("make_shared", C_CPP), ("make_unique", C_CPP), ("release", C_CPP),
    ("try_lock", C_CPP), ("move", C_CPP), ("forward", C_CPP),
    ("stoi", C_CPP), ("stol", C_CPP), ("stod", C_CPP), ("stof", C_CPP),
    ("stable_sort", C_CPP), ("transform", C_CPP), ("accumulate", C_CPP),
    ("copy_if", C_CPP), ("remove_if", C_CPP), ("distance", C_CPP), ("advance", C_CPP),
];

fn table() -> &'static HashMap<&'static str, LangMask> {
    static TABLE: OnceLock<HashMap<&'static str, LangMask>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut m: HashMap<&'static str, LangMask> = HashMap::with_capacity(UBIQUITOUS.len());
        for &(name, mask) in UBIQUITOUS {
            *m.entry(name).or_insert(0) |= mask;
        }
        m
    })
}

/// Is `name` a ubiquitous std/utility callable in the given caller language?
/// `language` is the caller file's language string (`FileEntry.language`);
/// `None`/unknown filters against every ecosystem's table (conservative).
pub fn is_ubiquitous_callable(name: &str, language: Option<&str>) -> bool {
    table()
        .get(name)
        .is_some_and(|mask| mask & mask_for_language(language) != 0)
}

/// Is `name` stop-listed in *any* ecosystem? Used on the query side so
/// callers/callees can tell the agent "this name is filtered by design"
/// instead of returning a silent empty result.
pub fn is_stoplisted_name(name: &str) -> bool {
    table().contains_key(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_names_filtered_everywhere() {
        for lang in [Some("rust"), Some("go"), Some("python"), None] {
            assert!(is_ubiquitous_callable("len", lang));
            assert!(is_ubiquitous_callable("new", lang));
            assert!(is_ubiquitous_callable("push", lang));
        }
    }

    #[test]
    fn per_language_names_do_not_leak_across_ecosystems() {
        // JS prototype method: noise in TS/JS, a legit unique symbol elsewhere.
        assert!(is_ubiquitous_callable("forEach", Some("typescript")));
        assert!(is_ubiquitous_callable("forEach", Some("javascript")));
        assert!(!is_ubiquitous_callable("forEach", Some("rust")));
        // libc: noise in C/C++ only.
        assert!(is_ubiquitous_callable("printf", Some("c")));
        assert!(is_ubiquitous_callable("printf", Some("cpp")));
        assert!(!is_ubiquitous_callable("printf", Some("python")));
        // Rust Option combinator: not filtered for Go callers.
        assert!(is_ubiquitous_callable("unwrap", Some("rust")));
        assert!(!is_ubiquitous_callable("unwrap", Some("go")));
    }

    #[test]
    fn unknown_language_filters_conservatively() {
        assert!(is_ubiquitous_callable("forEach", None));
        assert!(is_ubiquitous_callable("printf", Some("weird-lang")));
    }

    #[test]
    fn project_names_pass_through() {
        for lang in [Some("rust"), Some("typescript"), None] {
            assert!(!is_ubiquitous_callable("ask_context", lang));
            assert!(!is_ubiquitous_callable("fill_snippets", lang));
            assert!(!is_ubiquitous_callable("build_call_relations", lang));
        }
    }

    #[test]
    fn stoplisted_name_check_is_language_agnostic() {
        assert!(is_stoplisted_name("unwrap"));
        assert!(is_stoplisted_name("forEach"));
        assert!(!is_stoplisted_name("resolve_import"));
    }
}
