# Proposal

## Why

Pending native questions can appear independently of visible conversation output, while the current session watch has no question state and clients must query questions separately. The current device-wide HTTP liveness renewal also confuses observation activity with ownership of a submitted answer, making discovery, cancellation, and recovery difficult to compose correctly.

## What Changes

- **BREAKING**: replace the existing watch orchestration with one Core session observation interface selecting Conversation, Questions, or both, over the existing single device transport owner. Its five business event families are committed history, live Attempts, runtime, Approvals, and Questions; control events do not add subscriptions.
- Give Questions independent availability and semantic epoch/revision stamps. Existing watch notifications trigger Core-owned authoritative paginated reads; only a complete same-stamp collection becomes current. History failure cannot hide questions, and no UI discovery timer is required.
- Keep native question discovery, exact root/handle ownership, arbitration, and claim cleanup inside the DSH adapter. Reading or editing never acquires a claim. Keep one-shot list/detail commands on the shared reader.
- Separate observation, HTTP answer execution, and authorization lifetimes. Persist exact-request liveness/replay facts on the existing Access PostgreSQL request row; unrelated requests cannot renew an answer. MQTT observation loss does not cancel a separately live HTTP answer, while applicable revocation remains authoritative.
- Preserve native acceptance evidence and original command recovery identities across cancellation, deadlines, lost receipts, and observer replacement. Cancellation notification is not an instantaneous distributed rollback.
- **BREAKING**: retain application protocol version **1**, support only the new contract, regenerate its bindings/fixtures, and coordinate matching builds. Add no compatibility bridge or hidden second protocol version. The unchanged number cannot reliably identify all old builds.
- Preserve keys, pins, grants, Source sessions, pager keys, and durable outcomes. Extract private observation orchestration and consolidate command heartbeat scheduling instead of wrapping duplicate implementations.

## Capabilities

### New Capabilities

- `session-observation`: one scoped session observation, independent facets, question convergence, consumer delivery, resource/privacy behavior, and coordinated version-1 replacement.

### Modified Capabilities

- `user-questions`: automatic observation remains separate from native claim acquisition; guarded answers and their accepted/unknown outcomes survive observation changes and cancellation races.
- `claim-presence`: request-specific durable liveness, monotonic expiry and replay, scoped pre-admission cancellation, and separation of MQTT observation loss from HTTP answer ownership.

## Impact

- Public Core interface and internals: `core/src/api/{core,watch}.rs`, private observation extraction, `conversation`, `commands`, `runtime`, `sync`, and encrypted local state integration.
- Source contract and Host: `host/runtime/src/contracts/source.js`, `events/watches.js`, `commands/{service,question-pager}.js`, channel/presence lifecycle, and operational journal.
- DSH: native observation and question ownership in `host/dsh/src/adapter/{base,conversation,real,questions}.js`.
- Access: request-row migrations, renewal/expiry transactions, HTTP polling/heartbeat, cancellation notification and forwarding coordination.
- Consumers and protocol: CLI, mobile bindings/event queue, Swift service/presentation, canonical `proto/v1/` schema/bindings/vectors, native journeys and deployment documentation.
- No new service, broker, database, question-ownership registry, durable question-content log, account behavior, or arbitrary multi-session transport manager. Cloud only adopts matching public artifacts; Cloud enrollment/connection debugging is a separate change.

The starting design is [the local architecture study](../../../.local-docs/plans/session-question-observation-design-2026-11-18.md). Current architecture is documented in [Core](../../../docs/architecture/core.md), [Harness contract](../../../docs/architecture/harness-contract.md), and [streaming](../../../docs/streaming.md). Source inspection distinguishes these proposed behaviors from the existing implementation; unrelated dirty-tree fixes remain out of scope. Native hook completeness and numeric resource budgets require the bounded feasibility gates in the design, not invented guarantees.
