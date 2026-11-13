//! AI extraction usage reporting (`usage.summary`).

use payvy_domain::ai_usage::{format_usd, utc_month_bounds};
use payvy_store as store;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{CommandError, Cx};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SummaryInput {
    #[serde(default)]
    month: Option<String>,
}

pub async fn summary(cx: &mut Cx<'_>, input: SummaryInput) -> Result<Value, CommandError> {
    let (month, start, end) =
        utc_month_bounds(cx.now, input.month.as_deref()).map_err(CommandError::Invalid)?;
    let stale_before = cx.now - chrono::Duration::seconds(payvy_extract::REQUEST_TIMEOUT_SECS);
    let s = store::ai_usage_summary(&mut cx.tx, start, end, stale_before).await?;
    Ok(json!({
        "month": month,
        "period_start": start,
        "period_end": end,
        "attempts": s.attempts,
        "successful_reads": s.successful_reads,
        "failed_attempts": s.failed_attempts,
        "pending_attempts": s.pending_attempts,
        "unknown_usage_attempts": s.unknown_usage_attempts,
        "unpriced_attempts": s.unpriced_attempts,
        "input_tokens": s.input_tokens,
        "output_tokens": s.output_tokens,
        "cache_read_tokens": s.cache_read_tokens,
        "cache_write_tokens": s.cache_write_tokens,
        "estimated_cost_microcents": s.estimated_cost_microcents,
        "estimated_cost_usd": format_usd(s.estimated_cost_microcents),
        "currency": s.currency,
    }))
}
