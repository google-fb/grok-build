# Provider compatibility inventory

Audited baseline: `3163a92fa84c012c9d5a0fa27d36404089eef6f3` (runtime source is
still `0bf4d4bd539a`). This inventory adds source-backed tests, not a new provider
implementation. The older external fork was not consulted.

## What already works

| Capability | Evidence in current source (under `crates/codegen/`) | Boundary |
|---|---|---|
| Provider groups with base URL and key environment-variable name | `xai-grok-shell/src/agent/model_providers.rs:7–24,170–216` | `[model_providers.<id>]` already exists; models select it using `model_provider` |
| A model slug plus provider connection settings | `xai-grok-shell/src/agent/config.rs:4038–4056,4090–4115` | A new model defaults to a 200,000-token context window if omitted (`:3595`); this is an assumption, not remote discovery |
| Provider-group credentials remain separate from session credentials | `config.rs:3611–3631,4867–4928` | Only models selecting a provider group receive the fail-closed credential reference; this does not cover direct model-level connection settings |
| Standard Chat Completions transport | `xai-grok-sampling-types/src/types.rs:1018–1030`; `xai-grok-sampler/src/client.rs:1014–1068` | Default backend is `chat_completions`; POST goes to the configured base plus `chat/completions` |
| Streaming with usage request | `xai-grok-sampler/src/client.rs:288–304,1038–1044` | `stream=true` and `stream_options.include_usage=true` are already sent |
| Function tool calls | `xai-grok-sampling-types/src/types.rs:80–83,398–434`; `xai-grok-sampler/src/stream/chat_completions.rs:75–82,177–223` | Accumulates tool-call deltas by index; actual server/model support must still be tested |
| Prompt/completion/cached/reasoning counters from response usage | `xai-grok-sampling-types/src/types.rs:540–574`; `conversation.rs:871–889` | Standard nested detail fields are already decoded; absent detail counters currently become zero |
| Model-level reasoning/search/stream-tool capability flags | `xai-grok-shell/src/agent/config.rs:4163–4187`; `agent/models/resolution.rs:240–274`; `session/acp_session_impl/sampler_turn.rs:409` | Generic custom models default to no server search, no reasoning effort, and no xAI stream-tool extension |
| Optional cost at the sampler boundary | `xai-grok-sampler/src/stream/chat_completions.rs:129–140,299` | Missing xAI ticks remain `None`; this does not yet ensure every output/report preserves null |

The existing reusable configuration is sufficient to express the desired
connection triple without introducing a second configuration system:

```toml
[model_providers.example]
base_url = "https://provider.example/v1"
env_key = "PROVIDER_API_KEY"
api_backend = "chat_completions"

[model.example-model]
model_provider = "example"
model = "vendor/model"
context_window = 32768 # Set this to the actual served model's limit.
```

The new `a3_provider_triplet_*` test deliberately omits the context limit to
measure the existing fallback. The example above explicitly supplies one.

## What needs changes

| Gap | Evidence | Implementation boundary |
|---|---|---|
| Direct model-level connection settings can fall back to xAI credentials | `xai-grok-shell/src/agent/config.rs:3616–3623,4887–4903`; `xai-grok-shell/src/auth/backend/grok.rs:31–35` | Without `model_provider`, an unset `env_key` on a third-party `[model.<id>]` can resolve to the session token or global xAI key. The existing backend intentionally allows custom gateways. A4 must address this alternative configuration shape explicitly while accounting for that existing gateway behavior |
| OpenRouter `usage.cost` is silently discarded | `xai-grok-sampling-types/src/types.rs:540–554` only models `cost_in_usd_ticks`; `stream/chat_completions.rs:129–140` only captures ticks | Retain provider amount and its source/unit; do not infer money from token counts. Keep absent, explicit zero, and invalid cost distinct |
| OpenRouter cache-write details are discarded | `PromptTokensDetails` (`types.rs:557`) has no `cache_write_tokens`; `conversation.rs:886` sets cache creation to zero | Preserve the optional cache-write counter and define whether/how it forms a disjoint prompt bucket |
| Zero ticks currently mean unreported for xAI | `xai-grok-sampling-types/src/conversation.rs`, `reported_cost_ticks`; sampler cost tests | Preserve that legacy xAI interpretation separately from a provider's explicit monetary zero; do not globally reinterpret every zero as paid/free |
| Messages-compatible result loses unknown-cost state | `xai-grok-pager/src/headless/reducer/messages/usage.rs:65–68,131`; `messages/wire.rs:218,235` | Replace zero fallback with nullable cost and preserve completeness/provenance; plain JSON's absent cost also needs the specified explicit null contract |
| Provider defaults do not inherit all model capabilities | `model_providers.rs:7–24` versus `config.rs:4076–4088` | Reuse the provider table and give optional capabilities explicit defaults/inheritance. Keep xAI-specific options opt-in for other providers |
| xAI tracing headers remain unconditional on the chat transport | `xai-grok-sampler/src/client.rs:50–83,1045–1068` | Apply them only for the provider capability that supports them; ordinary function tools are independent of server-side tools |
| The CLI's xAI preset still has separate control-plane and inference defaults | `xai-grok-shell/src/agent/config.rs:300–332,3820–3880` | Represent xAI as a supported preset without repointing its unrelated service endpoints to an arbitrary inference service |
| Harness summary consumers can collapse unknown to zero | bench source listed below | Make nullable amount/source explicit throughout reports while preserving known partial amounts separately |

The decoder tests directly demonstrate both preserved token fields and discarded
OpenRouter fields. They lock the baseline diagnosis; the implementation must
update the omission assertions when support is added.

## What endpoint overrides alone cannot do

`--xai-api-base-url` and `--cli-chat-proxy-base-url` are assigned independently in
`xai-grok-pager-bin/src/main.rs:140–144`. The former routes xAI API-key inference
and related xAI API services; the latter routes session inference plus the xAI
control plane. `config.rs:300–332` explicitly keeps feedback, trace upload and
managed configuration on the proxy. Replacing both URLs with an OpenRouter or
vLLM URL does not make their unrelated `/settings`, `/deployment/config`, bundle,
or login endpoints exist. Use provider/model configuration for inference instead.

Changing a URL cannot add a decoder field, recover usage that a killed request
never returned, generate a trustworthy monetary cost when the provider reports
none, or guarantee function-calling support in the served model. The inventory
does not claim a live OpenRouter run or a real vLLM/GPU deployment succeeded.

## Current provider documentation

Checked 2026-10-05. OpenRouter's current usage accounting documentation says usage
is automatic, including the final SSE message; the older `usage.include` and
`stream_options.include_usage` opt-ins are deprecated. The account charge is
`usage.cost`; upstream inference cost is a separate field. Cached reads, optional
cache writes and reasoning details have separate counters. Therefore adding an
old opt-in flag is not the missing implementation; preserving the response is.
[OpenRouter usage accounting](https://openrouter.ai/docs/cookbook/administration/usage-accounting).

vLLM supports Chat Completions for models with a suitable chat template. Its
streaming usage must be requested unless the server forces it. Tool calling
depends on model/parser configuration. A response without a provider amount
must remain `null` and be labelled "no provider-reported monetary cost"; hardware
or electricity costs cannot be inferred from tokens.
[vLLM compatible server](https://docs.vllm.ai/en/latest/serving/online_serving/openai_compatible_server/),
[vLLM usage-dependent metrics](https://docs.vllm.ai/en/v0.30.0/features/per_request_metrics/),
[vLLM tool calling](https://docs.vllm.ai/en/latest/features/tool_calling/).

## Harness consumers to update and verify together

These paths are in the bench repository, inspected as source only:

- `harness/full-workflow/cost_accounting.py:15–17,23–47,99–118,175–201`:
  native ticks and null already exist; ledger validation accepts nullable ticks,
  while explicit zero ticks are rejected. `LEDGER_FIELDS` includes reasoning,
  but stream `TOKEN_FIELDS` does not. Provider currency/provenance and new schema
  need to be consumed without changing the legacy pin or declaring missing free.
- `tools/token-usage.py:41`: `or 0` currently collapses missing cost; later
  calculations divide the resulting integer and aggregate it.
- `harness/full-workflow/status.py:39` and
  `harness/parallel-workflow/render-summary.py:35`: read the stream float as
  real money. These require explicit null/partial handling as well.
- The runtime driver uses `streaming-messages-json`
  (`harness/full-workflow/run.py:174`), so only fixing plain JSON is insufficient.
- `harness/full-workflow/finalize.py:130–131`,
  and `harness/parallel-workflow/build-report.py:40–41` forward the accounting
  summary. Carry new amount source/unit fields through these consumers too.
  `harness/full-workflow/assemble-delivery.py:94` instead writes a fixed null
  amount and false completeness; it is an intentionally unpriced surface,
  not evidence that the amount/source reached the assembled delivery.

## Reproducible checks

Inside the isolated Rust builder, with a new or copied writable build cache:

```sh
cargo test --locked -j 2 -p xai-grok-sampling-types -p xai-grok-shell --lib a3_ -- --test-threads=1
```

This builds the actual source and exercises five new tests: grouped connection
triple and conservative capability defaults, group missing-key fail-closed behavior,
the direct-model credential-fallback baseline,
OpenRouter-shaped token/cost parsing, and no-cost vLLM-shaped usage. All data is
synthetic; no credentials, real sessions, or paid model calls are involved.
These are library-level checks; live HTTP streaming/tool execution remains part
of the implementation's mock end-to-end validation. The existing provider tests
and sampler streaming tests supply additional reusable regression coverage.
