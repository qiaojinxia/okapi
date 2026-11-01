# Subscription quota forecast

Subscription accounts (Claude Code `anthropic_max`, Codex) are limited by rolling
windows: a five-hour window and a weekly window, each reported as a percentage. The
poll that observes them runs every five minutes, rounds up to whole percentage points
and may be ten minutes old when a request is routed. A single cache-cold request on a
long conversation can move a Pro account's five-hour window by 5–15 points, so the
poll cannot see the wall coming. This document describes the request-level forecast
that closes that gap, how to read what it records, and how to turn it from `shadow`
into `enforce`.

## What production showed (2026-10-31, 14 days, 15 375 subscription requests)

- The four Pro accounts have a five-hour window worth about **$30–39** of API-equivalent
  cost (three exhaustion events measured as rolling five-hour spend); a Max 5x account
  about $161. The weekly window is roughly 6–8× the five-hour one. Pricing is the
  gateway's own `upstream_cost_micro`, so these are API-equivalent dollars, not a
  statement about the vendor's accounting.
- Cache-cold requests (prompt > 50k tokens, cache read < 20 %) were 4.9 % of calls and
  **47 %** of upstream cost. A 300k-token Opus cold start costs ≈ $1.5 (≈ 5 % of a Pro
  five-hour window); a 907k one ≈ $4.5 (≈ 14 %).
- All eight upstream 429s landed on Pro channels that had no `quota_limits`. Three of
  the four clusters were one large cold request arriving on a window that was already
  80–100 % used; the account then returned nothing successful for 4–23 hours.
- Of the cold requests, 464 (≈ $809) came after an idle gap of five minutes or more on
  the same account. The prompt cache was already dead, so choosing another account
  would have cost nothing — but the 1-hour sticky mapping pinned them back.
  True warm migrations (account changed within five minutes) were only 63 (≈ $105).

## Data flow

```
 upstream response ──► unified_limits::report ──► sink (gateway) ─┬─► attempts[].rate_limit   (diagnostics)
  (success or error)                                              └─► quota:hdr:ck:{key}      (latest header reading)
                                                                      quota:theta:ck:{key}:{window}  (θ estimate)
                                                                      quota:hist:ck:{key}    (capped samples)
 5-minute poll ───────────────────────────────────► quota:ck:{key} (+ burn_micro baseline), θ, samples
 settlement (upstream_cost_micro) ────────────────► burn:ck:{key}  (monotonic counter, micro-USD)
 routing: retain_available ──► assess(candidate, shape) ──► diagnostics.forecast  (+ exclusion in enforce)
```

* `anthropic-ratelimit-unified-{5h,7d}-{utilization,reset,status}` plus the unified
  `status`, `representative-claim` and `reset` headers are parsed by
  `okapi_providers::unified_limits`. Only short lowercase tokens and finite numbers in a
  sane range are kept. Responses from other upstreams carry none of them and report
  nothing. **Whether successful responses carry these headers, and the exact unit of
  `utilization` (a fraction in `[0, 1]` is assumed), is verified from the first
  diagnostics in production**; a window reading above 105 % after conversion is ignored.
* The sticky session map gets a companion key `stick:sess:{user}:v1:{hash}:at` holding
  the time of the session's last **successful** request. The mapping value is unchanged,
  so an older binary still reads it. A mapping without a companion (written before the
  upgrade) counts as warm — the behaviour before this change.

## The model

For each account and window, with `u_obs` observed at `t_obs` while the key's burn counter
was `B_obs`, and the counter now reading `B`:

```
û = u_obs + θ · (B − B_obs)            percentage points of the window
σ² = q² + (0.3 · θ · (B − B_obs))² + 0.8² · hours_since_observation (+ 4 when the baseline is unknown)
```

* `θ` is percentage points per API-equivalent dollar. Priors: Pro 100/33 for the
  five-hour window and 100/240 for the weekly one; Max 5x 100/161 and 100/1200; Max 20x
  100/660 and 100/4800. The plan comes from the cached plan observation; unknown plans use
  the Pro prior.
* `θ` is learned online from consecutive readings of the same window instance:
  `θ = (Σ Δu + prior·3) / (Σ ΔC + 3)` where the sums are discounted by spend (half-life
  $100), so quiet periods do not forget anything. Pairs across a window change, with no
  spend, going backwards in time, or with a counter that fell are discarded. Poll
  readings are debiased by 0.5 point (they are rounded up); differences telescope, so
  rounding noise does not accumulate. The estimate is clamped to 0.2–5× the prior.
* An observation older than 20 minutes, or whose window has passed its reset time, is
  unknown: it is neither extrapolated nor used to block anything.
* A request's own increment is `θ ·` its API-equivalent cost, priced with the same
  calculator as settlement from a synthetic usage:
  * **cold** (new session, or the session's last success is older than 270 s): the whole
    prompt (estimated tokens × 1.1) is written to the cache;
  * **warm** (the sticky key was served within 270 s): all but 3 000 tokens are cache
    reads;
  * plus 700 expected output tokens, times the channel's relative cost.
* A candidate is flagged when `û + 1.28·σ + increment > line` for either window.

## Configuration

All keys live in the channel's `settings.account_control`:

| key | default | meaning |
|---|---|---|
| `quota_forecast` | `"shadow"` | `off`: no assessment. `shadow`: assess and record in diagnostics, never exclude. `enforce`: exclude candidates the forecast flags. |
| `quota_limits` | none | Existing per-window cap (`{"18000": 80, "604800": 90}`). With the forecast it is the **placement line**: new sessions and cache-cold requests. |
| `quota_keep_limits` | none | Optional **keep line** for sessions whose cache is still warm (sticky hit within 270 s). Never below the placement line; without a placement line it is ignored. Also applied by the per-attempt `admit` check, so a warm session is not rejected a second time. |

With no `quota_limits` the forecast line is 100 %: it refuses only a request it predicts
will exhaust the window. `shadow` is the default, so existing channels change
behaviour in no way until `enforce` is set. Excluded candidates appear in the request
diagnostics under `excluded` with `"rule": "forecast"`, and the earliest reset feeds
`Retry-After` like any other quota exclusion. If every candidate is excluded the client
gets `no_available_channel` — the same failure mode as an account that is already capped.

Suggested starting point for a Pro account once the shadow data looks right:
`{"quota_forecast": "enforce", "quota_limits": {"18000": 85, "604800": 92}, "quota_keep_limits": {"18000": 97}}`.

**Rollback.** `account_control` rejects unknown fields. A binary that predates this
change treats a channel whose settings contain `quota_forecast` or `quota_keep_limits`
as having an invalid policy and stops routing to it. Remove both keys from every channel
before rolling back. Channels that never set them are unaffected, and every Redis key
introduced here is ignored by older binaries.

## Reading the diagnostics

Admin request logs (`diagnostics` column in ClickHouse) carry:

* `attempts[].rate_limit` — the raw reading from that attempt's response:
  `{"http": 200, "status": "allowed", "claim": "five_hour", "w": [{"name": "five_hour", "window_secs": 18000, "utilization": 0.42, "resets_at": …, "status": "allowed"}, …]}`.
* `forecast` — `{"mode", "tokens", "cold_usd", "warm_usd", "cands": [{"k": key, "warm": bool, "v": "ok"|"block", "w": [{"n", "src": "header"|"poll", "age", "obs", "u", "s", "th", "d", "line", "hit"}]}]}`.
  `obs` is the reading used, `u` the extrapolated estimate, `s` its σ, `th` θ, `d` this
  request's increment in points, `line` the line applied.

Personal (portal) logs expose neither.

Evaluation queries (ClickHouse, subscription channels only):

```sql
-- Do successful responses carry the headers, and in which unit?
select countIf(position(diagnostics, '"rate_limit"') > 0) as with_headers, count() as total,
       max(JSONExtractFloat(diagnostics, 'attempts', 1, 'rate_limit', 'w', 1, 'utilization')) as max_utilization
from okapi.request_log_raw
where log_type = 2 and channel_id in (…) and ts > now() - interval 1 day;

-- Forecast residual: what the response header reported after the request, minus the
-- forecast's estimate before it plus the request's own increment.
select toStartOfHour(ts) as hour, count() as n,
       round(avg(actual - (u + d)), 2) as bias,
       round(quantile(0.9)(abs(actual - (u + d))), 2) as p90_abs_error
from (
  select ts,
         JSONExtractFloat(diagnostics, 'attempts', 1, 'rate_limit', 'w', 1, 'utilization') * 100 as actual,
         JSONExtractFloat(diagnostics, 'forecast', 'cands', 1, 'w', 1, 'u') as u,
         JSONExtractFloat(diagnostics, 'forecast', 'cands', 1, 'w', 1, 'd') as d
  from okapi.request_log_raw
  where log_type = 2 and channel_id in (…) and ts > now() - interval 3 day
    and JSONExtractInt(diagnostics, 'forecast', 'cands', 1, 'k') = channel_key_id
    and JSONExtractString(diagnostics, 'forecast', 'cands', 1, 'w', 1, 'n') = 'five_hour'
    and position(diagnostics, '"rate_limit"') > 0)
group by hour order by hour;

-- Requests the forecast would have refused, and what became of them.
select ts, channel_key_id, upstream_status, error_code, prompt_tokens, cached_tokens
from okapi.request_log_raw
where log_type in (2, 5) and position(diagnostics, '"v":"block"') > 0
order by ts desc limit 100;
```

The same readings are kept in Redis for ad-hoc work: `LRANGE quota:hist:ck:{key} 0 99`
(newest first; `s` is `poll` or `hdr`, `b` the burn counter, `w` is `[window_secs, used, resets_at]`).

## When to enforce

Switch one Pro channel to `enforce` when, over at least three days of shadow data:

1. at least 95 % of successful responses carry `rate_limit`, and `utilization` stays in `[0, 1.05]`;
2. the forecast residual has |bias| below about one point and a 90th percentile below about three
   points for readings younger than a minute;
3. the requests marked `block` that were actually sent account for the 429s and the
   “window exhausted” periods, and few of them were later served without the window reaching
   its line;
4. every Pro account that has no `quota_limits` yet (channels 3, 4 and 7 at the time of writing)
   has one, so the forecast is not the only guard.

Tune `quota_limits` / `quota_keep_limits` first; the constants in `forecast::model` (σ terms,
`z`, prior capacities) are meant to be recalibrated from this data, not guessed.

## Known gaps

* Headers on 200 responses and the `utilization` unit are assumptions until measured (above).
* How cache reads and writes weigh against the vendor's quota is unknown. The model treats the
  gateway's API-equivalent cost as the regressor; if residuals show otherwise, split it per
  model and token class.
* The prompt estimate is tiktoken-based and padded by 10 %; Claude's own count differs.
* Usage outside the gateway (the account owner using the vendor's own client) is invisible and
  appears only as drift, absorbed at the next poll or header reading.
* Concurrent writers can lose a θ update; with a single replica this does not happen, and a lost
  update only delays convergence.
* Placement scoring that balances load across accounts (and re-places a session when its cache has
  already expired), a 429 classifier that reads `status` / `representative-claim`, and an offline
  replay evaluator are not part of this change.
