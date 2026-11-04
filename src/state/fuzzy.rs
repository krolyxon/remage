//! Fuzzy matching for the palette and filename search (nucleo, smart case).

use nucleo_matcher::pattern::{CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher, Utf32Str};

pub struct Fuzzy {
    matcher: Matcher,
    pattern: Pattern,
    buf: Vec<char>,
}

impl std::fmt::Debug for Fuzzy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Fuzzy")
    }
}

impl Fuzzy {
    pub fn new(query: &str) -> Self {
        Self {
            matcher: Matcher::new(Config::DEFAULT),
            pattern: Pattern::parse(query, CaseMatching::Smart, Normalization::Smart),
            buf: Vec::new(),
        }
    }

    /// Like [`Fuzzy::new`], tuned for paths (separators score as word boundaries).
    pub fn for_paths(query: &str) -> Self {
        let mut config = Config::DEFAULT;
        config.set_match_paths();
        Self {
            matcher: Matcher::new(config),
            ..Self::new(query)
        }
    }

    /// The match score, higher is better; `None` if it doesn't match. An empty query
    /// matches everything with score 0.
    pub fn score(&mut self, haystack: &str) -> Option<u32> {
        let h = Utf32Str::new(haystack, &mut self.buf);
        self.pattern.score(h, &mut self.matcher)
    }
}

/// Indices of `items` matching `query`, best first. Ties go to the shorter item, then keep
/// their original order (an empty query keeps every item in place).
pub fn rank<'a>(query: &str, items: impl IntoIterator<Item = &'a str>) -> Vec<usize> {
    let mut f = Fuzzy::new(query);
    let empty = query.trim().is_empty();
    let mut scored: Vec<(u32, usize, usize)> = items
        .into_iter()
        .enumerate()
        .filter_map(|(i, s)| {
            let len = if empty { 0 } else { s.chars().count() };
            f.score(s).map(|sc| (sc, len, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, _, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranks_best_first() {
        let items = ["next-mark", "next", "reload-config", "help"];
        let r = rank("nx", items);
        assert_eq!(r[0], 1, "{r:?}");
        assert!(r.contains(&0));
        assert!(!r.contains(&3));
        assert_eq!(rank("", items), [0, 1, 2, 3]);
        // Smart case: an upper-case letter makes it case-sensitive.
        assert_eq!(rank("HELP", items), Vec::<usize>::new());
    }
}
