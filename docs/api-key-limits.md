# API key limits

The portal key editor supports expiry, a lifetime USD spending limit and an exact
canonical-model allowlist. “Tier” in this editor is now “Group”. The group picker
continues to use `/api/me/groups`, not the global group directory.

- `POST /auth/keys`: optional `expires_at` (RFC3339), `quota_micro` (integer
  micro-USD), `model_allowlist` (model ID array). Omitted/null fields are unrestricted;
  an empty model list is normalized to null. Creation still requires an account session.
- `PATCH /api/me/keys/{id}`: omitted fields stay unchanged; null clears a restriction.
  Positive limits through 9,007,199,254,740,991 micro-USD are accepted. A new expiry
  must be in the future. Ownership is enforced and the auth cache is invalidated.
- A restricted API credential cannot lift restrictions through the portal API without
  its owner's matching account session. Administrators retain their privileged controls.
- `/api/me/keys` returns `quota_mode`, `quota_micro`, `expires_at`, `model_allowlist`,
  and `key_limits_supported: true`. The frontend disables budget editing on older
  servers, which otherwise silently ignore unknown JSON fields.

Expiry remains an authentication check. Models remain subject to group/channel
access; the allowlist only narrows access, including custom pass-through billing models.

For limited keys, ordinary admissions take the same per-user PostgreSQL advisory
lock as durable holds and settlement. After synchronizing pending bills, they check
fresh `used_micro` plus ordinary Redis reservations plus pending/held durable amounts
plus the new estimate against the cap. The lock is held through Redis admission.
Durable admissions use the same check. Replaying existing durable work does not
consume budget twice. Unlimited keys retain their existing ordinary hot path.

Insufficient key budget returns HTTP 429 `key_quota_exceeded`, independently of
wallet/subscription balance. Reservation refunds release their budget; confirmed
charges are reflected in `used_micro`. Limits are total lifetime spending, not an
extra balance and not a daily reset. In-flight work settles at its actual cost:
an underestimate or later limit reduction can put spending above the cap, after
which new requests are blocked. A cap does not truncate responses or change bills.

No new schema migration is needed for these fields. Deploy the new backend before
enabling budget editing. Its required `AuthedKey.quota_limited` field intentionally
invalidates old serialized auth-cache entries instead of treating old limited keys
as unlimited.

Regression coverage: `gateway_key_admission`, `durable_holds::key_budget`, and
`frontend/e2e/key-limits.spec.ts`, using isolated fixture data only.
