# Latency observation

[Overview](../architecture.md) · [Core](core.md) · [Access](access.md) · [DSH plugin](dsh-plugin.md)

The benchmark measures the interval between a source increment and a complete,
visible CLI stdout record. Attribution uses typed, content-free facts emitted at
the boundary that owns each step. The normative event contract is
[`contracts/latency/observation.json`](../../contracts/latency/observation.json);
the case-facts shape and ownership rules are summarized in the
[benchmark command reference](../../tools/repo/README.md#link-performance-benchmark).

## Boundaries

| Process | Owner boundaries |
| --- | --- |
| Fixture | Source emission, scheduled work, socket/runtime samples, public stdout observation, clock admission. |
| DSH adapter | Native callback receipt and normalized handoff. |
| Host runtime | Coalescing, queueing, local MQTT submission, command handling, object staging. |
| Access | Query admission/forward/reply, durable receipts and recovery, relay staging and transfer. |
| Core | Authenticated frame receipt, queue enqueue/dequeue, scoped handling disposition, delivery to the observation stream. |
| CLI | Successful completion of each public output record write. |

Every timed process reads the same kernel `CLOCK_MONOTONIC` inside one Linux
kernel boot and time namespace. Event timestamps are integer nanoseconds and
durations are direct differences; no offset, drift or remote-clock mapping is
accepted. The fixture admits each producer lifetime with a bracketing clock
check and records the run-scoped result.

## Outlets

One bounded outlet per process is shared by that process's modules. It is
created only in `trace` mode; `off` creates no writer and `aggregate` keeps the
existing component sinks. A full or stalled outlet drops records, counts loss
and never blocks business delivery. Outlet records are NDJSON lines with a
common header (`v`, `run`, `producer`, `lifetime`, `seq`, `ts`, `event`) and a
typed `data` object from the manifest. Only allowlisted fields may be emitted;
payloads, raw identifiers, cursors, credentials and capture keys never reach a
record. Business identities are replaced by run-local HMAC tokens at the
trusted boundary.

The capture is owned by the benchmark, not by the products. Products keep their
ordinary aggregate metrics; detailed capture and its files belong to the
fixture and are removed with the private case directory.

## Joins

- Requests and commands: authenticated scope, original request token and
  transport attempt.
- Streams: stream/watch tokens, connection generation, attempt/block and
  sequence range. Coalesced or split ranges are matched by their range, not by
  event count.
- Objects: parent request token plus scoped object operation.
- CLI records: process lifetime plus record ordinal on the existing NDJSON
  stream. A record write is stamped only after the writer's `write_all` and its
  required flush succeed. The fixture timestamps the read that completes the
  public record.

Missing, reversed or ambiguous edges remain unavailable; they are never
reconstructed as zero or as a serial sum. Overlapping intervals stay
overlapping and unmatched residual time stays visible. Inapplicable stages are
labelled from the actual route (for example a direct preview has no Access
application interval).
## Causal edges and residual

The model joins one preview increment across these edges:

`source_emit → dsh_callback → dsh_handoff → host_coalesce/enqueue → host_publish → core_receipt → core_delivery → cli_write → fixture_observe`

Each edge uses the run-local `stream`/`block`/`seq_start`/`seq_end` range, the
per-batch token (`HMAC(batch, "<stream>:<seq_start>:<seq_end>")`), the frame token
or the CLI record ordinal. Coalesced Host batches legitimately have a merged
batch token, so handoffs join their enqueue by sequence-range containment rather
than token equality. A split adapter frame emits one callback/handoff/batch per
published part.

Core's `core_dequeue` is stamped after handling, so delivery precedes it; the
model therefore reports queue wait separately (`wait_ns`) and joins
`core_receipt → core_delivery`, never a reversed dequeue edge. A frame that
produces no delivery is not an increment and does not create a missing edge.

For each source increment the model builds a per-increment waterfall, unions the
matched edges inside that increment window and reports `residual_ms`. Source
cadence idle is excluded from both the total and the residual. Missing or
ambiguous edges stay explicit in `unresolved`; they are never replaced by a
zero. Edge coverage is complete only when every eligible increment matched every
required edge.

`access_transport` records client-bound broker forwarding facts (connection,
generation, role, direction, bytes, outcome). It carries no stream/sequence
identity, so it describes transport cost without pretending to join a preview to
one increment; previews bypass Access application forwarding entirely.

## Terminal dispositions

`host_publish.outcome` is `accepted`, `failed`, `timeout` or `dropped`. A live
preview rejected by the Host's connected check, bounded queue or local publish
call emits one `host_publish` with `outcome=dropped`; the model reports the
`publish -> receipt` edge as `dropped` rather than `missing`. Core cannot confirm
a QoS 0 delivery, so an accepted publish with no receipt stays
`missing_boundary` (transport-unconfirmed) and is never relabelled as dropped.

The adapter records `dsh_callback`/`dsh_handoff.disposition` as
`published`, `split` or `suppressed`. A suppressed native callback keeps the
callback fact but has no normalized sequence/batch; split parts each carry their
own range and batch. Edge coverage treats `suppressed` as `inapplicable`, not as
a missing required boundary, and ignores `dropped` edges when deciding whether
the remaining chain is complete.

A reader may consume stdout after `write(2)` but before the CLI's post-flush
stamp. That pair is retained as a zero-length `reordered` edge instead of being
discarded or counted as a reversed boundary.
