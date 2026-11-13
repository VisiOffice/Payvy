//! Cross-company operator storage: idempotent organization provisioning,
//! organization listing, and cross-company AI usage aggregation.

use chrono::{DateTime, Utc};
use payvy_domain::ai_usage::CURRENCY;
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{
    AiUsageSummary, NewPerson, Organization, Person, Role, Store, actor_roles, append_event,
    insert_person, organization, set_actor_roles,
};

pub struct ProvisionOrgInput<'a> {
    pub idempotency_key: &'a str,
    pub request_hash: &'a str,
    pub org_name: &'a str,
    pub admin_actor: &'a str,
    pub admin_email: &'a str,
    pub admin_name: &'a str,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct ProvisionedOrg {
    pub created: bool,
    pub organization: Organization,
    pub admin: Person,
    pub admin_roles: Vec<Role>,
}

#[derive(Debug, Clone)]
pub enum ProvisionOrgOutcome {
    Ok(Box<ProvisionedOrg>),
    IdempotencyConflict,
}

#[derive(Debug, Clone, Serialize)]
pub struct OrganizationSummary {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub people_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrgAiUsageSummary {
    pub org_id: Uuid,
    pub org_name: String,
    pub summary: AiUsageSummary,
}

impl Store {
    /// Creates an organization, its first Admin person, role grant, and audit
    /// events in one transaction, idempotent on `idempotency_key`.
    pub async fn provision_organization(
        &self,
        input: &ProvisionOrgInput<'_>,
        preview: bool,
    ) -> anyhow::Result<ProvisionOrgOutcome> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended('payvy.org_create.' || $1, 0))")
            .bind(input.idempotency_key)
            .execute(&mut *tx)
            .await?;

        let existing = sqlx::query(
            "SELECT request_hash, org_id, admin_person_id \
             FROM org_provisioning WHERE idempotency_key = $1",
        )
        .bind(input.idempotency_key)
        .fetch_optional(&mut *tx)
        .await?;

        if let Some(row) = existing {
            let stored_hash: String = row.get("request_hash");
            if stored_hash != input.request_hash {
                tx.rollback().await?;
                return Ok(ProvisionOrgOutcome::IdempotencyConflict);
            }
            let org_id: Uuid = row.get("org_id");
            let admin_person_id: Uuid = row.get("admin_person_id");
            sqlx::query("SELECT set_config('payvy.org_id', $1, true)")
                .bind(org_id.to_string())
                .execute(&mut *tx)
                .await?;
            let org = organization(&mut tx, org_id)
                .await?
                .ok_or_else(|| anyhow::anyhow!("provisioned organization {org_id} missing"))?;
            let p_row = sqlx::query(
                "SELECT id, org_id, actor, email, name, created_at, created_by, removed_at \
                 FROM people WHERE id = $1",
            )
            .bind(admin_person_id)
            .fetch_one(&mut *tx)
            .await?;
            let removed_at: Option<DateTime<Utc>> = p_row.get("removed_at");
            let admin = Person {
                id: p_row.get("id"),
                org_id: p_row.get("org_id"),
                actor: p_row.get("actor"),
                email: p_row.get("email"),
                name: p_row.get("name"),
                created_at: p_row.get("created_at"),
                created_by: p_row.get("created_by"),
            };
            let roles = if removed_at.is_none() {
                actor_roles(&mut tx, org_id, &admin.actor).await?
            } else {
                Vec::new()
            };
            tx.rollback().await?;
            return Ok(ProvisionOrgOutcome::Ok(Box::new(ProvisionedOrg {
                created: false,
                organization: org,
                admin,
                admin_roles: roles,
            })));
        }

        let org_id = Uuid::new_v4();
        sqlx::query("SELECT set_config('payvy.org_id', $1, true)")
            .bind(org_id.to_string())
            .execute(&mut *tx)
            .await?;

        let org_row = sqlx::query(
            "INSERT INTO organizations (id, name, created_at) VALUES ($1, $2, $3) \
             RETURNING id, name, created_at",
        )
        .bind(org_id)
        .bind(input.org_name)
        .bind(input.now)
        .fetch_one(&mut *tx)
        .await?;
        let org = Organization {
            id: org_row.get("id"),
            name: org_row.get("name"),
            created_at: org_row.get("created_at"),
        };

        let admin = insert_person(
            &mut tx,
            &NewPerson {
                org_id,
                actor: input.admin_actor,
                email: input.admin_email,
                name: input.admin_name,
                created_at: input.now,
                created_by: "operator",
            },
        )
        .await?
        .ok_or_else(|| anyhow::anyhow!("new organization already had a conflicting person"))?;

        let admin_roles = vec![Role::Admin];
        set_actor_roles(&mut tx, org_id, &admin.actor, &admin_roles).await?;

        sqlx::query(
            "INSERT INTO org_provisioning \
             (idempotency_key, request_hash, org_id, admin_person_id, created_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(input.idempotency_key)
        .bind(input.request_hash)
        .bind(org_id)
        .bind(admin.id)
        .bind(input.now)
        .execute(&mut *tx)
        .await?;

        append_event(
            &mut tx,
            org_id,
            "org.created",
            Some(org_id),
            "operator",
            &serde_json::json!({
                "name": org.name,
                "admin_actor": admin.actor,
                "admin_email": admin.email,
            }),
        )
        .await?;

        append_event(
            &mut tx,
            org_id,
            "person.added",
            Some(admin.id),
            "operator",
            &serde_json::json!({
                "actor": admin.actor,
                "email": admin.email,
                "name": admin.name,
                "roles": &admin_roles,
            }),
        )
        .await?;

        if preview {
            tx.rollback().await?;
        } else {
            tx.commit().await?;
        }

        Ok(ProvisionOrgOutcome::Ok(Box::new(ProvisionedOrg {
            created: true,
            organization: org,
            admin,
            admin_roles,
        })))
    }

    pub async fn list_organizations(&self, limit: i64) -> anyhow::Result<Vec<OrganizationSummary>> {
        let rows = sqlx::query(
            "SELECT o.id, o.name, o.created_at, COUNT(p.id)::bigint AS people_count \
             FROM organizations o \
             LEFT JOIN people p ON p.org_id = o.id AND p.removed_at IS NULL \
             GROUP BY o.id, o.name, o.created_at \
             ORDER BY o.created_at ASC, o.id ASC \
             LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| OrganizationSummary {
                id: r.get("id"),
                name: r.get("name"),
                created_at: r.get("created_at"),
                people_count: r.get("people_count"),
            })
            .collect())
    }

    /// Cross-company monthly AI usage summary for the operator API.
    /// Runs in a read-only transaction with transaction-local `payvy.operator`.
    /// Returns `Ok(None)` when `org_filter` names an organization that does not exist.
    pub async fn operator_ai_usage_summary(
        &self,
        org_filter: Option<Uuid>,
        start_inclusive: DateTime<Utc>,
        end_exclusive: DateTime<Utc>,
        stale_before: DateTime<Utc>,
    ) -> anyhow::Result<Option<(AiUsageSummary, Vec<OrgAiUsageSummary>)>> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION READ ONLY")
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT set_config('payvy.operator', 'true', true)")
            .execute(&mut *tx)
            .await?;

        let rows = sqlx::query(
            "SELECT \
             o.id AS org_id, \
             o.name AS org_name, \
             COUNT(a.id)::bigint AS attempts, \
             COUNT(a.id) FILTER (WHERE a.outcome = 'succeeded')::bigint AS successful_reads, \
             COUNT(a.id) FILTER (WHERE a.outcome NOT IN ('pending', 'succeeded'))::bigint AS failed_attempts, \
             COUNT(a.id) FILTER (WHERE a.outcome = 'pending' AND a.started_at >= $3)::bigint AS pending_attempts, \
             COUNT(a.id) FILTER (WHERE (a.outcome <> 'pending' AND (a.input_tokens IS NULL OR a.output_tokens IS NULL)) OR (a.outcome = 'pending' AND a.started_at < $3))::bigint AS unknown_usage_attempts, \
             COUNT(a.id) FILTER (WHERE a.outcome <> 'pending' AND a.input_tokens IS NOT NULL AND a.output_tokens IS NOT NULL AND a.cost_microcents IS NULL)::bigint AS unpriced_attempts, \
             COALESCE(SUM(a.input_tokens), 0)::bigint AS input_tokens, \
             COALESCE(SUM(a.output_tokens), 0)::bigint AS output_tokens, \
             COALESCE(SUM(a.cache_read_tokens), 0)::bigint AS cache_read_tokens, \
             COALESCE(SUM(a.cache_write_tokens), 0)::bigint AS cache_write_tokens, \
             COALESCE(SUM(a.cost_microcents), 0)::bigint AS estimated_cost_microcents \
             FROM organizations o \
             LEFT JOIN ai_usage_attempts a \
                 ON a.org_id = o.id \
                AND a.started_at >= $1 \
                AND a.started_at < $2 \
             WHERE ($4::uuid IS NULL OR o.id = $4) \
             GROUP BY o.id, o.name, o.created_at \
             ORDER BY o.created_at ASC, o.id ASC",
        )
        .bind(start_inclusive)
        .bind(end_exclusive)
        .bind(stale_before)
        .bind(org_filter)
        .fetch_all(&mut *tx)
        .await?;

        tx.rollback().await?;

        if org_filter.is_some() && rows.is_empty() {
            return Ok(None);
        }

        let mut total = AiUsageSummary {
            attempts: 0,
            successful_reads: 0,
            failed_attempts: 0,
            pending_attempts: 0,
            unknown_usage_attempts: 0,
            unpriced_attempts: 0,
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            estimated_cost_microcents: 0,
            currency: CURRENCY.to_owned(),
        };
        let mut orgs = Vec::with_capacity(rows.len());
        for row in rows {
            let s = AiUsageSummary {
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
            };
            total.attempts += s.attempts;
            total.successful_reads += s.successful_reads;
            total.failed_attempts += s.failed_attempts;
            total.pending_attempts += s.pending_attempts;
            total.unknown_usage_attempts += s.unknown_usage_attempts;
            total.unpriced_attempts += s.unpriced_attempts;
            total.input_tokens += s.input_tokens;
            total.output_tokens += s.output_tokens;
            total.cache_read_tokens += s.cache_read_tokens;
            total.cache_write_tokens += s.cache_write_tokens;
            total.estimated_cost_microcents += s.estimated_cost_microcents;
            orgs.push(OrgAiUsageSummary {
                org_id: row.get("org_id"),
                org_name: row.get("org_name"),
                summary: s,
            });
        }

        Ok(Some((total, orgs)))
    }
}
