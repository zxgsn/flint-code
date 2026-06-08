//! Keyword-based memory search with TF-IDF-style scoring.
//!
//! No external embedding dependencies — pure Rust text matching.
//! Designed to be upgradeable: swap in vector search behind the same API later.

use std::collections::HashMap;

use crate::types::MemoryEntry;

/// A search result with score.
#[derive(Debug, Clone)]
pub struct SearchResult {
    pub entry: MemoryEntry,
    pub score: f64,
}

/// Search memories by keyword relevance.
///
/// Scoring formula:
/// - Term frequency: count of query terms found in memory content
/// - Inverse document frequency: rare terms across all memories score higher
/// - Tag bonus: +20% per matching tag
/// - Trust multiplier: High=1.5, Medium=1.0, Low=0.7
/// - Recency boost: `1.0 / (1.0 + age_hours / 168.0)` (1-week half-life)
/// - Access boost: `1.0 + ln(access_count + 1) * 0.3`
/// - Confidence: effective_confidence value
pub fn search(
    query: &str,
    entries: &[&MemoryEntry],
    limit: usize,
) -> Vec<SearchResult> {
    if query.is_empty() || entries.is_empty() {
        return Vec::new();
    }

    let query_terms = tokenize(query);
    if query_terms.is_empty() {
        return Vec::new();
    }

    // Build IDF: count how many entries contain each term
    let mut doc_freq: HashMap<String, u32> = HashMap::new();
    let tokenized_entries: Vec<(Vec<String>, &MemoryEntry)> = entries
        .iter()
        .map(|e| {
            let tokens = tokenize(&e.search_text());
            for token in tokens.iter().collect::<std::collections::HashSet<_>>() {
                *doc_freq.entry(token.clone()).or_insert(0) += 1;
            }
            (tokens, *e)
        })
        .collect();

    let n = entries.len() as f64;

    let mut results: Vec<SearchResult> = tokenized_entries
        .into_iter()
        .filter_map(|(tokens, entry)| {
            // Term frequency: count query terms found in entry
            let token_set: std::collections::HashSet<&str> =
                tokens.iter().map(|s| s.as_str()).collect();
            let mut tf_score = 0.0;
            for qt in &query_terms {
                if token_set.contains(qt.as_str()) {
                    // IDF weight
                    let df = doc_freq.get(qt).copied().unwrap_or(1) as f64;
                    let idf = (n / df).ln() + 1.0;
                    tf_score += idf;
                }
            }

            if tf_score == 0.0 {
                return None;
            }

            // Tag bonus: +20% per matching tag
            let query_lower = query.to_lowercase();
            let tag_bonus = entry
                .tags
                .iter()
                .filter(|t| query_lower.contains(&t.to_lowercase()))
                .count() as f64
                * 0.2;

            // Trust multiplier
            let trust_mult = entry.trust.score_multiplier();

            // Recency boost (1-week half-life)
            let age_hours = (chrono::Utc::now() - entry.updated_at)
                .num_hours()
                .max(0) as f64;
            let recency = 1.0 / (1.0 + age_hours / 168.0);

            // Access boost
            let access_boost = 1.0 + (entry.access_count as f64 + 1.0).ln() * 0.3;

            // Category bonus (normalized)
            let category_bonus = entry.category.score_bonus() / 50.0;

            // Confidence
            let confidence = entry.effective_confidence() as f64;

            // Final score
            let score = tf_score * (1.0 + tag_bonus) * trust_mult * recency * access_boost
                * (1.0 + category_bonus)
                * confidence;

            Some(SearchResult {
                entry: entry.clone(),
                score,
            })
        })
        .collect();

    // Sort by score descending
    results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));

    // Truncate to limit
    results.truncate(limit);
    results
}

/// Tokenize text into lowercase alphanumeric terms, filtering stopwords and short tokens.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2 && !is_stopword(w))
        .map(|w| w.to_string())
        .collect()
}

/// Common English stopwords to filter out of search queries.
fn is_stopword(word: &str) -> bool {
    matches!(
        word,
        "the" | "a" | "an" | "is" | "are" | "was" | "were" | "be" | "been"
            | "being" | "have" | "has" | "had" | "do" | "does" | "did"
            | "will" | "would" | "could" | "should" | "may" | "might"
            | "shall" | "can" | "to" | "of" | "in" | "for" | "on" | "with"
            | "at" | "by" | "from" | "as" | "into" | "through" | "during"
            | "before" | "after" | "above" | "below" | "between" | "out"
            | "off" | "over" | "under" | "again" | "further" | "then"
            | "once" | "here" | "there" | "when" | "where" | "why" | "how"
            | "all" | "each" | "every" | "both" | "few" | "more" | "most"
            | "other" | "some" | "such" | "no" | "nor" | "not" | "only"
            | "own" | "same" | "so" | "than" | "too" | "very" | "just"
            | "and" | "but" | "or" | "if" | "while" | "about" | "up"
            | "it" | "its" | "this" | "that" | "these" | "those" | "what"
            | "which" | "who" | "whom" | "i" | "me" | "my" | "we" | "our"
            | "you" | "your" | "he" | "him" | "his" | "she" | "her"
            | "they" | "them" | "their" | "any" | "also" | "much" | "many"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{MemoryCategory, MemoryEntry, TrustLevel};

    fn make_entry(content: &str, tags: Vec<&str>) -> MemoryEntry {
        MemoryEntry::new(MemoryCategory::Fact, content)
            .with_tags(tags.into_iter().map(String::from).collect())
    }

    #[test]
    fn test_basic_search() {
        let e1 = make_entry("Rust uses cargo for build management", vec!["rust", "cargo"]);
        let e2 = make_entry("Python uses pip for package management", vec!["python", "pip"]);
        let e3 = make_entry("The cargo build system is fast", vec!["rust", "cargo"]);

        let entries = vec![&e1, &e2, &e3];
        let results = search("cargo build", &entries, 10);

        assert!(!results.is_empty());
        // e1 and e3 should score higher than e2 for "cargo build"
        assert!(results[0].score > 0.0);
    }

    #[test]
    fn test_tag_bonus() {
        let e1 = make_entry("Some content about testing", vec!["testing"]);
        let e2 = make_entry("Some content about other stuff", vec!["other"]);

        let entries = vec![&e1, &e2];
        let results = search("testing", &entries, 10);

        // e1 with matching tag should score higher
        assert!(!results.is_empty());
        let e1_result = results.iter().find(|r| r.entry.id == e1.id);
        let e2_result = results.iter().find(|r| r.entry.id == e2.id);
        if let (Some(r1), Some(r2)) = (e1_result, e2_result) {
            assert!(r1.score > r2.score);
        }
    }

    #[test]
    fn test_trust_multiplier() {
        let mut e1 = make_entry("Important fact", vec![]);
        e1.trust = TrustLevel::High;
        let mut e2 = make_entry("Important fact", vec![]);
        e2.trust = TrustLevel::Low;

        let entries = vec![&e1, &e2];
        let results = search("important fact", &entries, 10);

        let r1 = results.iter().find(|r| r.entry.id == e1.id);
        let r2 = results.iter().find(|r| r.entry.id == e2.id);
        if let (Some(r1), Some(r2)) = (r1, r2) {
            assert!(r1.score > r2.score);
        }
    }

    #[test]
    fn test_empty_query() {
        let e1 = make_entry("Some content", vec![]);
        let entries = vec![&e1];
        let results = search("", &entries, 10);
        assert!(results.is_empty());
    }
}
