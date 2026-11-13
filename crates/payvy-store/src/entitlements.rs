//! What a hosted company's plan lets it do: `{reader, payments}` with a
//! version that only goes up. The operator sets it (`entitlement.set`); the
//! core reads it before a paid model call or a new payment (docs/hosted.md).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row;
use uuid::Uuid;

use crate::{Store, Tx, append_event, organization};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entitlement {
    pub org_id: Uuid,
    pub reader: bool,
    pub payments: bool,
    /// When the reader was last switched on; None while it is off.
    pub reader_since: Option<DateTime<Utc>>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
    pub updated_by: String,
}

pub struct SetEntitlement<'a> {
    pub org_id: Uuid,
    pub reader: bool,
    pub payments: bool,
    pub version: i64,
    pub now: DateTime<Utc>,
    pub actor: &'a str,
}

#[derive(Debug, Clone)]
pub enum SetEntitlementOutcome {
    /// Stored, with the row it replaced (None for the first one).
    Changed {
        before: Option<Entitlement>,
        after: Entitlement,
    },
    /// The same version with the same values: a retry of a write that
    /// already happened. Nothing changes and no event is appended.
    Replayed(Entitlement),
    /// A version no higher than the stored one, with different values.
    Stale(Entitlement),
    NoOrganization,
}

fn from_row(r: &sqlx::postgres::PgRow) -> Entitlement {
    Entitlement {
        org_id: r.get("org_id"),
        reader: r.get("reader"),
        payments: r.get("payments"),
        reader_since: r.get("reader_since"),
        version: r.get("version"),
        updated_at: r.get("updated_at"),
        updated_by: r.get("updated_by"),
    }
}

/// The company's stored entitlement, read inside its own transaction.
pub async fn entitlement(tx: &mut Tx, org_id: Uuid) -> anyhow::Result<Option<Entitlement>> {
    let row = sqlx::query(
        "SELECT org_id, reader, payments, reader_since, version, updated_at, updated_by \
         FROM org_entitlements WHERE org_id = $1",
    )
    .bind(org_id)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.as_ref().map(from_row))
}

impl Store {
    /// Stores a company's entitlement if `version` is higher than the one
    /// stored, appending `entitlement.set` with the old and new values in the
    /// same transaction. Serialized per company.
    pub async fn set_entitlement(
        &self,
        input: &SetEntitlement<'_>,
        preview: bool,
    ) -> anyhow::Result<SetEntitlementOutcome> {
        let mut tx = self.begin(input.org_id).await?;
        if organization(&mut tx, input.org_id).await?.is_none() {
            tx.rollback().await?;
            return Ok(SetEntitlementOutcome::NoOrganization);
        }
        sqlx::query(
            "SELECT pg_advisory_xact_lock(hashtextextended('payvy.entitlement.' || $1::text, 0))",
        )
        .bind(input.org_id)
        .execute(&mut *tx)
        .await?;
        let before = entitlement(&mut tx, input.org_id).await?;
        if let Some(b) = &before
            && input.version <= b.version
        {
            let same = input.version == b.version
                && input.reader == b.reader
                && input.payments == b.payments;
            tx.rollback().await?;
            return Ok(if same {
                SetEntitlementOutcome::Replayed(b.clone())
            } else {
                SetEntitlementOutcome::Stale(b.clone())
            });
        }
        // Kept while the reader stays on; set when it is switched on.
        let reader_since = match (&before, input.reader) {
            (_, false) => None,
            (Some(b), true) if b.reader => b.reader_since,
            (_, true) => Some(input.now),
        };
        let row = sqlx::query(
            "INSERT INTO org_entitlements \
             (org_id, reader, payments, reader_since, version, updated_at, updated_by) \
             VALUES ($1, $2, $3, $7, $4, $5, $6) \
             ON CONFLICT (org_id) DO UPDATE SET \
             reader = EXCLUDED.reader, payments = EXCLUDED.payments, \
             reader_since = EXCLUDED.reader_since, \
             version = EXCLUDED.version, updated_at = EXCLUDED.updated_at, \
             updated_by = EXCLUDED.updated_by \
             RETURNING org_id, reader, payments, reader_since, version, updated_at, updated_by",
        )
        .bind(input.org_id)
        .bind(input.reader)
        .bind(input.payments)
        .bind(input.version)
        .bind(input.now)
        .bind(input.actor)
        .bind(reader_since)
        .fetch_one(&mut *tx)
        .await?;
        let after = from_row(&row);
        let view = |e: &Entitlement| serde_json::json!({ "reader": e.reader, "payments": e.payments, "version": e.version });
        append_event(
            &mut tx,
            input.org_id,
            "entitlement.set",
            Some(input.org_id),
            input.actor,
            &serde_json::json!({
                "from": before.as_ref().map(view),
                "to": view(&after),
            }),
        )
        .await?;
        if preview {
            tx.rollback().await?;
        } else {
            tx.commit().await?;
        }
        Ok(SetEntitlementOutcome::Changed { before, after })
    }

    /// One company's stored entitlement, for the operator.
    pub async fn get_entitlement(&self, org_id: Uuid) -> anyhow::Result<Option<Entitlement>> {
        let mut tx = self.begin(org_id).await?;
        let e = entitlement(&mut tx, org_id).await?;
        tx.rollback().await?;
        Ok(e)
    }
}
