//! Token pricing (FR-4.2): turn a build's token usage into a dollar
//! estimate.
//!
//! The built-in rates are ballpark per-million-token prices for the
//! Anthropic model families, matched by family name so a versioned id like
//! `claude-opus-4-8` still resolves. Provider prices move and this isn't a
//! billing system, so the number is always an *estimate* — and the
//! `[prices]` config table overrides any model's rate when you want exact
//! figures. An unknown model is priced as `None` (shown as "—"), never
//! guessed.
//!
//! One honest caveat the caller should surface: a build's recorded `model`
//! is its tailoring (smart-tier) model, but a run also spends a little on
//! cheaper tiers (parsing, gap) and the interview tier. Charging the whole
//! total at the tailoring rate slightly overstates — fine for a budget
//! signal, but it's an estimate, not an invoice.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm::TokenUsage;

/// Dollars per million tokens, input and output.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
}

/// Built-in ballpark rates, matched by model family. `None` for a family we
/// don't recognize, so the caller shows "—" rather than a wrong number.
pub fn default_price(model: &str) -> Option<Price> {
    let m = model.to_lowercase();
    let price = |input_per_mtok, output_per_mtok| {
        Some(Price {
            input_per_mtok,
            output_per_mtok,
        })
    };
    if m.contains("opus") {
        price(15.0, 75.0)
    } else if m.contains("sonnet") {
        price(3.0, 15.0)
    } else if m.contains("haiku") {
        price(1.0, 5.0)
    } else {
        None
    }
}

/// The dollar cost of `usage` at `model`'s rate: a config override first,
/// then the built-in family default. `None` when the model isn't priced.
pub fn cost_usd(
    model: &str,
    usage: &TokenUsage,
    overrides: &BTreeMap<String, Price>,
) -> Option<f64> {
    let price = overrides
        .get(model)
        .copied()
        .or_else(|| default_price(model))?;
    let m = 1_000_000.0;
    Some(
        usage.input_tokens as f64 / m * price.input_per_mtok
            + usage.output_tokens as f64 / m * price.output_per_mtok,
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn rates_resolve_by_family_not_exact_id() {
        assert!(default_price("claude-opus-4-8").is_some());
        assert!(default_price("claude-sonnet-4-6").is_some());
        assert!(default_price("claude-haiku-4-5-20251001").is_some());
        // Opus costs more than Sonnet costs more than Haiku.
        assert!(
            default_price("claude-opus-4-8").unwrap().output_per_mtok
                > default_price("claude-sonnet-4-6").unwrap().output_per_mtok
        );
    }

    #[test]
    fn an_unknown_model_is_unpriced() {
        assert!(default_price("some-local-llama").is_none());
        let usage = TokenUsage {
            input_tokens: 1000,
            output_tokens: 1000,
        };
        assert!(cost_usd("some-local-llama", &usage, &BTreeMap::new()).is_none());
    }

    #[test]
    fn cost_is_tokens_times_rate() {
        // 1M in + 1M out on Sonnet (3 / 15) = $18.
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        };
        let cost = cost_usd("claude-sonnet-4-6", &usage, &BTreeMap::new()).unwrap();
        assert!((cost - 18.0).abs() < 1e-9);
    }

    #[test]
    fn a_config_override_wins_over_the_default() {
        let mut overrides = BTreeMap::new();
        overrides.insert(
            "claude-opus-4-8".to_string(),
            Price {
                input_per_mtok: 1.0,
                output_per_mtok: 1.0,
            },
        );
        let usage = TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
        };
        // Override -> $2, not the default Opus $90.
        let cost = cost_usd("claude-opus-4-8", &usage, &overrides).unwrap();
        assert!((cost - 2.0).abs() < 1e-9);
    }
}
