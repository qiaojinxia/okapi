//! sqlx creates and removes an isolated test database; no live account or price writes.
use okapi_store::{
    admin::RatioAxes,
    listing::{self, Slice},
    model_config::{self, ModelDraft, ModelMetadata},
};
use serde_json::{Value, json};
use sqlx::PgPool;

fn draft<'a>(name: &'a str, metadata: Option<&'a ModelMetadata>) -> ModelDraft<'a> {
    ModelDraft {
        name,
        axes: RatioAxes::basic("1", "4", "0.123456"),
        mode: Some("ratio"),
        tier_expr: None,
        per_call_price_micro: None,
        tier_ratios: None,
        fallbacks: None,
        metadata,
    }
}

#[sqlx::test(migrations = "./migrations")]
async fn metadata_prices_and_clear_operations_roundtrip(pool: PgPool) {
    let meta: ModelMetadata = serde_json::from_value(json!({"display_name":"Test vision", "vendor":"Example", "kind":"chat",
        "input_modalities":["text","image"], "output_modalities":["text"], "capabilities":{"vision":true,"tools":false},
        "context_window":128_000, "max_output":8192, "description":"Manual declaration"})).unwrap();
    assert!(meta.validate().is_ok());
    let tiers = json!({"flex":"0.5","priority":"2"});
    let modal = json!({"cache_write_5m":"1.25","cache_write_1h":"2","audio_cache_read":"0.3"});
    let mut d = draft("test-vision", Some(&meta));
    d.tier_ratios = Some(&tiers);
    d.axes.modality_ratios = Some(&modal);
    model_config::save(&pool, d).await.unwrap();
    let page = listing::list_models(&pool, None, false, Slice::ALL)
        .await
        .unwrap();
    let row = &page.page.data[0];
    assert_eq!(row.context_window, Some(128_000));
    assert_eq!(row.max_output, Some(8192));
    assert_eq!(row.capabilities["tools"], false);
    assert!(row.capabilities.get("reasoning").is_none());
    assert_eq!(
        row.catalog_config["input_modalities"],
        json!(["text", "image"])
    );
    assert_eq!(row.cache_ratio.as_deref(), Some("0.123456"));
    assert_eq!(row.modality_ratios, Some(modal.clone()));
    assert_eq!(row.tier_ratios, Some(tiers));

    // Missing fields preserve metadata, mode, tiers and independent prices.
    let mut call = draft("test-vision", None);
    call.mode = Some("per_call");
    call.per_call_price_micro = Some(12345);
    model_config::save(&pool, call).await.unwrap();
    let mut preserve = draft("test-vision", None);
    preserve.mode = None;
    model_config::save(&pool, preserve).await.unwrap();
    let row = listing::list_models(&pool, None, false, Slice::ALL)
        .await
        .unwrap()
        .page
        .data
        .remove(0);
    assert_eq!(row.pricing_mode.as_deref(), Some("per_call"));
    assert_eq!(row.per_call_price_micro, Some(12345));
    assert_eq!(row.modality_ratios, Some(modal));
    assert_eq!(row.catalog_config["kind"], "chat");

    let empty = json!({});
    let clear_meta = ModelMetadata::default();
    let mut clear = draft("test-vision", Some(&clear_meta));
    clear.tier_ratios = Some(&empty);
    clear.axes.modality_ratios = Some(&empty);
    model_config::save(&pool, clear).await.unwrap();
    let row = listing::list_models(&pool, None, false, Slice::ALL)
        .await
        .unwrap()
        .page
        .data
        .remove(0);
    assert!(row.tier_ratios.is_none());
    assert_eq!(row.modality_ratios, Some(empty));
    assert!(row.max_output.is_none());
    assert_eq!(row.capabilities, json!({}));
}

#[sqlx::test(migrations = "./migrations")]
async fn invalid_fallback_rolls_back_the_whole_draft(pool: PgPool) {
    model_config::save(&pool, draft("existing", None))
        .await
        .unwrap();
    let unknown = vec!["not-a-model".to_owned()];
    let meta: ModelMetadata =
        serde_json::from_value(json!({"display_name":"Must not persist"})).unwrap();
    let mut d = draft("existing", Some(&meta));
    d.fallbacks = Some(&unknown);
    d.axes.model = "3";
    assert!(model_config::save(&pool, d).await.is_err());
    let row = listing::list_models(&pool, None, false, Slice::ALL)
        .await
        .unwrap()
        .page
        .data
        .remove(0);
    assert!(row.display_name.is_none());
    assert_eq!(row.model_ratio.as_deref(), Some("1.000000"));
    let mut d = draft("new-invalid", None);
    d.fallbacks = Some(&unknown);
    assert!(model_config::save(&pool, d).await.is_err());
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM models WHERE model_name='new-invalid'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let _: Value = json!({});
}
