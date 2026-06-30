//! Keyword normalization shared by the verification triage and the
//! phrase-mirroring service. Both need to decide when two differently
//! worded keywords mean the same thing — "people management" vs "people
//! manager", "AI-powered products" vs "AI-Powered Product Development" —
//! without a real NLP stack. `keyword_key` is that decision, reduced to
//! a comparable token set.

/// Words that don't distinguish one keyword from another — seniority,
/// and filler the JD pads phrases with. Dropped before comparison so
/// "senior engineering manager" and "engineering manager" match.
const KEYWORD_NOISE: &[&str] = &[
    "sr",
    "snr",
    "senior",
    "jr",
    "junior",
    "lead",
    "staff",
    "principal",
    "mid",
    "entry",
    "level",
    "experience",
    "industry",
    "knowledge",
    "expertise",
    "the",
    "a",
    "an",
    "of",
    "in",
    "and",
    "or",
    "with",
];

/// A comparison key that collapses near-duplicate keywords: lowercase,
/// drop the noise words, light-stem each remaining word so inflections
/// fold together ("management"/"manager" -> "manag"), then sort so word
/// order doesn't matter. "people management", "people manager", "sr
/// engineering manager" and "engineering manager" reduce to keys that
/// dedupe down to two distinct concepts, not four. It's a heuristic, not
/// a stemmer — good enough to thin an interview or gate a mirror, not a
/// search index.
pub fn keyword_key(name: &str) -> Vec<String> {
    let mut tokens: Vec<String> = name
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .filter(|w| !KEYWORD_NOISE.contains(&w.as_str()))
        .map(|w| stem(&w))
        .filter(|w| !w.is_empty())
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}

/// True when every meaningful token of `phrase` also appears in `skill`'s
/// token set — the phrase names the same competency the skill does, in
/// fewer or reordered words ("AI-powered products" inside "AI-Powered
/// Product Development"). The shared gate behind both phrase mirroring
/// (`mirror.rs`) and gap matching (`gap.rs`): a phrase that subsets an
/// evidence-backed skill is something the user demonstrably has, just
/// worded differently. An empty phrase never matches — it would subset
/// everything.
pub(crate) fn is_token_subset(phrase: &[String], skill: &[String]) -> bool {
    !phrase.is_empty() && phrase.iter().all(|t| skill.contains(t))
}

/// A deliberately crude stem: strip one common suffix, then a trailing
/// "e", so "manage"/"manager"/"management"/"managing" all land on
/// "manag". Only touches words long enough that the stub stays
/// recognizable.
fn stem(word: &str) -> String {
    // Longest suffix first, so "ments" wins over "s".
    for suffix in ["ments", "ment", "ings", "ing", "ers", "er", "ed", "es", "s"] {
        if word.len() > suffix.len() + 2 && word.ends_with(suffix) {
            return word[..word.len() - suffix.len()]
                .trim_end_matches('e')
                .to_string();
        }
    }
    word.trim_end_matches('e').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewordings_collapse_but_distinct_concepts_stay_apart() {
        assert_eq!(
            keyword_key("people management"),
            keyword_key("people manager")
        );
        assert_eq!(
            keyword_key("engineering manager"),
            keyword_key("Sr Engineering Manager")
        );
        assert_ne!(
            keyword_key("team management"),
            keyword_key("people management")
        );
    }

    #[test]
    fn a_phrase_can_be_a_token_subset_of_a_skill() {
        // "AI-powered products" is a subset of the recorded skill's words.
        let phrase = keyword_key("AI-powered products");
        let skill = keyword_key("AI-Powered Product Development");
        assert!(phrase.iter().all(|t| skill.contains(t)));
        assert_ne!(phrase, skill); // subset, not equal
    }
}
