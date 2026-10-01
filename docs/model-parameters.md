# Playground model parameters

Checked against official API documentation on 2026-10-01:

- [OpenAI model guide](https://developers.openai.com/api/docs/guides/latest-model), [reasoning guide](https://developers.openai.com/api/docs/guides/reasoning)
- [Kimi K3](https://platform.kimi.ai/docs/guide/kimi-k3-quickstart)
- [Claude effort](https://platform.claude.com/docs/en/build-with-claude/effort)
- [Gemini thinking](https://ai.google.dev/gemini-api/docs/generate-content/thinking)

The curated contract lives in `okapi-providers::model_parameters`. It matches exact
**upstream** IDs after channel mapping, not catalog capability checkboxes. `k3` is
recognized only for the official Kimi Coding Plan endpoint. Custom/unknown IDs
remain passthrough; they do not inherit effort choices from a guessed vendor.

`GET /api/me/playground/parameters?model=...` authenticates the session and optional
`X-Okapi-Playground-Key`, verifies ownership, model allowlist and published pricing,
then reads the same canonical model/pool chain as the gateway. It returns only
the common safe controls across available chat candidates, including fallback
pools and channel strip/inject settings. It does not spend quota or call upstream.
The chat relay checks these rules again before billing; browser-side validation
is not an authorization boundary.

Blank fields are omitted. GPT reasoning sampling is conditional on effective
effort `none`; Kimi K3 fixed sampling fields are not editable. Effort uses the
actual API dialect: Chat `reasoning_effort`, Responses `reasoning.effort`, Claude
`output_config.effort`, Gemini 3 `thinkingLevel`. Claude budget-based models and
Gemini 2.5 instead offer an optional positive thinking-token budget. Budget is
not equivalent to effort; defaults remain with the model. The output cap includes
reasoning and is never enlarged by the modern effort translator.

Model/key changes reset model-specific parameters; old local settings and presets
remain readable but incompatible fields cannot be sent. Kimi K3 follow-up turns
include the assistant's `reasoning_content` for the same requested model. Signed
Claude thinking blocks and Gemini thought signatures are not reconstructed from
text: their native multi-turn protocols are outside this text playground's scope.

The playground loads every model and group page using the authenticated catalog
loader. Marketplace server pagination remains unchanged. If parameter metadata
is unavailable on an old backend, the UI omits sampling and effort rather than
guessing support.
