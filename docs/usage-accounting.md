# Request usage and costs

The CLI keeps two accounting scopes. Existing `usage`, `modelUsage`,
`num_turns`, and `total_cost_usd` fields retain their accepted main-agent
response scope, including subagent usage attributed by the existing coordinator.
They do not acquire auxiliary costs or failed/retried attempts.

The new request ledger records individual transport attempts, including retries
and auxiliary work. It is **session-wide**, so it can contain earlier prompts
and child sessions. Do not add it to the existing totals: the scopes overlap.

| Surface | Request ledger field |
| --- | --- |
| Session `usage.json` | `request_usage` |
| `updates.jsonl` / `turn_completed.usage` | `sessionRequests` |
| Terminal JSON, including Messages result JSON | `session_requests` |

A terminal result is a snapshot. Background requests can still be pending at
that point. The session checkpoint receives later updates while the process
remains alive. A timeout can prevent any terminal result from being emitted;
read `usage.json` for the last atomically saved request snapshots.

## Ledger envelope

`schema_version` is `1`. `calls` contains one latest snapshot per `call_id`.
`summary` is computed from these records, including when a checkpoint is read
back. An edited or stale saved summary is not trusted.

`history_complete` is true only when recording began with a new session. A
resumed legacy file without request history stays incomplete; old counts cannot
be reconstructed from its aggregate. `recording_errors` marks failed checkpoints
or conflicting records. Neither an old file nor a failed read becomes a zero
bill.

If a syntactically valid `usage.json` has an unparseable `request_usage` extension
(for example a newer schema, an unknown enum value, or a wrong `calls` type),
the legacy `totals`, `by_model`, `main_loop_model_calls`, and `incomplete` still
load unchanged. Only the unreadable request history is discarded. Its replacement
has `history_complete=false` and `recording_errors=1`, so complete request totals
remain null while new calls accumulate known subtotals. Missing or null extensions
retain the legacy unknown-history meaning. This recovery does not repair an
invalid whole JSON document or malformed legacy fields; the storage layer's
pre-existing whole-file read failure behavior remains outside this extension.

`summary.all`, `summary.main`, and `summary.auxiliary` partition the same records.
`main` means `purpose == "main_loop"`, including child-session main loops;
`auxiliary` contains the other purposes. These physical-attempt totals differ
from the legacy accepted-response totals even for main-loop calls.

Each summary reports `model_calls`, `pending_calls`, `failed_calls`,
`interrupted_calls`, and `usage_is_incomplete`. For each token counter:

- `total` is available only with complete history, no recording errors, completed
  requests with valid primary usage, and no missing value for that counter.
- `known_total` adds the values actually reported. It is a partial subtotal when
  `total` is null; it is null if no contributing call reported the counter.
- `missing_calls` counts records without that counter. `overflow` prevents a
  wrapped or saturated sum from masquerading as a correct total.
- An empty scope has zero calls and zero known sums. This differs from a call
  whose provider omitted usage.

Complete totals are often null: a single failed retry or a pending checkpoint
interrupted before HTTP submission makes the history incomplete. Consumers should
report `known_total` / `known_usd` as partial subtotals alongside `missing_calls`,
`usage_is_incomplete`, `history_complete`, and `recording_errors`. Missing call
counts only describe retained records; they cannot count discarded or pre-recording
history. Do not interpret a null complete total as zero, or rely on it being present.

## Call records

| Field | Meaning |
| --- | --- |
| `call_id` | Random identifier per attempt; retries get new identifiers. |
| `origin_session_id` | Session that issued the request, retained when forwarded to a parent. |
| `prompt_id` | Active prompt at request start, or null for unpinned background work. Child prompt identifiers remain child identifiers. |
| `model` | Requested model identifier, not an inferred billing model. |
| `provider` | Configured `xai`, `openrouter`, `vllm`, or `compatible` profile. |
| `backend` | `chat_completions`, `responses`, `messages`, `embeddings`, `images`, or `videos`. |
| `purpose` | Main loop or auxiliary role, such as compaction, session summary, title, turn summary, web search, embedding, or media generation. Unclassified helpers use `auxiliary`. |
| `status` | `pending`, `completed`, `failed`, or `interrupted`. Completion describes the transport's terminal response, not success of a user task. |
| `started_at_unix_ms` | Local start/checkpoint timestamp in Unix milliseconds. |
| `api_duration_ms` | Elapsed attempt time at the latest snapshot; includes streaming and local observation/backpressure. Null before an update. |
| `sequence` | Monotonic snapshot revision within this attempt. Old/duplicate delivery cannot add another charge. |
| `usage` | Whitelisted provider counters and money; null if no usage was received. |

An acknowledged pending checkpoint precedes HTTP dispatch. Usage snapshots are
saved before the corresponding response/chunk reaches its consumer. A terminal
checkpoint precedes terminal delivery. Cancellation retains already received
usage; if the process dies before a cancellation update is saved, the earlier
pending record remains an explicit unknown. A failed start checkpoint can leave
a record for an attempt that was never sent: records are not proof of billing.

Child records are forwarded with the same identifier and sequence to parent
ledgers, including later background updates. Do not sum parent and child files;
the parent's ledger already includes forwarded child requests.

## Token definitions

All request-level counters are nullable. Missing, null, invalid, and explicitly
reported zero values are not interchangeable. Cumulative streaming usage
replaces prior values; it is not added once per chunk.

| Counter inside `usage` | Definition |
| --- | --- |
| `input_tokens` | Full billed input, including cache read/write where reported. |
| `uncached_input_tokens` | Fresh input. Derived only when the necessary cache counters are known. |
| `output_tokens` | Reported output, including reasoning when the provider includes it. |
| `cached_read_tokens` | Cache-read subset of input. |
| `cache_creation_tokens` | Cache-write subset of input. |
| `reasoning_tokens` | Reasoning subset of output. Never added to output again. |
| `provider_total_tokens` | Raw provider total, retained for inspection; not used to override billed input/output. |
| `invalid_fields` | Invalid types, negative values, inconsistent cache counts, or overflow were observed. |

The summary's canonical `total_tokens` is full input plus output. It does not add
cached or reasoning tokens again. Responses API context-window overrides used
by the UI do not change these billing counters.

Chat Completions reads `prompt_tokens`, `completion_tokens`, and their details.
Responses reads `input_tokens`, `output_tokens`, and their details. Messages
`input_tokens` is fresh input; full input requires its cache-read and
cache-creation values as well. A Messages start event's input counters survive
an output-only final delta. Embeddings often report input without an output
counter; the omitted counter remains unknown. Provider-specific details that
are absent remain null, even if the legacy aggregate represents them as zero.

## Provider money

No price-table estimate or token-to-dollar conversion is made. xAI positive
`usage.cost_in_usd_ticks` is divided by 10^10 and tagged `xai_usage_ticks`.
The existing xAI convention treats zero/missing ticks as unknown. OpenRouter
`usage.cost` is a finite, nonnegative USD amount tagged `openrouter_usage_cost`;
an explicit zero is a known zero. Generic compatible/vLLM usage does not create
a dollar amount.

A record's `usage.provider_cost` contains `usd` and `source` when available.
`usage.cost_usd_ticks` preserves reported xAI ticks. Summary `cost` contains:

- `total_usd`: complete amount, otherwise null.
- `known_usd` and `cost_by_source`: reported subtotals, never mislabeled as a
  complete amount when calls/history are missing.
- `xai_cost_usd_ticks`: the xAI component only, not a mixed-provider total.
- `missing_calls` and `invalid_or_overflow`: completeness/validation signals.

The status line continues to show the legacy main-agent cost scope. It uses all
reported sources in that scope and hides partial/incomplete money; an xAI-only
subtotal is not displayed as the total of a mixed-provider session.

## Covered requests and data boundaries

Recording covers all six sampling entry points (three protocols, with and
without streaming), auxiliary clients carrying the session observer, direct
web-search Responses calls, embedding batches and their internal auth retries,
and image/video generation submissions. Generic HTTP fetches, media downloads,
video status polling, and context/token-estimation requests are not additional
model submissions. Media submissions can omit usage/cost; those amounts stay
unknown, and an asynchronous video's status here refers to submission rather
than generation completion.

The request ledger stores identifiers, model/role, lifecycle timestamps,
whitelisted counters, and provider money. It does not store prompts, generated
content, request/response bodies, URLs, headers, or credentials. Accounting
checkpoint failures are nonretryable so a local write failure cannot trigger
another billable inference attempt.
