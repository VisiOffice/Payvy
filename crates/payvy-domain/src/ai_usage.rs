//! AI extraction usage accounting and versioned model pricing.
//!
//! Costs are computed once when an attempt completes, using integer microcents
//! (1 cent = 1,000,000 microcents; 1 USD = 100,000,000 microcents) so fractional
//! per-token rates stay exact without floating point. Unknown usage or an
//! unpriced model always stays `None` (`NULL`), never `0`.

use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Serialize};

pub const CURRENCY: &str = "USD";
pub const PRICING_VERSION: &str = "2026-11-12-v1";

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

/// Per-million-token prices in USD cents (1 cent/MTok = 1 microcent/token).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelPricing {
    pub input_cents_per_mtok: i64,
    pub output_cents_per_mtok: i64,
    pub cache_read_cents_per_mtok: i64,
    pub cache_write_cents_per_mtok: i64,
}

impl ModelPricing {
    const fn new(input_cents: i64, output_cents: i64) -> Self {
        Self {
            input_cents_per_mtok: input_cents,
            output_cents_per_mtok: output_cents,
            cache_read_cents_per_mtok: input_cents / 10,
            cache_write_cents_per_mtok: (input_cents * 5) / 4,
        }
    }
}

/// Versioned price table. Unrecognized models return `None` so unknown pricing
/// is never recorded as zero cost.
pub fn price_for_model(model: &str) -> Option<ModelPricing> {
    let m = model.trim().to_ascii_lowercase();
    let m = m.as_str();
    if m.starts_with("claude-opus-5")
        || m.starts_with("claude-opus-4-5")
        || m.starts_with("claude-opus-4-6")
    {
        Some(ModelPricing::new(500, 2500))
    } else if m.starts_with("claude-opus-4") {
        Some(ModelPricing::new(1500, 7500))
    } else if m.starts_with("claude-sonnet-5") || m.starts_with("claude-sonnet-4") {
        Some(ModelPricing::new(300, 1500))
    } else if m.starts_with("claude-haiku-5") || m.starts_with("claude-haiku-4-5") {
        Some(ModelPricing::new(100, 500))
    } else if m.starts_with("claude-haiku-3-5") {
        Some(ModelPricing::new(80, 400))
    } else if m == "gpt-5" || m.starts_with("gpt-5-") {
        Some(ModelPricing {
            input_cents_per_mtok: 125,
            output_cents_per_mtok: 1000,
            cache_read_cents_per_mtok: 13,
            cache_write_cents_per_mtok: 125,
        })
    } else if m == "gpt-4.1" || m.starts_with("gpt-4.1-") {
        Some(ModelPricing {
            input_cents_per_mtok: 200,
            output_cents_per_mtok: 800,
            cache_read_cents_per_mtok: 50,
            cache_write_cents_per_mtok: 200,
        })
    } else if m == "grok-4" || m.starts_with("grok-4-") {
        Some(ModelPricing {
            input_cents_per_mtok: 300,
            output_cents_per_mtok: 1500,
            cache_read_cents_per_mtok: 75,
            cache_write_cents_per_mtok: 300,
        })
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
    let pricing = price_for_model(model)?;
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

    // 1 cent / MTok = 1,000,000 microcents / 1,000,000 tokens = 1 microcent / token.
    let total = i128::from(input) * i128::from(pricing.input_cents_per_mtok)
        + i128::from(output) * i128::from(pricing.output_cents_per_mtok)
        + i128::from(cache_read) * i128::from(pricing.cache_read_cents_per_mtok)
        + i128::from(cache_write) * i128::from(pricing.cache_write_cents_per_mtok);
    let microcents = i64::try_from(total).ok()?;
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
        // claude-sonnet-5-5: input 300, output 1500, cache_read 30, cache_write 375
        let (ver, cost) = compute_cost_microcents("claude-sonnet-5-5", &usage).unwrap();
        assert_eq!(ver, PRICING_VERSION);
        assert_eq!(cost, 1_000 * 300 + 200 * 1500 + 500 * 30 + 100 * 375);
        assert_eq!(cost, 652_500);
        assert_eq!(format_usd(cost), "0.006525");
    }

    #[test]
    fn unknown_model_and_unknown_usage_stay_none() {
        let usage = TokenUsage {
            input_tokens: Some(100),
            output_tokens: Some(50),
            ..Default::default()
        };
        assert!(compute_cost_microcents("llama-local-8b", &usage).is_none());

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
