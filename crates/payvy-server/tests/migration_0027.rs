//! Migration 0027 backfills po_allocations.seq in creation order on a
//! database that already has allocations — under forced RLS, which the
//! migration lifts around the backfill (the 0018/0020/0022 pattern).

use sqlx::{Connection, Executor, PgConnection, Row};
use uuid::Uuid;

#[tokio::test]
async fn seq_is_backfilled_in_creation_order_on_a_database_with_allocations() {
    let (Ok(admin_url), Ok(app_url)) = (
        std::env::var("PAYVY_TEST_ADMIN_URL"),
        std::env::var("PAYVY_TEST_APP_URL"),
    ) else {
        eprintln!("skipping: PAYVY_TEST_ADMIN_URL / PAYVY_TEST_APP_URL not set");
        return;
    };
    let name = format!("payvy_test_{}", Uuid::new_v4().simple());
    let mut admin = PgConnection::connect(&admin_url).await.unwrap();
    admin
        .execute(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {name} OWNER payvy"
        )))
        .await
        .unwrap();
    let url = format!("{app_url}/{name}");
    let pool = sqlx::PgPool::connect(&url).await.unwrap();
    let migrator = sqlx::migrate!("../payvy-store/migrations");
    // Everything through 0026 (bill batches), then seed allocations.
    migrator.run_to(26, &pool).await.unwrap();
    let org = Uuid::from_u128(1);
    let (vendor, po, rev) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (older, newer) = (Uuid::new_v4(), Uuid::new_v4());
    let mut conn = pool.acquire().await.unwrap();
    conn.execute(
        "SELECT set_config('payvy.org_id', '00000000-0000-0000-0000-000000000001', false)",
    )
    .await
    .unwrap();
    for sql in [
        format!(
            "INSERT INTO vendors (id, org_id, name) VALUES ('{vendor}', '{org}', 'Seed Hauler')"
        ),
        format!(
            "INSERT INTO purchase_orders (id, org_id, vendor_id, po_number, created_by, created_at, updated_at) VALUES ('{po}', '{org}', '{vendor}', 'PO-SEED', 'seed', '2027-01-01T00:00:00Z', '2027-01-01T00:00:00Z')"
        ),
        format!(
            "INSERT INTO po_revisions (id, org_id, po_id, revision, authorized_cents, allowance_cents, created_by, created_at) VALUES ('{rev}', '{org}', '{po}', 1, 100000, 0, 'seed', '2027-01-01T00:00:00Z')"
        ),
        format!(
            "INSERT INTO po_allocations (id, org_id, po_id, revision_id, amount_cents, state, source_actor, created_at) VALUES ('{older}', '{org}', '{po}', '{rev}', 100, 'reserved', 'seed', '2027-01-01T00:00:00Z')"
        ),
        format!(
            "INSERT INTO po_allocations (id, org_id, po_id, revision_id, amount_cents, state, source_actor, created_at) VALUES ('{newer}', '{org}', '{po}', '{rev}', 200, 'reserved', 'seed', '2027-01-02T00:00:00Z')"
        ),
        // An updated row can sit anywhere in heap order; seq must not care.
        format!("UPDATE po_allocations SET state = 'consumed' WHERE id = '{newer}'"),
    ] {
        conn.execute(sqlx::AssertSqlSafe(sql)).await.unwrap();
    }

    migrator.run(&pool).await.unwrap();

    let rows = sqlx::query("SELECT id, seq FROM po_allocations ORDER BY seq")
        .fetch_all(&mut *conn)
        .await
        .unwrap();
    let seqs: Vec<(Uuid, i64)> = rows.iter().map(|r| (r.get("id"), r.get("seq"))).collect();
    assert_eq!(seqs, [(older, 1), (newer, 2)]);
    // New allocations continue past the backfill.
    let next: i64 = sqlx::query(
        "INSERT INTO po_allocations (id, org_id, po_id, revision_id, amount_cents, state, source_actor, created_at) VALUES ($1, $2, $3, $4, 1, 'reserved', 'seed', '2027-01-03T00:00:00Z') RETURNING seq",
    )
    .bind(Uuid::new_v4())
    .bind(org)
    .bind(po)
    .bind(rev)
    .fetch_one(&mut *conn)
    .await
    .unwrap()
    .get("seq");
    assert_eq!(next, 3);

    drop(conn);
    pool.close().await;
    let sql = format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)");
    let _ = admin.execute(sqlx::AssertSqlSafe(sql)).await;
}
