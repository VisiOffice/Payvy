use chrono::{DateTime, Utc};
use payvy_domain::ai_usage::{CURRENCY, MicroCents, TokenUsage};
use sqlx::Row;
use uuid::Uuid;

use crate::{ActorKind, Tx};

pub struct NewAiUsageAttempt<'a> {
    pub document_id: Uuid,
    pub actor_kind: ActorKind,
    pub actor_name: &'a str,
    pub provider: &'a str,
    pub requested_model: &'a str,
    pub started_at: DateTime<Utc>,
}

pub struct CompleteAiUsageAttempt<'a> {
    pub outcome: &'a str,
    pub actual_model: Option<&'a str>,
    pub provider_request_id: Option<&'a str>,
    pub error: Option<&'a str>,
    pub usage: TokenUsage,
    pub pricing_version: Option<&'a str>,
    pub cost_microcents: Option<MicroCents>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiUsageAttempt {
    pub id: Uuid,
    pub org_id: Uuid,
    pub document_id: Option<Uuid>,
    pub actor_kind: String,
    pub actor_name: String,
    pub provider: String,
    pub requested_model: String,
    pub actual_model: Option<String>,
    pub provider_request_id: Option<String>,
    pub outcome: String,
    pub error: Option<String>,
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub cache_write_tokens: Option<i64>,
    pub currency: String,
    pub pricing_version: Option<String>,
    pub cost_microcents: Option<MicroCents>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiUsageSummary {
    pub attempts: i64,
    pub successful_reads: i64,
    pub failed_attempts: i64,
    pub pending_attempts: i64,
    pub unknown_usage_attempts: i64,
    pub unpriced_attempts: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_write_tokens: i64,
    pub estimated_cost_microcents: MicroCents,
    pub currency: String,
}

/// Inserts a durable `'pending'` attempt row before provider dispatch.
pub async fn start_ai_usage_attempt(
    tx: &mut Tx,
    org: Uuid,
    new: &NewAiUsageAttempt<'_>,
) -> anyhow::Result<Uuid> {
    let id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO ai_usage_attempts \
         (id, org_id, document_id, actor_kind, actor_name, provider, requested_model, outcome, currency, started_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', $8, $9)",
    )
    .bind(id)
    .bind(org)
    .bind(new.document_id)
    .bind(new.actor_kind.as_str())
    .bind(new.actor_name)
    .bind(new.provider)
    .bind(new.requested_model)
    .bind(CURRENCY)
    .bind(new.started_at)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Completes a `'pending'` attempt. Returns `true` if this call recorded the
/// completion, or `false` if the attempt was already completed (safe on duplicate).
pub async fn complete_ai_usage_attempt(
    tx: &mut Tx,
    id: Uuid,
    done: &CompleteAiUsageAttempt<'_>,
) -> anyhow::Result<bool> {
    let res = sqlx::query(
        "UPDATE ai_usage_attempts SET \
         outcome = $2, \
         actual_model = $3, \
         provider_request_id = $4, \
         error = $5, \
         input_tokens = $6, \
         output_tokens = $7, \
         cache_read_tokens = $8, \
         cache_write_tokens = $9, \
         pricing_version = $10, \
         cost_microcents = $11, \
         completed_at = $12 \
         WHERE id = $1 AND outcome = 'pending'",
    )
    .bind(id)
    .bind(done.outcome)
    .bind(done.actual_model)
    .bind(done.provider_request_id)
    .bind(done.error)
    .bind(done.usage.input_tokens)
    .bind(done.usage.output_tokens)
    .bind(done.usage.cache_read_tokens)
    .bind(done.usage.cache_write_tokens)
    .bind(done.pricing_version)
    .bind(done.cost_microcents)
    .bind(done.completed_at)
    .execute(&mut **tx)
    .await?;
    Ok(res.rows_affected() == 1)
}

pub async fn get_ai_usage_attempt(tx: &mut Tx, id: Uuid) -> anyhow::Result<Option<AiUsageAttempt>> {
    let row = sqlx::query(
        "SELECT id, org_id, document_id, actor_kind, actor_name, provider, requested_model, \
         actual_model, provider_request_id, outcome, error, input_tokens, output_tokens, \
         cache_read_tokens, cache_write_tokens, currency, pricing_version, cost_microcents, \
         started_at, completed_at \
         FROM ai_usage_attempts WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&mut **tx)
    .await?;

    Ok(row.map(|r| AiUsageAttempt {
        id: r.get("id"),
        org_id: r.get("org_id"),
        document_id: r.get("document_id"),
        actor_kind: r.get("actor_kind"),
        actor_name: r.get("actor_name"),
        provider: r.get("provider"),
        requested_model: r.get("requested_model"),
        actual_model: r.get("actual_model"),
        provider_request_id: r.get("provider_request_id"),
        outcome: r.get("outcome"),
        error: r.get("error"),
        input_tokens: r.get("input_tokens"),
        output_tokens: r.get("output_tokens"),
        cache_read_tokens: r.get("cache_read_tokens"),
        cache_write_tokens: r.get("cache_write_tokens"),
        currency: r.get("currency"),
        pricing_version: r.get("pricing_version"),
        cost_microcents: r.get("cost_microcents"),
        started_at: r.get("started_at"),
        completed_at: r.get("completed_at"),
    }))
}

/// Monthly usage summary for the current organization (enforced by RLS).
/// Attempts still `pending` with `started_at < stale_before` (past the
/// provider timeout, e.g. from a crash) are counted as unknown rather than
/// pending.
pub async fn ai_usage_summary(
    tx: &mut Tx,
    start_inclusive: DateTime<Utc>,
    end_exclusive: DateTime<Utc>,
    stale_before: DateTime<Utc>,
) -> anyhow::Result<AiUsageSummary> {
    let row = sqlx::query(
        "SELECT \
         COUNT(*)::bigint AS attempts, \
         COUNT(*) FILTER (WHERE outcome = 'succeeded')::bigint AS successful_reads, \
         COUNT(*) FILTER (WHERE outcome NOT IN ('pending', 'succeeded'))::bigint AS failed_attempts, \
         COUNT(*) FILTER (WHERE outcome = 'pending' AND started_at >= $3)::bigint AS pending_attempts, \
         COUNT(*) FILTER (WHERE (outcome <> 'pending' AND (input_tokens IS NULL OR output_tokens IS NULL)) OR (outcome = 'pending' AND started_at < $3))::bigint AS unknown_usage_attempts, \
         COUNT(*) FILTER (WHERE outcome <> 'pending' AND input_tokens IS NOT NULL AND output_tokens IS NOT NULL AND cost_microcents IS NULL)::bigint AS unpriced_attempts, \
         COALESCE(SUM(input_tokens), 0)::bigint AS input_tokens, \
         COALESCE(SUM(output_tokens), 0)::bigint AS output_tokens, \
         COALESCE(SUM(cache_read_tokens), 0)::bigint AS cache_read_tokens, \
         COALESCE(SUM(cache_write_tokens), 0)::bigint AS cache_write_tokens, \
         COALESCE(SUM(cost_microcents), 0)::bigint AS estimated_cost_microcents \
         FROM ai_usage_attempts \
         WHERE started_at >= $1 AND started_at < $2",
    )
    .bind(start_inclusive)
    .bind(end_exclusive)
    .bind(stale_before)
    .fetch_one(&mut **tx)
    .await?;

    Ok(AiUsageSummary {
        attempts: row.get("attempts"),
        successful_reads: row.get("successful_reads"),
        failed_attempts: row.get("failed_attempts"),
        pending_attempts: row.get("pending_attempts"),
        unknown_usage_attempts: row.get("unknown_usage_attempts"),
        unpriced_attempts: row.get("unpriced_attempts"),
        input_tokens: row.get("input_tokens"),
        output_tokens: row.get("output_tokens"),
        cache_read_tokens: row.get("cache_read_tokens"),
        cache_write_tokens: row.get("cache_write_tokens"),
        estimated_cost_microcents: row.get("estimated_cost_microcents"),
        currency: CURRENCY.to_owned(),
    })
}
