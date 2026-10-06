# Inference providers

Provider configuration controls inference, separately from the CLI's xAI login,
settings and feedback services. Use an explicit model context limit: the legacy
fallback of 200,000 tokens is not remote discovery.

```toml
[model_providers.router]
base_url = "https://openrouter.ai/api/v1"
env_key = "OPENROUTER_API_KEY"
api_backend = "chat_completions"
provider_profile = "openrouter"

[model.router-model]
model_provider = "router"
model = "vendor/model"
context_window = 32768 # Replace with the served model's actual limit.

[model_providers.local]
base_url = "http://127.0.0.1:8000/v1"
env_key = "LOCAL_INFERENCE_KEY" # Omit for a local service without authentication.
api_backend = "chat_completions"
provider_profile = "vllm"

[model.local-model]
model_provider = "local"
model = "served-model-name"
context_window = 32768
```

Select the configured model ID with `--model router-model` or `--model local-model`.
Keys stay in the named environment variables; configuration stores their names,
not their contents. A missing provider key does not fall back to an xAI key or
login token. This also applies to direct `[model.<id>]` endpoint overrides,
including a separate `api_base_url`, and to loopback endpoints. A misspelled
provider ID is an error. A model referencing a declared provider with an invalid
field (for example a misspelled profile or a non-boolean capability) is rejected
at configuration loading, before requests; the error identifies the provider
and field without printing credential values. Referenced provider tables also
reject unknown keys, including misspelled connection keys. Unreferenced bad
provider entries remain warnings. A malformed or empty model-level
`model_provider` is an error, never an implicit xAI selection.

A custom connection needs an explicit, nonblank `base_url` on its model or
provider. Selecting a non-xAI profile or supplying a per-model/provider credential
without that endpoint is rejected. A per-model/provider credential also
requires an explicit endpoint when `provider_profile = "xai"`; selecting a
profile alone cannot authorize sending a supplied key to an implicit URL.
Built-in xAI models without per-model credential overrides retain their defaults.
Direct model overrides may also inherit explicitly configured custom
`[endpoints].models_base_url`, or a custom proxy/API URL pair. This does not
replace the required endpoint of a referenced provider table. Environment-only
xAI URL overrides do not establish custom provider identity.
Connection fields with invalid types are rejected before the tolerant model
catalog parser can discard them. Unknown model keys without an explicit
endpoint are rejected as well, since a misspelling can otherwise erase all
custom routing intent.
Errors identify `unknown field` separately from `missing endpoint`, without
printing credential values. This applies to built-in model overrides too.

## Reloading provider configuration

The shared leader watches the global `[model]`, `[models]`, `[model_providers]`,
and `[auth_provider]` tables. A reload refreshes the model entries and both
provider maps together, including generated auth helpers from inline provider
`auth` blocks. Embedded ACP stdio clients can explicitly request
`_x.ai/internal/reload_models` with `{}`; they do not run the leader's general
configuration watcher.

New sessions use the refreshed catalog. Reload does not promise to rewrite an
existing session's captured sampler settings or reroute an in-flight call.
Invalid referenced provider settings reject the reload before replacing the
previous valid routing. Model resolution also rejects a missing provider in
an in-memory configuration: neither own credentials nor
`allow_xai_credential_fallback` can turn that missing reference into an implicit
xAI destination. Catalog construction fails, while catalog updates retain the
previous valid state on this error.

This differs from the existing login-token reload behavior of `auth.json`.

## Provider capabilities

The built-in xAI models use the `xai` profile. A custom model defaults to
`compatible`; other profiles are `openrouter` and `vllm`. Only `xai` enables
xAI-specific request headers. The optional provider fields
`supports_reasoning_effort`, `supports_backend_search`, and `stream_tool_calls`
default to false for custom providers; a model-level value overrides its provider
default. The custom default also overrides the global `[models].stream_tool_calls`
setting. `stream_tool_calls` is an xAI protocol extension, not the standard
Chat Completions function-call stream. Ordinary tool-call deltas work independently.

An intentional xAI gateway can explicitly set
`allow_xai_credential_fallback = true` in its model or provider table. This grants
that endpoint access to the xAI credential, including a login session token.
Enable it only for a trusted gateway. Protocol extensions require the independent
`provider_profile = "xai"` choice; changing the profile alone does not grant
credential access. Legacy sessions without a stored profile use xAI extensions
only when their endpoint is recognized as xAI.

## Service boundaries

Selecting a local or third-party main model does **not** make the entire CLI
local-only. Auxiliary summary/compaction or other side-model requests may still
use built-in xAI models and xAI credentials. Login, settings and feedback are
also separate services. Main-loop usage does not include those auxiliary calls;
selecting vLLM alone is not a no-egress guarantee. Auxiliary model routing and
accounting remain a separate scope.

The TUI `/usage` block supports provider amounts. The optional external status
line's `cost.total_cost_usd` uses all reported sources in the legacy main-agent
scope and hides partial/incomplete amounts. It does not add auxiliary costs.
See [request accounting](usage-accounting.md) for the independent session-wide
request ledger and its completeness signals.

## Usage and cost

Both headless JSON formats preserve input, output, cached read/write and reasoning
counters. Durable `usage.json` stores full prompt input. Headless result input is
the disjoint fresh bucket: full input minus cache reads and writes. Reasoning is
a subset of output, not an additional billable token sum. Missing detail counters
retain the existing zero default; this is not proof that the service measured
those details. These legacy aggregates cover the main loop and attributed
subagents. The separate `request_usage` ledger preserves nullable per-request
counters and separates auxiliary attempts; do not add the overlapping scopes.

Provider-reported money is kept separately from tokens:

| Profile | Amount | Source | Absent value |
|---|---|---|---|
| xAI | Positive `cost_in_usd_ticks`, divided by 10^10 | `xai_usage_ticks` | Null; legacy zero ticks mean unreported |
| OpenRouter | Finite, nonnegative numeric `usage.cost`, including explicit zero | `openrouter_usage_cost` | Null; invalid values fail the stream |
| Compatible / vLLM | No monetary field is assumed | None | Null, not free |

`usage.json` adds `cost_by_source` to totals and model rows. Existing
`cost_usd_ticks` remains xAI-only. A legacy ledger with only ticks still loads.
Mixed providers aggregate amounts once per source; ticks are not added again.

Terminal results use nullable `total_cost_usd`, nullable `total_cost_usd_ticks`,
`cost_sources`, nullable `cost_unit` (`USD` when reported), `usage_is_incomplete`
and `cost_is_partial`. Per-model `costUSD` is nullable too. Exact terminal ticks
are exposed only when the entire amount comes from xAI. Unknown, partial or
incomplete terminal amounts are null; a durable checkpoint may retain a known
partial amount without certifying that a terminated phase completed. No token
pricing is used to fill missing money.

OpenRouter credits are USD-denominated. `usage.cost` describes the account charge,
not top-up fees or separately billed BYOK upstream charges. The upstream-cost
field is not substituted for this amount. These CLI totals are not a complete
provider invoice. [OpenRouter usage accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting),
[OpenRouter credit currency](https://openrouter.ai/support/).

The transport requests streaming usage. OpenRouter documents automatic usage
in its final SSE message. vLLM requires an appropriate chat template and tool
parser/model configuration; an OpenAI-compatible endpoint alone does not prove
tool support. [vLLM server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/),
[vLLM tool calling](https://docs.vllm.ai/en/latest/features/tool_calling/).

Local mock tests verify the client protocol, not a live provider account or a
GPU-hosted vLLM installation. Live verification is recorded separately in the
implementation audit.
