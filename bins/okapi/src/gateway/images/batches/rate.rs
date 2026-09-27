//! Admission counters share the normal generation windows and count expanded
//! upstream requests. This gate never estimates tokens or changes billing usage.
use super::{AppState, store};
use fred::{clients::Client, interfaces::LuaInterface};
use okapi_store::AuthedKey;
use std::time::Duration;

const AXES: [&str; 5] = ["rpm", "rpd", "model_rpm", "group_rpm", "group_rph"];
const LUA: &str = r"
    local units = tonumber(ARGV[1])
    local maximum = 9007199254740991
    local ttl = {120, 172800, 120, 120, 7200}
    local active = {}
    -- Preflight every active counter before writing any. Invalid data fails
    -- closed; Redis scripts do not roll back earlier writes on a later error.
    for i = 1, 5 do
        local cap = tonumber(ARGV[i + 1])
        if not cap or cap > maximum then return -1 end
        active[i] = i <= 2 or cap > 0
        if active[i] then
            local raw = redis.call('GET', KEYS[i]) or '0'
            if raw ~= '0' and not string.match(raw, '^[1-9][0-9]*$') then return -1 end
            local current = tonumber(raw)
            if not current or current < 0 or current % 1 ~= 0 or current > maximum - units then return -1 end
            if cap > 0 and current + units > cap then return i end
        end
    end
    for i = 1, 5 do
        if active[i] then
            redis.call('INCRBY', KEYS[i], ARGV[1])
            redis.call('EXPIRE', KEYS[i], ttl[i])
        end
    end
    return 0
";

pub(super) struct Admission {
    client: Client,
    user_id: i64,
    key_id: i64,
    model: String,
    group: String,
    units: u32,
    caps: [i64; 5],
}

impl Admission {
    // Resolve settings before the PG transaction; checking only needs Redis,
    // avoiding nested pool acquisition while holding the storage admission lock.
    pub async fn load(state: &AppState, key: &AuthedKey, model: &str, units: u32) -> Self {
        let setting = state.setting_cached("model_rpm_limits").await;
        let model_rpm = setting
            .as_ref()
            .as_ref()
            .and_then(|value| value.get(model))
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        Self {
            client: state.sched.client().clone(),
            user_id: key.user_id,
            key_id: key.key_id,
            model: model.to_owned(),
            group: key.group_code.clone(),
            units,
            caps: [
                i64::from(key.rpm_limit.unwrap_or(0)),
                i64::from(key.rpd_limit.unwrap_or(0)),
                model_rpm,
                i64::from(key.group_rpm_limit.unwrap_or(0)),
                i64::from(key.group_rph_limit.unwrap_or(0)),
            ],
        }
    }

    pub async fn check(&self) -> Result<(), store::Error> {
        let now = chrono::Utc::now();
        let minute = now.timestamp().div_euclid(60);
        let prefix = format!("rl:{{{}}}", self.user_id);
        let keys = vec![
            format!("{prefix}:k:{}:rpm:{minute}", self.key_id),
            format!("{prefix}:k:{}:rpd:{}", self.key_id, now.format("%Y%m%d")),
            format!("{prefix}:m:{}:rpm:{minute}", self.model),
            format!("{prefix}:g:{}:rpm:{minute}", self.group),
            format!(
                "{prefix}:g:{}:rph:{}",
                self.group,
                now.timestamp().div_euclid(3600)
            ),
        ];
        let mut args = vec![self.units.to_string()];
        args.extend(self.caps.iter().map(ToString::to_string));
        let outcome: i64 =
            tokio::time::timeout(Duration::from_secs(3), self.client.eval(LUA, keys, args))
                .await
                .map_err(|_| store::Error::AdmissionUnavailable)?
                .map_err(|error| {
                    tracing::warn!(%error, "batch rate admission unavailable");
                    store::Error::AdmissionUnavailable
                })?;
        match outcome {
            0 => Ok(()),
            1..=5 => Err(store::Error::RateLimited(
                AXES[usize::try_from(outcome - 1).unwrap_or(0)],
            )),
            _ => Err(store::Error::AdmissionUnavailable),
        }
    }
}
