use super::*;

#[test]
fn matches_the_official_native_signer_byte_for_byte() {
    // Official 2.1.290 SDK/print capture through an isolated local TLS responder.
    // Fake token/account/device, synthetic ASCII prompt, no real account call.
    // Do not format this fixture: the signature binds the original wire bytes.
    let original = include_bytes!("cch_native.json");
    let body: serde_json::Value = serde_json::from_slice(original).unwrap();
    let billing = body["system"][0]["text"].as_str().unwrap();
    let observed = billing
        .split("cch=")
        .nth(1)
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let marker = format!("cch={observed};");
    let mut unsigned = original.to_vec();
    let offset = unsigned
        .windows(marker.len())
        .position(|s| s == marker.as_bytes())
        .unwrap();
    unsigned[offset + 4..offset + 9].copy_from_slice(b"00000");
    assert_eq!(format!("{:05x}", checksum(&unsigned)), observed);
    sign(&mut unsigned).unwrap();
    assert_eq!(unsigned, original.as_slice());
}

#[test]
fn checksum_matches_independent_xxhash_goldens_for_native_omitted_ranges() {
    // Hash inputs are explicit byte fixtures; expected digests were computed with
    // Python xxhash, independently of this implementation. Keep their key order.
    let cases: serde_json::Value = serde_json::from_str(include_str!("cch_goldens.json")).unwrap();
    for case in cases.as_array().unwrap() {
        let wire = case["wire"].as_str().unwrap();
        assert_eq!(
            checksum(wire.as_bytes()),
            case["expected"].as_u64().unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn signer_patches_only_its_first_top_level_system_block() {
    let mut wire = br#"{"messages":[{"content":"cch=00000;","nested":{"system":[{"text":"cch=00000;"}]}}],"model":"x","system":[{"text":"x-anthropic-billing-header: cch=00000;"},{"text":"Caller policy cch=00000;"}]}"#.to_vec();
    let original: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    let expected = format!("{:05x}", checksum(&wire));
    sign(&mut wire).unwrap();
    let body: serde_json::Value = serde_json::from_slice(&wire).unwrap();
    assert_eq!(body["messages"], original["messages"]);
    assert_eq!(body["system"][1], original["system"][1]);
    assert_eq!(
        body["system"][0]["text"],
        format!("x-anthropic-billing-header: cch={expected};")
    );
}

#[test]
fn checksum_binds_wire_encoding_and_conversation_but_not_model_or_output_cap() {
    let cases: serde_json::Value = serde_json::from_str(include_str!("cch_goldens.json")).unwrap();
    let wire = cases[0]["wire"].as_str().unwrap();
    let expected = checksum(wire.as_bytes());
    assert_eq!(
        checksum(wire.replace("claude-sonnet-5-5", "other-model").as_bytes()),
        expected
    );
    assert_eq!(checksum(wire.replace("1024", "32768").as_bytes()), expected);
    assert_ne!(
        checksum(
            wire.replace("\"messages\":[]", "\"messages\":[{}]")
                .as_bytes()
        ),
        expected
    );
    assert_ne!(checksum(wire.replace("[]", "[ ]").as_bytes()), expected);
}

#[test]
fn missing_own_placeholder_fails_without_modifying_user_content() {
    for original in [
        br#"{"messages":[{"content":"cch=00000;"}],"system":[]}"#.as_slice(),
        br#"{"system":[{"text":"already signed cch=abcde;"},{"text":"cch=00000;"}]}"#.as_slice(),
        b"invalid json".as_slice(),
    ] {
        let mut wire = original.to_vec();
        assert!(sign(&mut wire).is_err());
        assert_eq!(wire, original);
    }
}
