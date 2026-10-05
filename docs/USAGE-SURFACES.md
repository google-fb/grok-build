# Usage surface audit (baseline `0bf4d4bd539a`)

The baseline does **not** universally expose dollars without tokens. Its native
ledger and plain JSON projection already include input, output, cached, and
reasoning totals. Format selection, aggregation, interrupted output, and
consumer field selection determine which details are visible.

This is a diagnosis, not an accounting change. The offline probe uses invented
counters through the production `UsageLedger`, `PromptUsage`, JSON projection,
and `JsonlStorageAdapter`. It never creates a model client or loads an existing
session. It does not emulate the whole CLI or a provider; full mock-server CLI
coverage belongs to the follow-up implementation.

## Field matrix

| Surface | Input | Output | Cached / cache creation | Reasoning | Cost | Per-model / per-call |
|---|---|---|---|---|---|---|
| `--output-format json` terminal | `usage.input_tokens`, uncached only | `usage.output_tokens` | `usage.cache_read_input_tokens` / `cache_creation_input_tokens` | `usage.reasoning_tokens` | `total_cost_usd` and exact `total_cost_usd_ticks`, only when trustworthy | `modelUsage` aggregates; reasoning and duration dropped there; no individual call records |
| `updates.jsonl`, `turn_completed.usage` | `inputTokens`, full prompt | `outputTokens` | `cachedReadTokens` / `cacheCreationTokens` | `reasoningTokens` | `costUsdTicks`, omitted if partial/incomplete | `modelUsage` includes reasoning and duration, still aggregated; no call history |
| `usage.json` | `totals.input_tokens`, full prompt | `totals.output_tokens` | `totals.cached_read_tokens` / `cache_creation_tokens` | `totals.reasoning_tokens` | `totals.cost_usd_ticks` (nullable), with `cost_missing_calls` and `incomplete` | `by_model` aggregates include all these counters; `model_calls` is a count, not a list |
| Companion: `streaming-messages-json` terminal `result` | `usage.input_tokens`, uncached only | `usage.output_tokens` | same two cache buckets as plain JSON | **not projected** | `total_cost_usd`, falls back to `0.0` if absent; exact ticks and completeness flags are not projected | reduced `modelUsage`; reasoning/duration absent |

Disk updates use an envelope: `method == "_x.ai/session/update"`, with the event
under `params.update`, tagged `sessionUpdate == "turn_completed"`. Treating it as
a top-level `turn_completed` field misses the event. Cost scale: 10^10 ticks = $1.
Zero or absent ticks are not evidence of free work.

The terminal and completed-turn projection cover a **prompt**, while the durable
ledger covers the **session**, including previous prompts and folded subagents.
They coincide in the one-prompt, two-call fixture, not necessarily after resume.
Do not sum cumulative snapshots, or add `reasoning_tokens` a second time to
`output_tokens`. The normalization copies provider completion and reasoning
fields separately; it does not subtract reasoning from completion. Provider
invoice completeness is not established by this audit.

## Normal versus external timeout

| Surface | Normal fixture (two completed model calls) | `timeout` exit 124 while waiting after a checkpoint |
|---|---|---|
| Terminal JSON | input 105, output 30, cache read 50, cache creation 5, reasoning 10, total 190, cost $0.30 | Not emitted if completion has not happened; a terminal already emitted before the timeout remains usable subject to exit/completeness checks |
| Completed-turn event | input 160, output 30, cache read 50, cache creation 5, reasoning 10, ticks 3,000,000,000 | No event for the unfinished turn; previous completed turns can remain |
| Durable ledger | input 160, output 30, cache read 50, cache creation 5, reasoning 10, ticks 3,000,000,000 | Last successfully persisted snapshot survives; after call 1 only: input 100, output 20, reasoning 7, ticks 1,000,000,000; after call 2: normal fixture totals |

A timeout before the first checkpoint can leave no ledger. A response still in
flight has not supplied final usage; an actor mutation queued but not persisted
can also be absent. `incomplete: false` in an earlier checkpoint does **not** prove
that a subsequently killed phase completed. Consumers must retain the process
exit status and mark recovered values partial. The probe demonstrates a real
SIGTERM/exit-124 boundary around the production storage adapter, not the full
CLI's signal handler or actor scheduling.

With a missing second cost, all tokens survive in all three native projections;
the durable ledger retains the known $0.10 and `cost_missing_calls: 1`, while
plain JSON and completed-turn wire output omit money and mark partiality. With
an incomplete ledger, durable known ticks likewise remain while wire cost is
scrubbed. This is intentional fail-closed behavior, not token loss.

## Source evidence and root causes

- `xai-grok-pager/src/headless.rs:282–321`: plain JSON is assembled at turn end
  and attaches the shared usage projection. Interrupt before this point can
  remove the terminal settlement without deleting earlier checkpoints.
- `xai-grok-shell/src/extensions/notification.rs:187–221,302–389`: native fields,
  full-input-to-disjoint-cache projection, exact ticks, trust gates, and the
  explicit per-model reasoning/duration omission.
- `xai-grok-shell/src/session/storage/mod.rs:731–762`: disk update envelope;
  `storage/jsonl/mod.rs:1455–1463`: ledger serialization through atomic writes.
- `xai-chat-state/src/actor/mutations.rs:463–512`: completed calls update prompt
  and session ledgers, then enqueue persistence. `xai-grok-shell/src/session/
  persistence.rs:2230`: the persistence actor writes the snapshot.
- `xai-chat-state/src/usage.rs:37–71,113–123`: counters are folded per model;
  individual call identity/order is not preserved.
- `xai-grok-sampler/src/stream/responses.rs:645–663` and
  `xai-grok-sampling-types/src/conversation.rs:871–889`: completion/reasoning
  are copied independently from response usage. No evidence here establishes
  the hypothesis that charged reasoning is excluded from provider output totals.
- `xai-grok-pager/src/headless/reducer/messages/usage.rs:27–79` and
  `messages/wire.rs:63–77,207–248`: the Messages compatibility shape drops
  reasoning and trust flags, and substitutes zero cost when the shared
  projection omitted it. It is not the same schema as `--output-format json`.
- `xai-grok-shell/src/session/acp_session_impl/sampler_turn.rs:2098–2132`:
  a response with no usage skips ledger recording; outside two guarded contexts
  there is already a TODO that this omission may not mark incompleteness.

All source paths above are relative to `crates/codegen/`. Negative prices from
fitting a few aggregate token columns to dollars are not a diagnosis of which
field is missing. Model mix, unobserved calls, provider billing semantics, and
incomplete settlements are not identified by that regression. This audit does
not infer prices, invent missing tokens, or claim to reconstruct historical bills.

Follow-up work should preserve per-call identity and full per-model counters,
make untrustworthy/missing cost explicit, and consume durable checkpoints with
exit/completeness information. It must not add reasoning twice, count repeated
checkpoints twice, or promise recovery of usage never returned by a provider.

## Reproduce without a model API

In the isolated Rust 1.94.0 builder described in `BUILD-CLI.md`, build:

```sh
cargo build --locked -j 4 -p xai-grok-shell --example usage_surfaces
```

Then run the six contract tests against that executable:

```sh
USAGE_SURFACES_BIN=/path/to/build/debug/examples/usage_surfaces \
  PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover \
  -s tools/tests -p 'test_usage_surfaces.py' -v
```

The tests pass only `PATH` and a new empty `GROK_HOME` to each probe; all session
files are synthetic and deleted with their temporary directory. Two cases use
GNU `timeout` and expect exit 124. Existing output directories and unknown modes
are rejected. To preserve a fixture for inspection, invoke the example with an
explicit empty `GROK_HOME` and a **new** output directory. Do not point it at
an existing session or credential directory.
