//! Seeded prices remain drafts until explicitly published, including at gateway startup.
//! Keep media/rule fixtures on that boundary rather than weakening production loading.

pub(super) async fn publish(pg: &sqlx::PgPool, publisher: i64) {
    let source = okapi_store::pricing::load_pricing_source_rows(pg)
        .await
        .unwrap();
    let mut snapshot = serde_json::to_value(source).unwrap();
    snapshot["base_price_per_1m_micro"] =
        serde_json::json!(okapi_pricing::book::BASE_PRICE_PER_1M_MICRO);
    okapi_store::admin::publish_epoch(pg, publisher, &snapshot)
        .await
        .unwrap();
}
