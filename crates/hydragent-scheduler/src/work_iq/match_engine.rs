//! Keyword rule engine for Work IQ.
//!
//! Each feed carries a list of [`KeywordRule`]s. When a feed entry arrives,
//! the engine evaluates the rules against the entry's title + summary and
//! returns a [`MatchResult`] containing a relevance score and the list of
//! matching rule phrases (or `Exclude` phrases, which short-circuit to a
//! zero/negative score).
//!
//! ### Rule kinds
//!
//! | Kind      | Behavior                                                        |
//! |-----------|-----------------------------------------------------------------|
//! | `Include` | Case-insensitive substring match. Each match adds `weight`.     |
//! | `Phrase`  | Case-insensitive *exact phrase* match (whitespace normalized).  |
//! | `Exclude` | Case-insensitive substring. If present, the entry is rejected. |
//!
//! `Include` and `Phrase` are positive signals. `Exclude` is a veto.
//!
//! ### Backwards compatibility
//!
//! [`KeywordRule::from_legacy_csv`] parses the old comma-separated
//! `keywords` string into `Include` rules with weight `1.0`.

use serde::{Deserialize, Serialize};

/// A single keyword rule persisted alongside a feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum KeywordRule {
    /// Case-insensitive substring match. Each occurrence adds `weight`.
    Include { phrase: String, weight: f32 },
    /// Case-insensitive exact phrase match (whitespace collapsed).
    /// The phrase must contain a space (use `Include` for single tokens).
    Phrase { phrase: String, weight: f32 },
    /// Case-insensitive substring. If found anywhere in the entry, the
    /// entry is vetoed — score becomes `0.0` regardless of includes.
    Exclude { phrase: String },
}

impl KeywordRule {
    /// Parse the legacy `"foo, bar, baz"` format into `Include` rules with
    /// weight `1.0`. Empty entries and surrounding whitespace are ignored.
    pub fn from_legacy_csv(csv: &str) -> Vec<Self> {
        csv.split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|phrase| KeywordRule::Include {
                phrase: phrase.to_string(),
                weight: 1.0,
            })
            .collect()
    }

    /// JSON-encoded list of rules. Stable on disk; safe to round-trip.
    pub fn list_to_json(rules: &[KeywordRule]) -> Result<String, serde_json::Error> {
        serde_json::to_string(rules)
    }

    pub fn list_from_json(json: &str) -> Result<Vec<KeywordRule>, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The phrase this rule looks for, regardless of kind.
    pub fn phrase(&self) -> &str {
        match self {
            KeywordRule::Include { phrase, .. }
            | KeywordRule::Phrase { phrase, .. }
            | KeywordRule::Exclude { phrase } => phrase,
        }
    }
}

/// Outcome of evaluating a rule list against an entry's text.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchResult {
    /// Total relevance score from `Include`/`Phrase` rules.
    /// `0.0` if the entry was excluded, vetoed, or had no positive matches.
    pub score: f32,
    /// The phrases that contributed to the score (informational only).
    pub matched: Vec<String>,
    /// `true` if the entry matched at least one positive rule.
    pub matched_positive: bool,
    /// `true` if the entry contained an `Exclude` phrase (veto).
    pub excluded: bool,
}

impl MatchResult {
    pub fn no_match() -> Self {
        Self {
            score: 0.0,
            matched: Vec::new(),
            matched_positive: false,
            excluded: false,
        }
    }
}

/// Evaluate a rule list against the given text (typically `title + " " + summary`).
///
/// Returns a [`MatchResult`]. If any `Exclude` rule matches, the score is
/// clamped to `0.0` and `excluded` is set. Otherwise the score is the sum of
/// weights for each `Include`/`Phrase` rule that matched.
pub fn evaluate(rules: &[KeywordRule], text: &str) -> MatchResult {
    if rules.is_empty() {
        return MatchResult::no_match();
    }

    let lower = text.to_lowercase();

    // Veto pass: any exclude → done.
    for rule in rules {
        if let KeywordRule::Exclude { phrase } = rule {
            if contains_ci(&lower, phrase) {
                return MatchResult {
                    score: 0.0,
                    matched: Vec::new(),
                    matched_positive: false,
                    excluded: true,
                };
            }
        }
    }

    // Scoring pass.
    let mut score = 0.0_f32;
    let mut matched = Vec::new();
    for rule in rules {
        match rule {
            KeywordRule::Include { phrase, weight } => {
                let occurrences = count_ci(&lower, phrase);
                if occurrences > 0 {
                    score += weight * occurrences as f32;
                    matched.push(format!("include:{}", phrase));
                }
            }
            KeywordRule::Phrase { phrase, weight } => {
                if !phrase.contains(char::is_whitespace) {
                    // Phrase rule on a single token — silently ignore.
                    // Users should use Include for tokens.
                    continue;
                }
                let normalized = normalize_whitespace(phrase);
                let normalized_text = normalize_whitespace(&lower);
                if normalized_text.contains(&normalized) {
                    score += *weight;
                    matched.push(format!("phrase:{}", phrase));
                }
            }
            KeywordRule::Exclude { .. } => { /* already handled */ }
        }
    }

    MatchResult {
        score,
        matched,
        matched_positive: score > 0.0,
        excluded: false,
    }
}

/// Case-insensitive substring search. Caller has already lower-cased `haystack`.
fn contains_ci(haystack_lower: &str, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return false;
    }
    haystack_lower.contains(&needle)
}

/// Count case-insensitive substring occurrences. Caller has already lower-cased.
fn count_ci(haystack_lower: &str, needle: &str) -> usize {
    let needle = needle.to_lowercase();
    if needle.is_empty() {
        return 0;
    }
    haystack_lower.matches(&needle).count()
}

/// Collapse runs of whitespace to a single space. Used for phrase matching
/// so `"foo  bar"` and `"foo bar"` are equivalent.
fn normalize_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn inc(phrase: &str, weight: f32) -> KeywordRule {
        KeywordRule::Include { phrase: phrase.into(), weight }
    }
    fn phr(phrase: &str, weight: f32) -> KeywordRule {
        KeywordRule::Phrase { phrase: phrase.into(), weight }
    }
    fn exc(phrase: &str) -> KeywordRule {
        KeywordRule::Exclude { phrase: phrase.into() }
    }

    #[test]
    fn empty_rules_yield_no_match() {
        let r = evaluate(&[], "anything");
        assert_eq!(r, MatchResult::no_match());
    }

    #[test]
    fn empty_text_never_matches() {
        let r = evaluate(&[inc("foo", 1.0)], "");
        assert_eq!(r.score, 0.0);
        assert!(!r.matched_positive);
        assert!(!r.excluded);
    }

    #[test]
    fn include_substring_is_case_insensitive() {
        let rules = vec![inc("rust", 2.0)];
        let r = evaluate(&rules, "I love Rust and RUST");
        assert!(r.matched_positive);
        // "Rust" + "RUST" → 2 occurrences × 2.0 = 4.0
        assert!((r.score - 4.0).abs() < 1e-6);
        assert_eq!(r.matched, vec!["include:rust".to_string()]);
    }

    #[test]
    fn exclude_vetoes_score() {
        let rules = vec![inc("ai", 5.0), exc("crypto")];
        let r = evaluate(&rules, "AI news, no crypto today");
        assert!(r.excluded);
        assert_eq!(r.score, 0.0);
        assert!(!r.matched_positive);
    }

    #[test]
    fn exclude_does_not_veto_unrelated_text() {
        let rules = vec![inc("ai", 1.0), exc("crypto")];
        let r = evaluate(&rules, "AI regulation update");
        assert!(!r.excluded);
        assert_eq!(r.score, 1.0);
    }

    #[test]
    fn phrase_requires_whitespace_normalization() {
        let rules = vec![phr("machine learning", 3.0)];
        let r1 = evaluate(&rules, "advances in machine   learning systems");
        assert!(r1.matched_positive);
        assert_eq!(r1.score, 3.0);

        let r2 = evaluate(&rules, "machinelearning");
        assert!(!r2.matched_positive);
        assert_eq!(r2.score, 0.0);
    }

    #[test]
    fn single_token_phrase_is_ignored() {
        let rules = vec![phr("ai", 5.0)];
        let r = evaluate(&rules, "ai everywhere");
        // Single-token Phrase rules are silently ignored — use Include.
        assert_eq!(r, MatchResult::no_match());
    }

    #[test]
    fn legacy_csv_parses_to_include_rules() {
        let rules = KeywordRule::from_legacy_csv("rust, tokio , ,");
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[0].phrase(), "rust");
        assert_eq!(rules[1].phrase(), "tokio");
        assert!(matches!(rules[0], KeywordRule::Include { weight, .. } if weight == 1.0));
    }

    #[test]
    fn json_round_trip_preserves_rules() {
        let rules = vec![
            inc("rust", 1.5),
            phr("machine learning", 2.0),
            exc("crypto"),
        ];
        let json = KeywordRule::list_to_json(&rules).unwrap();
        let back = KeywordRule::list_from_json(&json).unwrap();
        assert_eq!(back, rules);
    }

    #[test]
    fn multiple_includes_accumulate() {
        let rules = vec![inc("ai", 1.0), inc("rust", 2.0), inc("python", 3.0)];
        let r = evaluate(&rules, "ai rust python");
        assert_eq!(r.score, 6.0);
        assert_eq!(r.matched.len(), 3);
    }

    #[test]
    fn normalize_whitespace_collapses_runs() {
        assert_eq!(normalize_whitespace("  foo\t bar  baz "), "foo bar baz");
    }

    #[test]
    fn exclude_veto_takes_precedence_over_multiple_includes() {
        let rules = vec![inc("ai", 10.0), inc("rust", 10.0), exc("crypto")];
        let r = evaluate(&rules, "AI and Rust crypto news");
        assert!(r.excluded);
        assert_eq!(r.score, 0.0);
    }
}