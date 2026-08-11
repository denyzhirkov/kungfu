//! Release-tag comparison. Deliberately not a semver dependency: kungfu tags are
//! plain `vMAJOR.MINOR.PATCH`, and an unparsable tag must degrade to "no update"
//! rather than fail a command.

/// `v2.6.2` / `2.6.2` / `2.6.2-rc1` → `(2, 6, 2)`. Anything else → `None`.
pub fn parse(tag: &str) -> Option<(u64, u64, u64)> {
    let core = tag.trim().trim_start_matches('v');
    let core = core.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0").parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// True only when both tags parse and `candidate` is strictly greater. An
/// unparsable tag never triggers an update prompt.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (parse(candidate), parse(current)) {
        (Some(a), Some(b)) => a > b,
        _ => false,
    }
}

/// Normalized `MAJOR.MINOR.PATCH` string (no leading `v`), for URLs and output.
pub fn normalize(tag: &str) -> String {
    tag.trim().trim_start_matches('v').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tags_with_and_without_prefix() {
        assert_eq!(parse("v2.6.2"), Some((2, 6, 2)));
        assert_eq!(parse("2.6.2"), Some((2, 6, 2)));
        assert_eq!(parse("2.7"), Some((2, 7, 0)));
        assert_eq!(parse("2.7.0-rc1"), Some((2, 7, 0)));
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("latest"), None);
        assert_eq!(parse("2.6.2.1"), None);
        assert_eq!(parse("<html>"), None);
    }

    #[test]
    fn compares_components_not_strings() {
        assert!(is_newer("2.10.0", "2.9.9"), "10 > 9 numerically");
        assert!(is_newer("v3.0.0", "2.99.99"));
        assert!(!is_newer("2.6.2", "2.6.2"));
        assert!(!is_newer("2.6.1", "2.6.2"));
    }

    #[test]
    fn unparsable_never_claims_an_update() {
        assert!(!is_newer("garbage", "2.6.2"));
        assert!(!is_newer("2.7.0", "garbage"));
    }
}
