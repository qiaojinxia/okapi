use super::*;
use serde_json::json;

fn group(code: &str, pool: &str, fallback: Option<&str>) -> Group {
    Group {
        code: code.into(),
        name: None,
        ratio: "1.000000".into(),
        pool_code: pool.into(),
        self_select: false,
        fallback_pool_code: fallback.map(str::to_owned),
    }
}

fn route(model: &str, pool: &str, provider: &str) -> Route {
    Route {
        model_name: model.into(),
        pool_code: pool.into(),
        provider: provider.into(),
        upstream_model: "upstream-model".into(),
        responses_native: None,
        capabilities: json!({}),
    }
}

#[test]
fn endpoint_visibility_preserves_pool_fallback_and_provider_boundaries() {
    let routes = [
        route("shared", "codex-pool", "codex"),
        route("shared", "gemini-pool", "gemini"),
        route("openai-only", "codex-pool", "openai"),
    ];
    let groups = [
        group("codex", "codex-pool", None),
        group("gemini", "gemini-pool", None),
        group("fallback", "empty-pool", Some("codex-pool")),
        group("isolated", "empty-pool", None),
        group("combined", "codex-pool", Some("gemini-pool")),
    ];
    let index = signatures(&routes);
    let signature = |model| -> Signature {
        index[model]
            .iter()
            .map(|(p, m)| ((*p).into(), *m))
            .collect()
    };
    // Equal pool membership alone must not merge different providers' endpoint capabilities.
    let shared = visibility(&signature("shared"), &groups).unwrap();
    let openai = visibility(&signature("openai-only"), &groups).unwrap();
    let endpoints: Value = serde_json::from_str(shared.endpoints.get()).unwrap();
    assert_eq!(
        endpoints["codex"],
        json!(["/v1/responses", "/v1/responses/compact"])
    );
    assert_eq!(endpoints["fallback"], endpoints["codex"]);
    assert_eq!(endpoints["isolated"], json!([]));
    assert_eq!(
        endpoints["gemini"],
        json!([
            "/v1/chat/completions",
            "/v1/responses",
            "/v1beta/models/{model}:generateContent"
        ])
    );
    assert_eq!(endpoints["combined"].as_array().unwrap().len(), 4);
    let openai_endpoints: Value = serde_json::from_str(openai.endpoints.get()).unwrap();
    assert_eq!(openai_endpoints["codex"].as_array().unwrap().len(), 5);
    assert_eq!(openai_endpoints["gemini"], json!([]));
    assert_eq!(
        serde_json::from_str::<Value>(shared.groups.get()).unwrap(),
        json!(["codex", "gemini", "fallback", "combined"])
    );
}

#[test]
fn orphan_visibility_and_public_group_serialization_keep_contract() {
    let groups = [group("public", "private-pool", Some("private-fallback"))];
    let orphan = visibility(&vec![], &groups).unwrap();
    assert_eq!(orphan.groups.get(), "[]");
    assert_eq!(
        serde_json::from_str::<Value>(orphan.endpoints.get()).unwrap(),
        json!({"public": []})
    );
    assert_eq!(
        serde_json::to_value(&groups[0]).unwrap(),
        json!({"code": "public", "name": null, "ratio": "1.000000", "self_select": false})
    );
}
