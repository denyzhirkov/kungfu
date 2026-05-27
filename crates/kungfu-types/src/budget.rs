use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Budget {
    Tiny,
    #[default]
    Small,
    Medium,
    Full,
    /// Auto-resolve based on project size and query complexity
    Auto,
}

impl Budget {
    pub fn top_k(&self) -> usize {
        match self {
            Budget::Tiny => 3,
            Budget::Small | Budget::Auto => 5,
            Budget::Medium => 8,
            Budget::Full => 12,
        }
    }

    pub fn max_lines(&self) -> usize {
        match self {
            Budget::Tiny => 0,
            Budget::Small | Budget::Auto => 20,
            Budget::Medium => 40,
            Budget::Full => 100,
        }
    }

    /// Resolve Auto to a concrete budget based on project size.
    /// - Small projects (<100 files): Full (cheap to return more)
    /// - Medium projects (<1000 files): Small
    /// - Large projects (1000+): Tiny
    pub fn resolve(self, file_count: usize) -> Budget {
        if self != Budget::Auto {
            return self;
        }
        match file_count {
            0..=100 => Budget::Medium,
            101..=500 => Budget::Small,
            _ => Budget::Small,
        }
    }
}

impl fmt::Display for Budget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Budget::Tiny => write!(f, "tiny"),
            Budget::Small => write!(f, "small"),
            Budget::Medium => write!(f, "medium"),
            Budget::Full => write!(f, "full"),
            Budget::Auto => write!(f, "auto"),
        }
    }
}

impl FromStr for Budget {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "tiny" => Ok(Budget::Tiny),
            "small" => Ok(Budget::Small),
            "medium" => Ok(Budget::Medium),
            "full" => Ok(Budget::Full),
            "auto" => Ok(Budget::Auto),
            _ => Err(format!(
                "invalid budget: '{}' (expected tiny, small, medium, full, auto)",
                s
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_all_budgets() {
        assert_eq!("tiny".parse::<Budget>().unwrap(), Budget::Tiny);
        assert_eq!("small".parse::<Budget>().unwrap(), Budget::Small);
        assert_eq!("medium".parse::<Budget>().unwrap(), Budget::Medium);
        assert_eq!("full".parse::<Budget>().unwrap(), Budget::Full);
        assert_eq!("SMALL".parse::<Budget>().unwrap(), Budget::Small);
        assert!("invalid".parse::<Budget>().is_err());
    }

    #[test]
    fn top_k_ordering() {
        assert!(Budget::Tiny.top_k() < Budget::Small.top_k());
        assert!(Budget::Small.top_k() < Budget::Medium.top_k());
        assert!(Budget::Medium.top_k() < Budget::Full.top_k());
    }

    #[test]
    fn max_lines_tiny_is_zero() {
        assert_eq!(Budget::Tiny.max_lines(), 0);
    }

    #[test]
    fn display_roundtrip() {
        for b in [Budget::Tiny, Budget::Small, Budget::Medium, Budget::Full] {
            assert_eq!(b.to_string().parse::<Budget>().unwrap(), b);
        }
    }

    #[test]
    fn serde_roundtrip() {
        for b in [Budget::Tiny, Budget::Small, Budget::Medium, Budget::Full] {
            let json = serde_json::to_string(&b).unwrap();
            let parsed: Budget = serde_json::from_str(&json).unwrap();
            assert_eq!(b, parsed);
        }
    }

    #[test]
    fn default_is_small() {
        assert_eq!(Budget::default(), Budget::Small);
    }

    #[test]
    fn auto_resolve_small_project() {
        assert_eq!(Budget::Auto.resolve(50), Budget::Medium);
        assert_eq!(Budget::Auto.resolve(100), Budget::Medium);
    }

    #[test]
    fn auto_resolve_large_project() {
        assert_eq!(Budget::Auto.resolve(500), Budget::Small);
        assert_eq!(Budget::Auto.resolve(10000), Budget::Small);
    }

    #[test]
    fn explicit_budget_not_resolved() {
        assert_eq!(Budget::Tiny.resolve(50), Budget::Tiny);
        assert_eq!(Budget::Full.resolve(50000), Budget::Full);
    }

    #[test]
    fn auto_parse_and_display() {
        assert_eq!("auto".parse::<Budget>().unwrap(), Budget::Auto);
        assert_eq!(Budget::Auto.to_string(), "auto");
    }
}
