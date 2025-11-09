use regex::{Regex, RegexBuilder};
use std::collections::HashSet;

pub fn build_highlight_regex(tokens: &[String]) -> Option<Regex> {
    if tokens.is_empty() {
        return None;
    }
    let mut unique = Vec::new();
    let mut seen = HashSet::new();
    for token in tokens {
        if token.is_empty() {
            continue;
        }
        let lowered = token.to_lowercase();
        if seen.insert(lowered) {
            unique.push(token.clone());
        }
    }
    if unique.is_empty() {
        return None;
    }
    unique.sort_by(|a, b| b.len().cmp(&a.len()));
    let pattern = unique
        .into_iter()
        .map(|token| regex::escape(&token))
        .collect::<Vec<_>>()
        .join("|");
    RegexBuilder::new(&pattern)
        .case_insensitive(true)
        .build()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_longer_tokens_first() {
        let regex = build_highlight_regex(&["not".into(), "note".into()]).expect("regex");
        let matches: Vec<_> = regex.find_iter("notebook").map(|m| m.as_str()).collect();
        assert_eq!(matches, vec!["note"]);
    }

    #[test]
    fn deduplicates_case_insensitive_tokens() {
        let regex =
            build_highlight_regex(&["Note".into(), "note".into(), "NOTE".into()]).expect("regex");
        let matches: Vec<_> = regex.find_iter("note").map(|m| m.as_str()).collect();
        assert_eq!(matches, vec!["note"]);
    }
}
