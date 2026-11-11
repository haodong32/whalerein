# Harness Conversation Contract

[Overview](../architecture.md) · [Glossary](../glossary.md) ·
[Wire schema](../../proto/v1/WIRE.md) · [Streaming](../streaming.md)

WhaleRein uses one typed conversation model across Harness connectors. DSH is the
only production connector. Other Harnesses are future work; this contract does
not claim their APIs or behavior are implemented.

## Ownership

| Component | Owns |
|---|---|
| Source adapter | Native session/history reads, actions, event interpretation, source lifecycle and opaque cursors. |
| Shared Host runtime | Pairing peers, command outcomes, watches, event sequencing, encryption and bounded delivery. |
| Access | Grants, request copies, forwarding and opaque relay. |
| Core | Channel security, cursor continuity, encrypted cache, synchronization and conversation reduction. |
| CLI | Input, human/JSON presentation and terminal behavior. |

Source adapters do not own Host identity, grants, MQTT topics or presentation.
Core and CLI do not parse source-native JSON. DSH remains authoritative for its
history and actions; WhaleRein owns transport authorization, command deduplication
and client cache state.

## Identity and typed model

A Host identifies one authenticated connector instance, not a physical machine.
Session identity is `(host_id, session_id)` throughout routing, cache and watches.
Native session IDs and cursors are opaque; cursors must never be treated as event
counts or filesystem paths.

The wire schema defines these typed updates:

- `Message` is committed content with stable identity, role and ordered blocks.
- `HistoryBatch` carries committed changes and a source cursor interval.
- `LiveFrame` carries provisional Attempt updates with an epoch, ID, block and
  normalized sequence interval.
- `RuntimeState` and `ApprovalState` report current source state independently
  of committed history.
- `HistoryReset` signals that a view/cursor must be resynchronized.

Exact fields and encodings live in [`proto/v1/WIRE.md`](../../proto/v1/WIRE.md).
Native event names and arbitrary JSON payloads are not part of the common model.

A Message is authoritative content; an Attempt is one generation effort. A
committed Message may settle an Attempt, but interrupted/retried Attempts need not
produce a Message. Preview text is provisional, never committed history.

## Source adapter

The Host consumes a source adapter with lifecycle, session listing/lookup,
`watch`, `readHistory`, `readState`, and command execution operations. The adapter
translates native data into the shared types and advertises per-session
capabilities such as resumable history, live text, interruption and approvals.
Unsupported capabilities return explicit results; adapters do not simulate them.

The adapter owns native ID mapping only when the source lacks stable IDs. Any
persisted mapping contains identity/view metadata, not conversation content.
The DSH credential binding remains outside the conversation adapter.

## History and live updates

History cursors are opaque and bound to Host, session, view and projection.
Core applies a batch only when its starting cursor matches the applied cursor;
duplicate batch identities are idempotent. Conflicts or gaps trigger repair.
Cursor and committed changes are written atomically.

Pagination reads a fixed history cut. Core stages pages and replaces the previous
complete view only after the final page succeeds. Branch changes, expired cuts,
or unavailable ranges require a reset/snapshot; they do not clear pairing.

To close the live/sync race, Core subscribes first, starts a channel-scoped Host
watch, installs the baseline history/state, then reconciles buffered updates.
Reconnect or view replacement clears provisional output and recovers committed
history plus current runtime/approval state. Watch leases and publication queues
are bounded; expiry never changes device grants.

Core owns provisional ordering, duplicate/stale filtering and final-message
precedence. Only committed messages enter the encrypted cache or advance the
history cursor. The CLI may append a suffix only when Core confirms an exact
UTF-8 prefix; otherwise it renders the corrected complete message.

## Commands, approvals and identity

`command_id` identifies a requested operation, not a Message or Attempt. The Host
journals admission before one native invocation. If execution outcome is uncertain,
the result stays `unknown`; controls are not blindly re-executed.

`SourceAdapter.execute` resolves with a confirmed invocation receipt. An explicit
`DefinitiveExecutionError` establishes a known failure or pre-invocation rejection;
an ordinary adapter error or wire error code does not establish execution outcome.
Connectors must preserve uncertainty when a native reply is lost. The Host handles
native execution, response encoding and journal writes separately. A result is
published as terminal only after its journal update succeeds; completion errors
retain `unknown`, or the durable admission for startup recovery, without invoking
the action again.

Execution evidence is scoped to one invocation. Connectors must not propagate a
nested operation's definitive error after the enclosing action has already
mutated source state. If completion storage remains unavailable, the admission
can still be `accepted` on disk and on duplicate lookup until startup recovery
marks it `unknown`; that pending state never permits another native invocation.

Send modes are explicit where supported. Acceptance or queueing is not proof that
generation finished. Interrupt acknowledgement and later runtime settlement are
separate facts.

Advertise approvals only when the source has a real pending native action and can
validate its digest/version/expiry and resolve races with a local answerer. Host
checks cannot create source-side atomicity. DSH approval behavior still requires
native release validation.

The shared Host owns its SQLite operational state. It stores peer references,
command outcomes, cursors/metadata and pending ciphertext, not a second plaintext
conversation log. Host and device keys, grants and command outcomes survive source
restart. Source changes do not rotate identity or imply revocation.

## Scope and validation

The current release targets DSH and keeps application protocol v1. It does not
add a Pi connector, runtime registry, provider abstraction, second session store,
new transport service or compatibility bridge for unpublished development builds.

Resource limits and exact fragmentation rules are defined in the
[wire contract](../../proto/v1/WIRE.md); streaming semantics are in
[Conversation streaming and history](../streaming.md). Validate with typed
contract fixtures, cross-language protocol checks, isolated integration cases and
separate real-DSH tests. FakeDSH does not certify native persistence, credentials,
approval races or reload behavior.
