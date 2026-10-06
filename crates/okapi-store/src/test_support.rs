//! Explicit opt-in for disposable integration-test resources. Never load .env.
fn validate_resources(marker: &str, pg: &str, redis: &str) -> Result<(), &'static str> {
    if marker != "1" {
        return Err("run scripts/test-isolated.py; fixtures require OKAPI_TEST_ISOLATED=1");
    }
    let pg = reqwest::Url::parse(pg).map_err(|_| "invalid test PostgreSQL URL")?;
    if !matches!(pg.scheme(), "postgres" | "postgresql")
        || !pg
            .path()
            .strip_prefix("/okapi_test_")
            .is_some_and(|suffix| !suffix.is_empty() && !suffix.contains('/'))
    {
        return Err("test PostgreSQL database must start with okapi_test_");
    }
    let redis = reqwest::Url::parse(redis).map_err(|_| "invalid test Redis URL")?;
    if !matches!(redis.scheme(), "redis" | "rediss")
        || !redis
            .path()
            .trim_start_matches('/')
            .parse::<u8>()
            .is_ok_and(|db| (1..16).contains(&db))
    {
        return Err("tests require an explicit Redis database between 1 and 15");
    }
    Ok(())
}
pub fn assert_isolated() {
    let marker = std::env::var("OKAPI_TEST_ISOLATED").unwrap_or_default();
    let pg = std::env::var("DATABASE_URL").expect("isolated DATABASE_URL");
    let redis = std::env::var("OKAPI_REDIS_URL").expect("isolated Redis URL");
    validate_resources(&marker, &pg, &redis).expect("unsafe integration-test resources");
}

#[cfg(test)]
mod tests {
    use super::validate_resources;
    #[test]
    fn fixtures_reject_developer_stores_and_missing_opt_in() {
        let pg = "postgres://test@127.0.0.1/okapi_test_disposable";
        let redis = "redis://127.0.0.1:6379/3";
        assert!(validate_resources("1", pg, redis).is_ok());
        assert!(validate_resources("", pg, redis).is_err());
        assert!(validate_resources("1", "postgres://test@127.0.0.1/okapi", redis).is_err());
        assert!(
            validate_resources(
                "1",
                "postgres://test@127.0.0.1/okapi_test_disposable/nested",
                redis
            )
            .is_err()
        );
        for unsafe_redis in [
            "redis://127.0.0.1/0",
            "redis://127.0.0.1",
            "redis://127.0.0.1/16",
            "redis://127.0.0.1/not-a-database",
        ] {
            assert!(validate_resources("1", pg, unsafe_redis).is_err());
        }
        assert!(validate_resources("1", "https://127.0.0.1/okapi_test_disposable", redis).is_err());
    }
}
