# Channel and subscription controls

One channel represents one upstream API key or one subscription account. Request
transformation remains independent of account observation and channel health.

## General controls

Every channel supports concurrency, rate-limit pauses, and pauses after consecutive
failures. The initial concurrency limit is `max_concurrency` on channel creation;
subsequent changes use the existing credential management endpoint. Empty concurrency
means unlimited. Weights and credentials keep their existing management semantics.

The optional `settings.account_control` object contains:

```json
{
  "quota_limits": { "18000": 85, "604800": 90 },
  "local_tokens": { "cap": 10000000, "period": "total" },
  "rate_limit_cooldown_secs": 60,
  "failure_threshold": 3,
  "failure_cooldown_secs": 60,
  "refresh_mode": "external",
  "refresh_margin_secs": 120
}
```

429 uses upstream Retry-After if supplied, otherwise the configured pause applies.
Transient errors pause after `failure_threshold` consecutive failures, including
retries, with exponential backoff starting at `failure_cooldown_secs` and capped at
two hours. This threshold is a channel health policy, not the number of retries
allowed for one HTTP request. Credential-invalid states and safe failover remain
authoritative. One account's pause never disables other channels.

## Subscription-only controls

Only plugins declaring subscription capabilities expose subscription quota and
renewal options. The current registrations are Claude Code (`anthropic_max`) and
Codex (`codex`). API-key channels display the general controls without subscription
fields. Defaults remain folded, and configured values appear in the section summary.

The per-channel **Automatic renewal** switch saves `refresh_mode: "managed"`
when enabled and `"external"` when disabled. Disabling blocks background,
request-time and manual refresh; upstream quota queries remain independent.
Access-token-only credentials show a manual-update notice instead of the switch.
The default renewal margin stays in a folded advanced section. Subscription
summaries show the effective renewal state, including token-only credentials.
Usage observations are fetched only while the subscription section is expanded;
query failures offer a retry without resetting any form draft.

Each upstream allowance has its own optional percentage cap. `quota_limits`
maps window durations in seconds to percentages: `18000` for five hours and
`604800` for a week. The plugin declares which controls to display; admission
matches the actual reported duration, independently of provider or window name.
Either window reaching its cap stops new attempts. Missing/reset/stale observations
remain unknown. The historical single-percentage setting remains compatible:

- Claude: the `five_hour` allowance. Weekly usage is also displayed; a custom
  five-hour threshold does not become a custom weekly threshold.
- Codex: the longest reported allowance, usually the long-term window. If only one
  window exists, it is used. With absent durations, the plugin prefers the secondary
  allowance. No local calendar or assumed lifetime total substitutes for this data.

Codex plans can have both five-hour and weekly windows. Actual durations and reset
times are displayed from the account response. Any fresh reported window at full
exhaustion, or a fresh upstream `allowed=false`, blocks admission even if the custom
percentage threshold is disabled. Missing/expired data remains unknown, never 0%.

The quota APIs do not provide authoritative USD/token allowances. Dollar limits
remain hidden. The optional **local token limit** explicitly uses this channel's
recorded input plus output tokens, including cache/reasoning as subsets without
counting them twice. It does not include native-client usage outside this channel.
The default `total` period includes recorded history and never automatically resets;
`day` and `week` use machine-timezone calendar boundaries (weeks start Monday).
New requests stop at the cap; requests already running can exceed it.

A billing insert trigger updates the lifetime totals in the same transaction; idempotent
settlement replay never inserts another bill. Retention and refunds do not undo
upstream tokens already used. Daily/weekly reads combine retained bills and receipts
under the retention lock. Billing data is not an upstream subscription entitlement.

## Shared creation flow

API-key imports and subscription code exchange use the same validated creation
options and transactional writer. Settings, owner, priority, concurrency, relative
procurement cost, retention declaration, the one initial credential and pool
memberships commit together. Invalid settings/pools are rejected before supplier
code exchange; a later database failure rolls back the whole creation. Empty
endpoints resolve from the provider registration. New subscription authorization
also uses the configured proxy and optional token endpoint.

The relative procurement cost option is a channel routing/accounting factor,
not a dollar allowance from the subscription supplier.

## Observation and admission

The bounded maintenance worker queries registered account hooks even when the
percentage field is empty. Query leases are shared across instances (one per key
per minute), response sizes/timeouts are bounded, and failures back off without
changing inference health. Observations expire after 120 seconds; individual windows
stop governing admission after their upstream reset time. Observation identity is
bound to the credential/account, so reauthorization cannot inherit another account's
quota. Cached payloads contain no access or refresh tokens.

The fixed gateway admission and scheduling flow consumes normalized snapshots.
It filters exhausted/threshold-limited accounts before sticky promotion and checks
again before each inference attempt, including retries. Pool and priority ordering
remain intact; known headroom refines ordering within those boundaries. Unknown
headroom is neutral. Account affinity never bypasses limits or moves upstream-bound
history to another account. Safe failover returns `no_available_channel` if none remain.

## Renewal ownership

`managed` uses the existing singleflight, distributed lease, PG reread and credential
compare-and-swap. Background, request, manual and 401-triggered refresh share that
coordination. `external` never consumes a refresh token: use it when a native CLI
owns the same login and import updated credentials when needed. An access token alone
cannot auto-renew. These hooks never read or modify native CLI login files.

## Hook boundary

`AccountHooks` owns quota endpoints, response parsing, subscription configuration
metadata, authorization URL/state rules, code exchange, window selection and
renewal wire behavior. Public authorization metadata declares code presentation,
raw access-token import support, account-id requirements, and optional import
profile defaults. Admin handlers retain ownership checks, one-time Redis state,
SSRF validation, encrypted persistence and compare-and-swap; they never select
Claude/Codex endpoints or parse supplier callback formats. Claude and Codex implementations
live under their provider directories. A hook has no PG, Redis, scheduler or ledger
handles. `scope_quota` also reapplies provider semantics to old cached observations.
The core admission flow and UI have no Claude/Codex window-selection branches.

`GET /admin/channels/providers` publishes sanitized capabilities, including optional
`authorization` and `subscription` metadata (`quota_scope`, `window_secs`). The UI
renders from that metadata and loads provider options from the registry catalog.
Frontend account controls split policy/draft validation, catalog and
observation queries, common settings, token limits, quota observations and renewal
settings into separate modules; adding an account hook needs no provider-name
branches in these components.
`GET /admin/channels/{id}/usage` returns policy, sanitized quota observations,
`token_usage` and server timezone, requiring channel read permission plus scoped
ownership. Optional `token_period=total|day|week` previews the selected local period.
It never initiates supplier inference.

## Historical local budgets

Historical combined request/token/cost admission has been retired. The old `usage`
configuration is deserialized for compatibility but never governs admission or appears
in the active policy response. The drawer explains retirement and removes `usage` on
save, preserving other plugin settings. Billing data and historical counter rows are
not deleted; inference no longer writes those local request counters. The new
`local_tokens` setting opts into token-only limits explicitly; old inactive `usage`
settings cannot silently reactivate limits.

## Verification

Provider tests cover Claude session thresholds, Codex actual long-term windows,
all-window hard exhaustion, absent/expired observations and old-cache normalization.
Isolated gateway tests verify general pauses, quota filtering, identity, observation
without a configured threshold, API authorization, normal fallback, and retired local
limits remaining inactive. Browser tests use API stubs and exercise category visibility,
plugin metadata, validation, folding and OAuth/token-import preservation. No verification
uses a real subscription credential or alters native client conversations.

The lifecycle/UI refactor was verified with a workspace build, frontend build,
lint and localization checks, 122 provider unit tests, 170 application unit tests,
13 channel-control integration tests, 35 OAuth/lifecycle integration tests and
32 focused browser tests. The optional installed-native-client test remained
ignored. Creation tests verify configuration parity for API and subscription
channels and transaction rollback after a database write failure. Mobile inspection
verified folded controls and the disabled-renewal state.

These results do not certify the whole repository suite: broader regression runs
still failed on session-revocation and TOTP browser expectations and an analytics
test expecting a 90-day cap where the current endpoint allows 366 days. They are
outside this refactor. Real-account token renewal was not exercised.

Quota window references: [OpenAI plan usage](https://help.openai.com/en/articles/11369540-using-codex-with-your-chatgpt-plan)
and [Claude limit resets](https://support.claude.com/en/articles/17007452-what-is-a-limit-reset).
