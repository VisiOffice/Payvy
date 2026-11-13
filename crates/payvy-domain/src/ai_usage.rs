//! AI extraction usage accounting and versioned model pricing.
//!
//! Costs are computed once when an attempt completes, using integer microcents
//! (1 cent = 1,000,000 microcents; 1 USD = 100,000,000 microcents) so fractional
//! per-token rates stay exact without floating point. Unknown usage or an
//! unpriced model always stays `None` (`NULL`), never `0`.
//!
//! Not modelled in the price table: 1-hour cache writes (2x input), the 1.1x
//! US-only inference surcharge, and batch discounts.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

pub const CURRENCY: &str = "USD";
pub const PRICING_VERSION: &str = "2026-11-12-v2";

/// 1/1,000,000 of a cent (1/100,000,000 of a USD).
pub type MicroCents = i64;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenUsage {
    /// Non-cached input tokens (normalized across providers).
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
}

/// Per-million-token prices in USD millicents (1/1,000 of a cent, or
/// 1/100,000 of a USD; 1,000 millicents/MTok = 1 microcent/token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPricing {
    pub input_millicents_per_mtok: i64,
    pub output_millicents_per_mtok: i64,
    pub cache_read_millicents_per_mtok: i64,
    pub cache_write_millicents_per_mtok: i64,
}

impl ModelPricing {
    const fn from_millicents(input: i64, output: i64, cache_read: i64, cache_write: i64) -> Self {
        Self {
            input_millicents_per_mtok: input,
            output_millicents_per_mtok: output,
            cache_read_millicents_per_mtok: cache_read,
            cache_write_millicents_per_mtok: cache_write,
        }
    }
}

/// Matches an exact model ID or a dated/latest snapshot (`<base>-YYYY...`,
/// `<base>-latest`, or `<base>@...`) without matching sibling variants such as
/// `-mini`, `-nano`, `-pro`, or another minor version (e.g. `claude-opus-5-5`
/// vs `claude-opus-5`).
fn matches_model(m: &str, base: &str) -> bool {
    let Some(rest) = m.strip_prefix(base) else {
        return false;
    };
    if rest.is_empty() || rest.starts_with('@') {
        return true;
    }
    let Some(suffix) = rest.strip_prefix('-') else {
        return false;
    };
    suffix == "latest"
        || (suffix.len() >= 4 && suffix.as_bytes()[..4].iter().all(u8::is_ascii_digit))
}

const HAIKU_5_5_PROMPT_TIER_LIMIT: i64 = 100_000;

/// Versioned price table (base prompt tier). Unrecognized models return `None`
/// so unknown pricing is never recorded as zero cost.
pub fn price_for_model(model: &str) -> Option<ModelPricing> {
    price_for_model_with_prompt(model, 0)
}

/// Versioned price table with prompt-size tier selection (used by models such
/// as Haiku 5.5 whose rate changes above 100k input tokens).
pub fn price_for_model_with_prompt(model: &str, prompt_tokens: i64) -> Option<ModelPricing> {
    let m = model.trim().to_ascii_lowercase();
    let m = m.as_str();

    // Anthropic Claude — exact families ordered most-specific first, explicit cache rates.
    if matches_model(m, "claude-opus-5-5") {
        // $4 / $20 / $0.20 / $5 per MTok
        Some(ModelPricing::from_millicents(
            400_000, 2_000_000, 20_000, 500_000,
        ))
    } else if matches_model(m, "claude-opus-5")
        || matches_model(m, "claude-opus-4-8")
        || matches_model(m, "claude-opus-4-7")
        || matches_model(m, "claude-opus-4-6")
        || matches_model(m, "claude-opus-4-5")
    {
        // $5 / $25 / $0.50 / $6.25 per MTok
        Some(ModelPricing::from_millicents(
            500_000, 2_500_000, 50_000, 625_000,
        ))
    } else if matches_model(m, "claude-opus-4-1") || matches_model(m, "claude-opus-4") {
        // $15 / $75 / $1.50 / $18.75 per MTok
        Some(ModelPricing::from_millicents(
            1_500_000, 7_500_000, 150_000, 1_875_000,
        ))
    } else if matches_model(m, "claude-sonnet-5-5") {
        // $2 / $10 / $0.10 / $2.50 per MTok
        Some(ModelPricing::from_millicents(
            200_000, 1_000_000, 10_000, 250_000,
        ))
    } else if matches_model(m, "claude-sonnet-5") {
        // $2 / $10 / $0.20 / $2.50 per MTok
        Some(ModelPricing::from_millicents(
            200_000, 1_000_000, 20_000, 250_000,
        ))
    } else if matches_model(m, "claude-sonnet-4-6")
        || matches_model(m, "claude-sonnet-4-5")
        || matches_model(m, "claude-sonnet-4-1")
        || matches_model(m, "claude-sonnet-4")
    {
        // $3 / $15 / $0.30 / $3.75 per MTok
        Some(ModelPricing::from_millicents(
            300_000, 1_500_000, 30_000, 375_000,
        ))
    } else if matches_model(m, "claude-haiku-5-5") {
        if prompt_tokens > HAIKU_5_5_PROMPT_TIER_LIMIT {
            // > 100k prompt: $0.50 / $2.50 / $0.05 / $0.625 per MTok
            Some(ModelPricing::from_millicents(
                50_000, 250_000, 5_000, 62_500,
            ))
        } else {
            // <= 100k prompt: $0.10 / $0.50 / $0.01 / $0.125 per MTok
            Some(ModelPricing::from_millicents(10_000, 50_000, 1_000, 12_500))
        }
    } else if matches_model(m, "claude-haiku-4-5") {
        // $1 / $5 / $0.10 / $1.25 per MTok
        Some(ModelPricing::from_millicents(
            100_000, 500_000, 10_000, 125_000,
        ))
    } else if matches_model(m, "claude-haiku-3-5") {
        // $0.80 / $4 / $0.08 / $1.00 per MTok
        Some(ModelPricing::from_millicents(
            80_000, 400_000, 8_000, 100_000,
        ))
    // OpenAI — exact variants, dotted versions (5.1..5.6) intentionally unpriced until listed.
    } else if matches_model(m, "gpt-5-mini") {
        // $0.25 / $2 / $0.025 / $0.25 per MTok
        Some(ModelPricing::from_millicents(
            25_000, 200_000, 2_500, 25_000,
        ))
    } else if matches_model(m, "gpt-5-nano") {
        // $0.05 / $0.40 / $0.005 / $0.05 per MTok
        Some(ModelPricing::from_millicents(5_000, 40_000, 500, 5_000))
    } else if matches_model(m, "gpt-5-pro") {
        // $15 / $120 / $15 / $15 per MTok (no cached discount)
        Some(ModelPricing::from_millicents(
            1_500_000, 12_000_000, 1_500_000, 1_500_000,
        ))
    } else if matches_model(m, "gpt-5") {
        // $1.25 / $10 / $0.125 / $1.25 per MTok
        Some(ModelPricing::from_millicents(
            125_000, 1_000_000, 12_500, 125_000,
        ))
    } else if matches_model(m, "gpt-4.1-mini") {
        // $0.40 / $1.60 / $0.10 / $0.40 per MTok
        Some(ModelPricing::from_millicents(
            40_000, 160_000, 10_000, 40_000,
        ))
    } else if matches_model(m, "gpt-4.1-nano") {
        // $0.10 / $0.40 / $0.025 / $0.10 per MTok
        Some(ModelPricing::from_millicents(10_000, 40_000, 2_500, 10_000))
    } else if matches_model(m, "gpt-4.1") {
        // $2 / $8 / $0.50 / $2 per MTok
        Some(ModelPricing::from_millicents(
            200_000, 800_000, 50_000, 200_000,
        ))
    // xAI — bare grok-4 only.
    } else if m == "grok-4" {
        Some(ModelPricing::from_millicents(
            300_000, 1_500_000, 75_000, 300_000,
        ))
    } else {
        None
    }
}

/// Computes `(pricing_version, cost_microcents)` when both the model price and
/// base token usage (`input_tokens` and `output_tokens`) are known.
pub fn compute_cost_microcents(
    model: &str,
    usage: &TokenUsage,
) -> Option<(&'static str, MicroCents)> {
    let input = usage.input_tokens?;
    let output = usage.output_tokens?;
    if input < 0 || output < 0 {
        return None;
    }
    let cache_read = usage.cache_read_tokens.unwrap_or(0);
    let cache_write = usage.cache_write_tokens.unwrap_or(0);
    if cache_read < 0 || cache_write < 0 {
        return None;
    }
    let prompt_tokens = input.checked_add(cache_read)?.checked_add(cache_write)?;
    let pricing = price_for_model_with_prompt(model, prompt_tokens)?;

    // 1,000 millicents / MTok = 1 microcent / token.
    let total_thousandths = i128::from(input) * i128::from(pricing.input_millicents_per_mtok)
        + i128::from(output) * i128::from(pricing.output_millicents_per_mtok)
        + i128::from(cache_read) * i128::from(pricing.cache_read_millicents_per_mtok)
        + i128::from(cache_write) * i128::from(pricing.cache_write_millicents_per_mtok);
    let microcents = i64::try_from((total_thousandths + 500) / 1_000).ok()?;
    Some((PRICING_VERSION, microcents))
}

/// Formats microcents as a USD decimal string (up to 8 decimal places, trimmed
/// to at least 2 decimal places), using integer math only.
pub fn format_usd(microcents: MicroCents) -> String {
    let negative = microcents < 0;
    let abs = i128::from(microcents).unsigned_abs();
    let dollars = abs / 100_000_000;
    let frac = abs % 100_000_000;
    let mut frac_str = format!("{frac:08}");
    while frac_str.len() > 2 && frac_str.ends_with('0') {
        frac_str.pop();
    }
    if negative {
        format!("-{dollars}.{frac_str}")
    } else {
        format!("{dollars}.{frac_str}")
    }
}

/// Returns `(month_yyyy_mm, start_utc_inclusive, end_utc_exclusive)`.
pub fn utc_month_bounds(
    now: DateTime<Utc>,
    month: Option<&str>,
) -> Result<(String, DateTime<Utc>, DateTime<Utc>), String> {
    let (year, m) = match month {
        None => (now.year(), now.month()),
        Some(raw) => {
            let trimmed = raw.trim();
            let (y, mo) = trimmed
                .split_once('-')
                .ok_or_else(|| "month must be YYYY-MM".to_owned())?;
            if y.len() != 4 || mo.len() != 2 {
                return Err("month must be YYYY-MM".into());
            }
            let year: i32 = y.parse().map_err(|_| "month must be YYYY-MM".to_owned())?;
            let mo: u32 = mo.parse().map_err(|_| "month must be YYYY-MM".to_owned())?;
            if !(1..=12).contains(&mo) {
                return Err("month must be between 01 and 12".into());
            }
            (year, mo)
        }
    };
    let start_date =
        NaiveDate::from_ymd_opt(year, m, 1).ok_or_else(|| "invalid month".to_owned())?;
    let (next_year, next_m) = if m == 12 {
        (year + 1, 1)
    } else {
        (year, m + 1)
    };
    let end_date =
        NaiveDate::from_ymd_opt(next_year, next_m, 1).ok_or_else(|| "invalid month".to_owned())?;
    let start = Utc.from_utc_datetime(&start_date.and_hms_opt(0, 0, 0).unwrap());
    let end = Utc.from_utc_datetime(&end_date.and_hms_opt(0, 0, 0).unwrap());
    Ok((format!("{year:04}-{m:02}"), start, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_model_computes_exact_microcents() {
        let usage = TokenUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(200),
            cache_read_tokens: Some(500),
            cache_write_tokens: Some(100),
        };
        // claude-sonnet-5-5: $2 / $10 / $0.10 / $2.50 per MTok
        // = 200 / 1000 / 10 / 250 microcents per token
        let (ver, cost) = compute_cost_microcents("claude-sonnet-5-5", &usage).unwrap();
        assert_eq!(ver, PRICING_VERSION);
        assert_eq!(cost, 1_000 * 200 + 200 * 1_000 + 500 * 10 + 100 * 250);
        assert_eq!(cost, 430_000);
        assert_eq!(format_usd(cost), "0.0043");
    }

    #[test]
    fn opus_and_sonnet_families_match_exact_rates() {
        let usage = TokenUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(100),
            cache_read_tokens: Some(200),
            cache_write_tokens: Some(100),
        };
        // Opus 5.5: $4 / $20 / $0.20 / $5
        let (_, opus_5_5) = compute_cost_microcents("claude-opus-5-5", &usage).unwrap();
        assert_eq!(opus_5_5, 1_000 * 400 + 100 * 2_000 + 200 * 20 + 100 * 500);

        // Opus 5 (dated): $5 / $25 / $0.50 / $6.25
        let (_, opus_5) = compute_cost_microcents("claude-opus-5-20260101", &usage).unwrap();
        assert_eq!(opus_5, 1_000 * 500 + 100 * 2_500 + 200 * 50 + 100 * 625);

        // Opus 4.7 and 4.8 use $5 / $25, not Opus 4's $15 / $75
        let (_, opus_4_7) = compute_cost_microcents("claude-opus-4-7", &usage).unwrap();
        let (_, opus_4_8) = compute_cost_microcents("claude-opus-4-8", &usage).unwrap();
        let (_, opus_4) = compute_cost_microcents("claude-opus-4", &usage).unwrap();
        assert_eq!(opus_4_7, opus_5);
        assert_eq!(opus_4_8, opus_5);
        assert_eq!(
            opus_4,
            1_000 * 1_500 + 100 * 7_500 + 200 * 150 + 100 * 1_875
        );

        // Sonnet 5 has $0.20 cache read vs Sonnet 5.5's $0.10 cache read
        let (_, sonnet_5) = compute_cost_microcents("claude-sonnet-5", &usage).unwrap();
        assert_eq!(sonnet_5, 1_000 * 200 + 100 * 1_000 + 200 * 20 + 100 * 250);
    }

    #[test]
    fn haiku_5_5_prompt_tiers_and_fractional_cache_write() {
        let small = TokenUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(200),
            cache_read_tokens: Some(500),
            cache_write_tokens: Some(100),
        };
        // <= 100k prompt: $0.10 / $0.50 / $0.01 / $0.125 per MTok
        let (_, small_cost) = compute_cost_microcents("claude-haiku-5-5", &small).unwrap();
        // 1000*10 + 200*50 + 500*1 + 100*12.5 = 10000 + 10000 + 500 + 1250 = 21750
        assert_eq!(small_cost, 21_750);

        let large = TokenUsage {
            input_tokens: Some(90_000),
            output_tokens: Some(1_000),
            cache_read_tokens: Some(10_001),
            cache_write_tokens: Some(0),
        };
        // > 100k prompt (90k + 10,001 = 100,001): $0.50 / $2.50 / $0.05 / $0.625 per MTok
        let (_, large_cost) = compute_cost_microcents("claude-haiku-5-5", &large).unwrap();
        assert_eq!(large_cost, 90_000 * 50 + 1_000 * 250 + 10_001 * 5);
    }

    #[test]
    fn openai_variants_do_not_fall_through_to_base_model() {
        let usage = TokenUsage {
            input_tokens: Some(1_000),
            output_tokens: Some(1_000),
            cache_read_tokens: Some(1_000),
            cache_write_tokens: None,
        };
        let (_, gpt5) = compute_cost_microcents("gpt-5", &usage).unwrap();
        assert_eq!(gpt5, 1_000 * 125 + 1_000 * 1_000 + 12_500);

        let (_, mini) = compute_cost_microcents("gpt-5-mini-2025-08-07", &usage).unwrap();
        assert_eq!(mini, 1_000 * 25 + 1_000 * 200 + 2_500);

        let (_, nano) = compute_cost_microcents("gpt-5-nano", &usage).unwrap();
        assert_eq!(nano, 1_000 * 5 + 1_000 * 40 + 500);

        let (_, pro) = compute_cost_microcents("gpt-5-pro", &usage).unwrap();
        assert_eq!(pro, 1_000 * 1_500 + 1_000 * 12_000 + 1_000 * 1_500);

        let (_, g41_mini) = compute_cost_microcents("gpt-4.1-mini", &usage).unwrap();
        assert_eq!(g41_mini, 1_000 * 40 + 1_000 * 160 + 1_000 * 10);
    }

    #[test]
    fn unknown_model_and_unknown_usage_stay_none() {
        let usage = TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            ..Default::default()
        };
        assert!(compute_cost_microcents("llama-local-8b", &usage).is_none());
        assert!(compute_cost_microcents("gpt-5.1", &usage).is_none());
        assert!(compute_cost_microcents("gpt-5.4", &usage).is_none());
        assert!(compute_cost_microcents("grok-4-fast", &usage).is_none());

        let missing = TokenUsage::default();
        assert!(compute_cost_microcents("claude-opus-5-5", &missing).is_none());
    }

    #[test]
    fn utc_month_bounds_handles_december_rollover() {
        let now = Utc.with_ymd_and_hms(2026, 11, 12, 23, 0, 0).unwrap();
        let (label, start, end) = utc_month_bounds(now, Some("2026-12")).unwrap();
        assert_eq!(label, "2026-12");
        assert_eq!(start, Utc.with_ymd_and_hms(2026, 12, 1, 0, 0, 0).unwrap());
        assert_eq!(end, Utc.with_ymd_and_hms(2027, 1, 1, 0, 0, 0).unwrap());

        let (default_label, _, _) = utc_month_bounds(now, None).unwrap();
        assert_eq!(default_label, "2026-11");
    }
}
