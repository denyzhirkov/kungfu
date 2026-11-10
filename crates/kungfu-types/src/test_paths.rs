//! Path and module-name conventions that mark code as tests. One predicate
//! for every crate, so indexing, search and the test-selection use cases agree.
//! A bare "test" substring is never a signal ("contest.rs" is not a test).

const TEST_DIRS: [&str; 6] = ["tests", "test", "__tests__", "spec", "specs", "testdata"];

/// True when any directory segment (case-insensitive) is a test directory.
/// This also covers the JVM `src/test/java` layout and a top-level `tests/`.
pub fn is_test_dir(path: &str) -> bool {
    let mut parts = path.split('/');
    parts.next_back();
    parts.any(|d| TEST_DIRS.contains(&d.to_lowercase().as_str()))
}

/// True for test directories and test file names: `*_test.*`, `*_tests.*`,
/// `*_unittest.*`, `*_spec.*`, `*.test.*`, `*.spec.*`, `test_*.{py,c,cc,cpp,cxx}`,
/// `conftest.py`, Rust `tests.rs`/`test.rs` (an out-of-line `mod tests;`) and
/// Java/Kotlin/C# `*Test`/`*Tests`. `ABTest.java`-style production names are an
/// accepted false positive.
pub fn is_test_path(path: &str) -> bool {
    if is_test_dir(path) {
        return true;
    }
    let file = path.rsplit('/').next().unwrap_or(path);
    let lower = file.to_lowercase();
    if lower.contains(".test.") || lower.contains(".spec.") {
        return true;
    }
    let (stem, ext) = file.rsplit_once('.').unwrap_or((file, ""));
    let lstem = stem.to_lowercase();
    if ["_test", "_tests", "_unittest", "_spec"]
        .iter()
        .any(|s| lstem.ends_with(s))
    {
        return true;
    }
    match ext {
        "py" => lstem.starts_with("test_") || lstem == "conftest",
        "c" | "cc" | "cpp" | "cxx" => lstem.starts_with("test_"),
        "rs" => lstem == "tests" || lstem == "test",
        "java" | "kt" | "cs" => {
            stem.len() > "Test".len() && (stem.ends_with("Test") || stem.ends_with("Tests"))
        }
        _ => false,
    }
}

/// Module/namespace names that hold tests: `tests`, `test`, `*_tests`,
/// `*_test`, `test_*` (case-insensitive).
pub fn is_test_module_name(name: &str) -> bool {
    let n = name.to_lowercase();
    n == "tests"
        || n == "test"
        || n.ends_with("_tests")
        || n.ends_with("_test")
        || n.starts_with("test_")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_paths() {
        for p in [
            "tests/foo.rs",
            "crates/a/tests/foo.rs",
            "pkg/x_test.go",
            "app/test_x.py",
            "app/x_test.py",
            "app/conftest.py",
            "web/foo.test.ts",
            "web/foo.spec.js",
            "web/__tests__/foo.ts",
            "src/test/java/FooTest.java",
            "Foo/Tests/Bar.cs",
            "Foo/BarTests.cs",
            "app/FooTest.kt",
            "spec/foo_spec.rb",
            "src/foo/tests.rs",
            "native/test_math.cpp",
            "native/math_unittest.cc",
        ] {
            assert!(is_test_path(p), "{p} should be a test path");
        }
        for p in [
            "src/lib.rs",
            "crates/kungfu-core/src/review.rs",
            "pkg/x.go",
            "app/app.py",
            "src/latest.rs",
            "src/contest.ts",
            "src/Test.java",
            "src/test_helpers.rs",
            "src/attests.rs",
        ] {
            assert!(!is_test_path(p), "{p} should not be a test path");
        }
    }

    #[test]
    fn test_module_names() {
        for n in ["tests", "Test", "ask_tests", "parse_test", "test_utils"] {
            assert!(is_test_module_name(n), "{n}");
        }
        for n in ["latest", "contest", "testing", "protests_api"] {
            assert!(!is_test_module_name(n), "{n}");
        }
    }
}
