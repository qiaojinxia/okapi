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
  "local_cost": { "cap_micro": 50000000, "period": "month" },
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

## Usage caps (every channel)

Any channel, API key or subscription, can carry two optional caps on what this site has
recorded for it. Each has its own period:

- `local_cost`: `cap_micro` is in micro-USD and is compared with the channel's recorded
  upstream cost, the list price of each settled request times the channel's relative
  procurement cost (`billing_records.upstream_cost_micro`). What customers were charged is
  not part of it, and a request whose cost is unknown (an unpriced server tool, for
  example) adds nothing.
- `local_tokens`: `cap` is compared with recorded input plus output tokens, with cache and
  reasoning counted as the subsets they are, never twice.

`period` is `total` (the default: the lifetime total, which never resets), `day`, `week`
(weeks start on Monday) or `month`, calendar periods in the machine's timezone. A channel
at either cap takes no new requests; other channels carry on and normal fallback applies.
It comes back when the period ends, or as soon as the cap is raised or removed. Requests
already running when the cap is reached are settled afterwards and can run slightly over.
Usage outside this site, such as a native client on the same account, is not counted.

When the cap is periodic, the request diagnostics record `local_cost` or `local_tokens`
together with `resets_at`, the end of the period; if no other channel can serve the
request, the 503 carries that moment as `Retry-After`. When both caps are reached the
reason is `local_cost`. Usage that cannot be read fails closed with
`local_usage_unavailable`: a capped channel is not used while its usage is unknown.

A billing insert trigger keeps the lifetime totals (`channel_token_totals.tokens` and
`cost_micro`) in the same transaction; idempotent settlement replay never inserts another
bill. Retention and refunds do not undo upstream usage already spent. Daily, weekly and
monthly reads combine retained bills and archived receipts under the retention lock.
Billing data is not an upstream subscription entitlement.

The drawer keeps these under **额度上限 / Usage caps** on the scheduling tab, for every
provider, together with the subscription percentages described below. The channel list
shows used against cap for a capped channel and marks a configured percentage on the
window it governs; the row menu opens the section directly.

## Subscription-only controls

Only plugins declaring subscription capabilities expose the upstream percentage caps and
renewal options. The current registrations are Claude Code (`anthropic_max`) and
Codex (`codex`). API-key channels display the usage caps and the general controls without
subscription fields. Defaults remain folded, and configured values appear in the section summary.

The per-channel **Automatic renewal** switch saves `refresh_mode: "managed"`
when enabled and `"external"` when disabled. Disabling blocks background,
request-time and manual refresh; upstream quota queries remain independent.
Access-token-only credentials show a manual-update notice instead of the switch.
The default renewal margin stays in a folded advanced section. Subscription
summaries show the effective renewal state, including token-only credentials.
Usage observations are fetched only while the usage caps section is expanded;
query failures offer a retry without resetting any form draft.

Each upstream allowance has its own optional percentage cap. `quota_limits`
maps window durations in seconds to percentages: `18000` for five hours and
`604800` for a week. The plugin declares which controls to display; admission
matches the actual reported duration, independently of provider or window name.
Either window reaching its cap stops new attempts. Missing/reset/stale observations
remain unknown. The historical single-percentage setting remains compatible:

- Claude: the `five_hour` allowance. Weekly usage is also displayed; a custom
  five-hour threshold does not become a custom weekly threshold. Max plans also carry a
  weekly allowance that applies to one model family (the usage response lists it in
  `limits[]` as `kind: weekly_scoped` with `scope.model.display_name`, `Fable` today): it
  is stored in the snapshot's separate `scoped` list and only displayed, as one more bar
  labelled with the model name in the channel list and the drawer. It never enters
  `windows`: a spent Fable week must not stop the account from serving other models,
  and the configured weekly cap does not apply to it.
- Codex: the longest reported allowance, usually the long-term window. If only one
  window exists, it is used. With absent durations, the plugin prefers the secondary
  allowance. No local calendar or assumed lifetime total substitutes for this data.

Codex plans can have both five-hour and weekly windows. Actual durations and reset
times are displayed from the account response. Any fresh reported window at full
exhaustion, or a fresh upstream `allowed=false`, blocks admission even if the custom
percentage threshold is disabled. Missing/expired data remains unknown, never 0%.

The quota APIs do not provide authoritative USD/token allowances, so no dollar figure is
ever presented as what the supplier allows. A dollar or token cap on a subscription channel
is the local cap described under [Usage caps](#usage-caps-every-channel): this site's own
record of the channel, at API-equivalent prices.

## Shared creation flow

API-key imports and subscription code exchange use the same validated creation
options and transactional writer. Settings, owner, priority, concurrency, relative
procurement cost, the one initial credential and pool
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
changing inference health. Observations expire after 600 seconds; individual windows
stop governing admission after their upstream reset time. Observation identity is
bound to the credential/account, so reauthorization cannot inherit another account's
quota. Cached payloads contain no access or refresh tokens.

The fixed gateway admission and scheduling flow consumes normalized snapshots.
It filters exhausted/threshold-limited accounts before sticky promotion and checks
again before each inference attempt, including retries. Pool and priority ordering
remain intact; known headroom refines ordering within those boundaries. Unknown
headroom is neutral. Account affinity never bypasses limits or moves upstream-bound
history to another account. Safe failover returns `no_available_channel` if none remain.

### Request-level forecast and the keep line

The five-minute poll reports whole, rounded-up percentage points, so it cannot tell that
a cache-cold request on a long conversation will push an account over its cap. Two
optional `account_control` keys address that; both are described, with the model and the
diagnostics, in [quota-forecast.md](quota-forecast.md):

- `quota_forecast`: `"shadow"` (default) records an assessment in the request diagnostics
  and never changes routing; `"enforce"` excludes a candidate whose predicted usage after
  this request would exceed its line; `"off"` disables the assessment. Without a
  `quota_limits` entry the line is 100 %.
- `quota_keep_limits`: per-window keep lines (same units as `quota_limits`). A session whose
  prompt cache is still warm (sticky hit, last successful request under 270 seconds ago)
  may run up to its keep line, while new sessions and cache-cold requests stop at the
  `quota_limits` line. This stops a hot conversation from being evicted, and re-placed
  cold, the moment its account crosses the placement cap. A keep line below the placement
  line is ignored, and a keep line without a placement line has no effect.

Older binaries reject these keys (unknown fields), so remove them before rolling back.
The same holds for `local_cost` and for `"period": "month"` on `local_tokens`: a binary from
before them treats the channel's policy as invalid and stops using the channel.

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
`token_usage`, `cost_usage` and server timezone, requiring channel read permission plus
scoped ownership. Each usage is given for its own period: the configured cap's, or `total`
without one; optional `token_period` and `cost_period` (`total|day|week|month`) preview
another period. It never initiates supplier inference.

## Historical local budgets

Historical combined request/token/cost admission has been retired. The old `usage`
configuration is deserialized for compatibility but never governs admission or appears
in the active policy response. The drawer explains retirement and removes `usage` on
save, preserving other plugin settings. Billing data and historical counter rows are
not deleted; inference no longer writes those local request counters. `local_cost` and
`local_tokens` are separate, explicit settings; old inactive `usage` settings cannot
silently reactivate limits. Unlike the retired cost cap, `local_cost` does not block a
channel because one of its requests has an unknown cost, takes no per-attempt lock and
writes no counter of its own.

## Verification

Provider tests cover Claude session thresholds, the Fable weekly window being displayed
but never gating the account, Codex actual long-term windows,
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
