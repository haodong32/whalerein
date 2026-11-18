# Design

## Context

See [proposal.md](proposal.md) for the motivation and scope. The user accepted one session subscription with five business event families, requested a clean hard cut, and explicitly selected application protocol version 1 without compatibility behavior.

Inspection of the current working tree establishes the following starting seams; it does not establish passing execution:

- [Core exports](../../../core/src/lib.rs) keep api/conversation public and other modules private. [Watch orchestration](../../../core/src/api/watch.rs) has one profile-scoped MQTT owner and gates state around history synchronization.
- [Core facade](../../../core/src/api/core.rs) exposes one-shot question reads separately and schedules the original command heartbeat while waiting for terminal results.
- [Source adapter](../../../host/runtime/src/contracts/source.js), [Host watches](../../../host/runtime/src/events/watches.js), and [DSH normalization](../../../host/dsh/src/adapter/conversation.js) carry no complete question facet. Generic state revision currently advances on each read.
- [Native question broker](../../../host/dsh/src/adapter/questions.js) observes live provider requests outside transcript history. Claimed-handle lifecycle alone is not question discovery.
- [Question pager](../../../host/runtime/src/commands/question-pager.js) already authenticates mutable identity-based cursors and sizes actual encrypted responses. It does not supply collection version consistency.
- [Access request handling](../../../access/internal/requests/requests.go) renews all same-device claims in both registration and Touch and stores liveness in memory. [Host presence](../../../host/runtime/src/host.js) currently cancels device scopes on MQTT offline.
- Existing dirty-tree changes overlap native ownership, scoped mobile events, watch registration, and recovery. Implementation must reconcile them rather than overwrite or count them as accepted work.

The existing [claim-presence spec](../../specs/claim-presence/spec.md) deliberately used blanket device-offline cancellation. This change explicitly removes that rule; it is not presented as an undocumented equivalent refactor. [Session wait](../../specs/session-wait/spec.md) remains an independent native-backed read-only operation, with its existing deadline and no-Agent-cancel semantics.

## Goals / Non-Goals

**Goals:** keep caller knowledge small; deliver current native questions without text parsing or UI timers; isolate facet readiness, recipient delivery and observer generations; retain native arbitration; make exact answer liveness restart-safe; preserve native outcome evidence.

**Non-Goals:** a new service/broker/database; a question ownership table; a durable question-content log; arbitrary multi-session MQTT multiplexing; changes to native answer restrictions, Source model/queue behavior, account enrollment, or the Cloud connection bug. Existing one-shot CLI commands remain one-shot; no new CLI flags are introduced by this proposal.

## Architecture Review

### Module ownership and dependency direction

| Module | Owns | Excludes |
| --- | --- | --- |
| DSH and its adapter | Exact native owner/handle, normalized inventory, question stamp, event interpretation, arbitration and native cleanup | Host routing, remote grant authority, presentation |
| Shared Host | Shared Source observation, typed facet refresh, scoped fanout, page budget, command admission/journal | Native JSON interpretation or plaintext question persistence |
| Access | Authorization, ciphertext relay, durable exact-request renewal/expiry/replay on existing request rows | Question bodies, answer meaning, a second native claim |
| Core api | Supported interface, explicit SessionKey and stable domain/error values | Consumer-owned protocol assembly |
| Core observation | One observation owner, interests, scoped output and independent facet scheduling | Native state inference or command lifetime ownership |
| Core observation/questions | Stamp reduction, paginated read/staging and complete view replacement | History cursors or native claim management |
| Core commands | Durable submission identity, heartbeat, bounded wait/cancel, native receipt validation and recovery | UI timer or observation ownership |
| Core conversation and sync | Message/Attempt reduction; committed history validation/cache staging | Treating pending questions as Messages |
| Core runtime/security/access | Shared channels, Noise, HTTPS/MQTT and framing | Inferring revocation from transport loss |
| CLI/mobile/Swift | Typed rendering, edits and explicit actions | Discovery polling, re-scoping old events, raw DSH/wire parsing |

Core remains self-contained Rust; SecretStore is its only external injection interface. Extract the current watch implementation into a private observation module rather than wrap it with another observer. Move heartbeat scheduling out of the facade into commands; do not retain two schedulers. These are implementation modules, not a requirement for a trait/service per row.

    CLI / mobile bindings -> api::Core
                              |-- observation
                              |     |-- conversation state
                              |     |-- question reconciliation
                              |     +-- history sync -> encrypted cache
                              +-- commands -> execution liveness / journal / recovery
                                    |
                         runtime + security + access -> Access -> Host -> adapter -> DSH

Profiles/configuration support Core identity and setup. Questions-only observation does not invoke history/readState paths that require a history head. A shared native observer uses its recipients' union of interests but commits each facet independently.

### Interface and wire contracts

The supported Core surface has one observe_session(SessionKey, interests, cancellation), explicit read_questions/read_question convenience queries, and guarded answer_question. Generic command cancellation/recovery remains shared. One profile has one active session observer; a screen requests Conversation and Questions on that same handle.

Every watch-scoped event retains SessionKey, observation identity and generation from its producer through bindings and consumer delivery. Pairing and command events retain their own scope/lifetimes and are not assigned to the currently visible conversation.

A Questions descriptor is Unsupported, Unavailable(code), or Available(QuestionStamp). A stamp has a Source observation epoch and monotonic revision. A Core QuestionView is Loading, Current, Stale, Unavailable or Unsupported with the last fully installed batches where appropriate. Available metadata is not an empty collection; only a terminal complete same-stamp read establishes Current-empty.

Question pages carry the stamp of the complete inventory actually sampled. The native batch guard remains separate: batch ID, native/source epoch, version, digest and phase. Neither a collection stamp nor a displayed card authorizes a native answer.

### Observation algorithm

1. Register the shared native observation before reading initial facets. All recipients waiting on that registration share success/failure; an unpublished subscription cannot masquerade as a working source.
2. Use independent facet reads. Questions can become visible while history is absent, slow or failed. A global authorization failure remains fatal to applicable facets.
3. Native broker insertion/removal, native projection/phase changes, owner replacement and supported local resolution hooks invalidate Questions. A question with no Message must participate.
4. Per Source/session/facet, one in-flight reconciliation captures a dirty generation. A notification during an awaited read guarantees another read; old completion cannot overwrite newer accepted state.
5. Unchanged normalized inventory retains its stamp. Token output and generic read-completion counters do not advance it. Refresh uses the existing five-second watch schedule and shared Source work.
6. Core stages authoritative pages. First page must not be older than the accepted target within its epoch. Later pages must match exactly. An unexpected epoch requires an authoritative descriptor for the current observer generation, not lexical/numeric comparison of opaque epochs.
7. Before publication, verify final-page completeness, cursor progress, scope/generation and buffered invalidations. Publish one atomic Current replacement; otherwise discard the stage, retry within reviewed resource admission, or expose Stale/Unavailable.
8. Reconnect, queue loss and missed final hints trigger reconciliation. No fixed-cut registry or frozen Host question-content copies are introduced. The mutable explicit-list cursor remains useful, but is not a future-event cursor.

Continuous churn can prevent a fresh complete view. That is explicit stale state, not partial success. Native refresh may still scan/materialize a full inventory; fewer wire queries do not prove constant-time native work. One-shot list/detail reuse the same reader, sizing and native normalization.

Stage and event byte admission must cover old/new collections and FFI copies. Coalesce superseded replacements. There is no invented question-count/product-size cap. D08 gates any unmeasured numeric budget; explicit resource failure preserves honest freshness and one-shot pagination.

### Answer execution and cancellation

Observation, answer execution, and authorization have independent lifetimes:

- Closing observation stops its refresh and delivery. It does not cancel a separately live HTTP answer.
- Core owns the answer task, request R heartbeat, original command identity, and result recovery. Follow-up request IDs do not maintain R.
- Applicable revocation fences the affected observations/executions regardless of fresh heartbeat.
- Native acceptance remains irreversible evidence; cancellation or delayed bookkeeping cannot fabricate no effect.

Store exact-request liveness on the existing PostgreSQL request row: lease deadline, monotonic live/expired/closed state, and retryable exact-cancellation delivery metadata. Exact schema names are implementation details. Registration and renewal never touch sibling claims. Renewal authenticates device/R, cannot extend the original hard deadline, and cannot revive expired R. Sweeping and renewal have one transaction winner using Access database time.

Retain a pending cancellation on the same row across Access restart or MQTT publication failure. Access-generated unknown is not proof native execution ended. Explicit cancellation and expiry stop further forwarding but allow read-only result recovery. No renewal-to-Host stream, request graph, extra execution ID, or native ownership table is required.

Cancellation metadata contains bounded Host/device/request/command/digest/deadline binding. A Host pre-admission cancellation fence uses its existing operational journal so a delayed R cannot invoke after closure; R2 has another identity. Duplicate notices are harmless and cannot overwrite accepted outcomes. Define row retention and queryability separately from native claim state.

Audit device_offline, cancelDevice, channel replacement and native prechecks together. MQTT observation loss cannot erase an admitted HTTP operation's authority. Revalidate authorization generation, exact native guards, local cancellation and deadline before native entry. A new channel cannot resurrect R.

Access expiry is not a distributed atomic native cancellation. Acceptance can win while notification is in transit. Once Host observes cancellation before entry, a not-yet-entered invocation is prevented where the verified native interface supports it. Confirmed acceptance retains its receipt; uncertain acceptance retains original-command recovery and no replay. Native claim release remains in the adapter's single lifecycle cleanup.

## Decisions

### Decision Review

CONFIRMED refers to user decisions in this chat, including the accepted redesigned architecture followed by the explicit request to formalize it. CONSTRAINED refers to an existing project requirement. Detailed technical mappings below implement that architecture; they are not assertions that every code-level choice was separately approved. PENDING is limited to the new numeric budget question and blocks its dependent implementation until evidence/decision resolves it.

| ID | Options and tradeoffs | Recommendation | Status and decision basis | Affected ACs |
| --- | --- | --- | --- | --- |
| D01 | One observer with interests; separate question watcher; full-content status push | One owner, independent facets, small descriptors and Core reads | CONFIRMED: user accepted one stream/five families and requested this proposal | AC-01–AC-13, AC-17–AC-19 |
| D02 | Native authority; secondary remote question lock | Keep exact ownership/arbitration in adapter/DSH | CONSTRAINED: AGENTS.md, Harness contract and user-question requirements | AC-02, AC-04, AC-10, AC-19–AC-24 |
| D03 | Operation-scoped liveness; device-wide liveness | Exact R; observation loss independent of live HTTP answer | CONFIRMED: accepted redesign explicitly separated observation/execution/authorization; supersedes old presence rule | AC-19, AC-22–AC-35 |
| D04 | Volatile lease map; durable request-row lifecycle; new registry | Existing PostgreSQL row and Host journal, no new ownership registry | CONSTRAINED: server state/tasks use PostgreSQL; existing durable request/journal seams support the accepted restart-safe lifetime | AC-15, AC-26–AC-35 |
| D05 | Protocol 2; dual contracts; protocol 1 hard cut | Protocol 1, only new behavior, coordinated artifacts | CONFIRMED: user explicitly said to retain version 1 and require no compatibility | AC-14, AC-15 |
| D06 | Existing moving cursor with stamped assembly; frozen snapshot registry | Retain moving cursor; only same-stamp assembly is Current | CONSTRAINED: existing identity cursor semantics; no new snapshot-content registry needed for D01 | AC-07, AC-08, AC-16 |
| D07 | New timing promises; current request/refresh timing with explicit failure semantics | Retain observed 10s lease, Core 1s heartbeat, 5s watch refresh, existing hard request deadlines | CONSTRAINED by current constants/contracts; these are controlled scheduling policies, not a new network/native release SLA | AC-09, AC-26–AC-30, AC-35 |
| D08 | Reuse existing bounded byte admission; introduce a new numeric resource/product cap | Measure existing budgets first; never silently truncate or guess a cap | PENDING technical budget evidence: F01; obtain user review if a new product limit or acceptance threshold is necessary | AC-08, AC-16, AC-17 |
| D09 | Preserve only supported native semantics; assume arbitrary label restrictions or arbitration | Preserve exact native input and independently verified open/continued gates | CONSTRAINED by existing user-questions requirements and earlier confirmed native decisions | AC-19–AC-24, AC-28, AC-35 |

### Capability acceptance gates

| Gate | Required acceptance | Native requirement and partial-delivery rule |
| --- | --- | --- |
| Observation contract | AC-01–AC-18 | AC-02/04/09/10/16 require real DSH observations where native behavior is asserted. Pure contract success alone is not native discovery acceptance. |
| Question read/discovery | AC-02–AC-13, AC-16–AC-20 | Verify broker-only/cold/continued/local-owner changes on the pinned Source; history can be independently accepted. |
| Open/continued answer execution | AC-19–AC-24, AC-25–AC-35 | Each advertised answer mode needs its own native arbitration, cleanup, and receipt branches; a read-only unsupported test cannot satisfy its positive gate. |
| Liveness and persistence | AC-25–AC-35 | Real PostgreSQL/broker/CLI fault cases plus native release evidence. Publication counters do not prove native release. |
| Full new-contract workflow | All AC-01–AC-35 | Requires every mandatory subcase, resolved D08, consumer binding behavior, version-1 cutover, and preserved-state recovery. No partial gate is reported as full completion. |

These are acceptance gates, not a requirement to add more protocol feature flags. Keep existing read/open/continued capability distinctions; a shared flag is accepted only when every operation it covers has the required evidence.

## Acceptance Review

The table covers every spec scenario. Exact mandatory subcases and implementation/test ownership are in tasks.md. Planned test identities are not claims that tests exist.

Thirty-five change-local ACs extend three retained named baseline scenarios. Existing scenario titles are preserved as required by OpenSpec synchronization; the additional baseline rows below bind them to their owning AC subcases rather than silently removing them.

| AC ID | Scenario and required outcome | Verification method | Source scenario |
| --- | --- | --- | --- |
| AC-01 | One observer, five business families; every mandatory branch is enumerated in tasks.md. | Core/Host contract + CLI regression | [AC-01](specs/session-observation/spec.md#scenario-ac-01-one-observation-carries-five-business-event-families) |
| AC-02 | Questions without history; every mandatory branch is enumerated in tasks.md. | Core/adapter + native DSH | [AC-02](specs/session-observation/spec.md#scenario-ac-02-questions-only-observation-has-no-history-dependency) |
| AC-03 | Independent availability; every mandatory branch is enumerated in tasks.md. | Core/consumer | [AC-03](specs/session-observation/spec.md#scenario-ac-03-unsupported-or-failing-questions-does-not-end-conversation) |
| AC-04 | Initial subscription race; every mandatory branch is enumerated in tasks.md. | Host/Core + native DSH | [AC-04](specs/session-observation/spec.md#scenario-ac-04-initial-question-discovery-closes-the-subscription-race) |
| AC-05 | Dirty read and late completion; every mandatory branch is enumerated in tasks.md. | Adapter/Host/Core barriers | [AC-05](specs/session-observation/spec.md#scenario-ac-05-a-change-during-an-in-flight-read-is-not-swallowed) |
| AC-06 | Stable semantic stamp; every mandatory branch is enumerated in tasks.md. | Adapter/Core request counters | [AC-06](specs/session-observation/spec.md#scenario-ac-06-unchanged-inventory-does-not-retransmit-question-content) |
| AC-07 | Same-stamp page assembly; every mandatory branch is enumerated in tasks.md. | Core/protocol | [AC-07](specs/session-observation/spec.md#scenario-ac-07-pagination-cannot-replace-a-newer-target-with-old-or-mixed-pages) |
| AC-08 | Partial failure and churn; every mandatory branch is enumerated in tasks.md. | Core/consumer | [AC-08](specs/session-observation/spec.md#scenario-ac-08-partial-failure-or-sustained-churn-retains-honest-stale-state) |
| AC-09 | Lost final hint repair; every mandatory branch is enumerated in tasks.md. | Host/Core + native DSH | [AC-09](specs/session-observation/spec.md#scenario-ac-09-lost-final-notification-is-repaired-by-existing-refresh) |
| AC-10 | Reconnect and owner replacement; every mandatory branch is enumerated in tasks.md. | Core/binding + native DSH | [AC-10](specs/session-observation/spec.md#scenario-ac-10-reconnect-and-source-replacement-discard-obsolete-observation) |
| AC-11 | Consumer scope and authorization; every mandatory branch is enumerated in tasks.md. | Core/binding + chain | [AC-11](specs/session-observation/spec.md#scenario-ac-11-old-queued-content-and-foreign-scope-never-enter-a-new-session) |
| AC-12 | Shared recipient isolation; every mandatory branch is enumerated in tasks.md. | Host delivery barriers | [AC-12](specs/session-observation/spec.md#scenario-ac-12-shared-observation-isolates-slow-and-closing-recipients) |
| AC-13 | CLI and GUI integration; every mandatory branch is enumerated in tasks.md. | CLI binary/Swift + native client | [AC-13](specs/session-observation/spec.md#scenario-ac-13-existing-consumers-migrate-without-independent-polling) |
| AC-14 | Version-one hard cut; every mandatory branch is enumerated in tasks.md. | Protocol + artifact conformance | [AC-14](specs/session-observation/spec.md#scenario-ac-14-new-version-one-artifacts-enforce-the-new-contract) |
| AC-15 | Retained identity and outcomes; every mandatory branch is enumerated in tasks.md. | Upgrade fixtures + native procedure | [AC-15](specs/session-observation/spec.md#scenario-ac-15-hard-cut-preserves-identity-and-recovery-facts) |
| AC-16 | Budgets and privacy; every mandatory branch is enumerated in tasks.md. | Protocol/Host/Core + native procedure | [AC-16](specs/session-observation/spec.md#scenario-ac-16-large-or-private-collections-retain-their-correct-meaning) |
| AC-17 | Owner cleanup and resource failure; every mandatory branch is enumerated in tasks.md. | Core/Host/binding native OS | [AC-17](specs/session-observation/spec.md#scenario-ac-17-owner-close-and-resource-pressure-remain-bounded) |
| AC-18 | Public interface and module ownership; every mandatory branch is enumerated in tasks.md. | Rust consumer + architecture checks | [AC-18](specs/session-observation/spec.md#scenario-ac-18-core-remains-self-contained-and-native-neutral) |
| AC-19 | Discovery is not a claim; every mandatory branch is enumerated in tasks.md. | Adapter/Core/native DSH | [AC-19](specs/user-questions/spec.md#scenario-ac-19-reading-and-editing-do-not-own-or-renew-an-answer) |
| AC-20 | Native guards and arbitration; every mandatory branch is enumerated in tasks.md. | Adapter + native DSH | [AC-20](specs/user-questions/spec.md#scenario-ac-20-changed-owner-or-phase-rejects-stale-submission) |
| AC-21 | Exact native answer semantics; every mandatory branch is enumerated in tasks.md. | Core/CLI + native DSH | [AC-21](specs/user-questions/spec.md#scenario-ac-21-supported-answers-retain-exact-meaning) |
| AC-22 | Exact native cleanup; every mandatory branch is enumerated in tasks.md. | Adapter/Host + native DSH | [AC-22](specs/user-questions/spec.md#scenario-ac-22-exact-native-cleanup-does-not-affect-another-execution) |
| AC-23 | Acceptance versus cancellation; every mandatory branch is enumerated in tasks.md. | Host/Core + native DSH | [AC-23](specs/user-questions/spec.md#scenario-ac-23-cancellation-races-native-acceptance) |
| AC-24 | Recovery survives observer replacement; every mandatory branch is enumerated in tasks.md. | Core/Host/binding + chain | [AC-24](specs/user-questions/spec.md#scenario-ac-24-lost-receipt-and-observer-replacement-preserve-one-invocation) |
| AC-25 | MQTT is not HTTP owner death; every mandatory branch is enumerated in tasks.md. | Core/Host real broker + native DSH | [AC-25](specs/claim-presence/spec.md#scenario-ac-25-mqtt-observation-ends-while-the-http-answer-remains-live) |
| AC-26 | Exact authenticated renewal; every mandatory branch is enumerated in tasks.md. | Access/PostgreSQL + chain | [AC-26](specs/claim-presence/spec.md#scenario-ac-26-unrelated-same-device-traffic-cannot-renew-an-answer) |
| AC-27 | Core-owned full execution heartbeat; every mandatory branch is enumerated in tasks.md. | Core/Access barriers | [AC-27](specs/claim-presence/spec.md#scenario-ac-27-original-answer-heartbeat-survives-follow-up-queries) |
| AC-28 | Killed answer with sibling traffic; every mandatory branch is enumerated in tasks.md. | Real CLI/Access/Host + native DSH | [AC-28](specs/claim-presence/spec.md#scenario-ac-28-killed-answer-releases-despite-unrelated-live-activity) |
| AC-29 | Monotonic renewal and expiry; every mandatory branch is enumerated in tasks.md. | PostgreSQL concurrent transactions | [AC-29](specs/claim-presence/spec.md#scenario-ac-29-renewal-and-expiry-have-one-monotonic-winner) |
| AC-30 | Durable liveness and replay; every mandatory branch is enumerated in tasks.md. | PostgreSQL/Access/broker fault fixtures | [AC-30](specs/claim-presence/spec.md#scenario-ac-30-restart-and-publication-failure-preserve-expiry-facts) |
| AC-31 | Cancellation before admission; every mandatory branch is enumerated in tasks.md. | Host journal + chain | [AC-31](specs/claim-presence/spec.md#scenario-ac-31-cancellation-precedes-request-admission) |
| AC-32 | Acceptance during notification transit; every mandatory branch is enumerated in tasks.md. | Host/Core/native DSH | [AC-32](specs/claim-presence/spec.md#scenario-ac-32-native-acceptance-wins-during-cancellation-transit) |
| AC-33 | Revocation remains authoritative; every mandatory branch is enumerated in tasks.md. | Access/Host + chain | [AC-33](specs/claim-presence/spec.md#scenario-ac-33-revocation-stops-the-applicable-live-scopes) |
| AC-34 | Presence/cancel scope validation; every mandatory branch is enumerated in tasks.md. | Broker/Access/Host + chain | [AC-34](specs/claim-presence/spec.md#scenario-ac-34-spoofed-or-foreign-presence-cannot-cancel-another-scope) |
| AC-35 | Honest deadline fallback; every mandatory branch is enumerated in tasks.md. | Fault fixtures + native DSH | [AC-35](specs/claim-presence/spec.md#scenario-ac-35-dependency-failure-uses-an-honest-bounded-fallback) |

| Retained baseline scenario | Required outcome and owning subcase | Verification method | Source scenario |
| --- | --- | --- | --- |
| Killed answer process | Exact claim releases and a later answer succeeds; AC-28.baseline-killed-process | Real CLI/Access/Host and native release procedure | [Baseline](specs/claim-presence/spec.md#scenario-killed-answer-process) |
| Spoofed device event | Foreign will cannot abort another device; AC-34.baseline-spoofed-device | Broker ACL and chain scope test | [Baseline](specs/claim-presence/spec.md#scenario-spoofed-device-event) |
| user-questions/AC-14 Release open-question claims | Exact cleanup, stale-handle rejection and native capability gate; AC-22.baseline-release-scenario | Adapter and real native lifecycle procedure | [Baseline](specs/user-questions/spec.md#scenario-ac-14-release-open-question-claims) |

## Risks / Trade-offs

- [Native hints are incomplete] -> authoritative reconciliation on existing refresh; F02 identifies actual hooks and read guarantees before enabling affected native behavior.
- [History blocks question readiness] -> independent Source facet reads and Core reducers; verify zero history calls for Questions-only.
- [Stamps certify incoherent native reads] -> serialized owner-aware sampling; dirty rerun or explicit unavailable instead of inventing a revision.
- [Churn/starvation and large inventory allocation] -> bounded staging/queues, explicit stale, D08 budget gate; wire pagination does not bound native memory.
- [Observation disconnect aborts live answer through a second callback] -> audit all channel/presence/cancel paths and test both directions of independence.
- [Expiry races acceptance] -> monotonic Access closure with eventual exact Host cancellation; native outcome evidence wins.
- [Version 1 cannot distinguish every old build] -> coordinated exact artifacts and required-field validation, no mixed-build promise.
- [Unrelated dirty-tree fixes alter seams] -> inspect their final code/diff before implementation; preserve them and bind evidence to a dirty-tree fingerprint.

## Migration Plan

1. Snapshot current source/tests and reconcile unrelated work; complete F01–F03. Do not implement a guessed new product bound while D08 is pending.
2. Change the canonical version-1 contract and Core-owned types; regenerate bindings and fixtures with an explicit baseline rationale. Reserve removed tags and eliminate old interface paths.
3. Implement adapter facets, Host observation/pages, Core reconciliation and scoped bindings in dependency order. Preserve existing Message/Attempt/sync semantics and one-shot CLI behavior.
4. Add idempotent migration for request liveness/replay on existing rows; establish exact closed-request admission rules and command recovery for rows of unknown forwarding history.
5. Stop old clients/connectors, upgrade the coordinated Access/Host/Core/CLI/mobile set, and rebuild transient observers. Preserve keys/pins/grants/journals; do not reinterpret old queued events or signed pairing payloads.
6. Verify every gate against the final tested tree. Cloud adopts that public artifact revision separately; it does not host question semantics.
7. Rollback is a stopped-stack restore of coordinated compatible artifacts/state, subject to the existing forward-migration policy. Never run an older Access binary against a newer unknown ledger or reset keys to force startup.

Maintained architecture/security/operator documentation is updated only from verified final behavior. Planning files and dated results stay ignored; do not copy this proposal into tracked guides.

## Bounded feasibility gates and agent details

- **F01 resource admission:** read the existing transport/preview/event queue constants and native/protocol budgets; inventory staging and FFI copies. Produce an exact byte-accounting plan and below/at/above case bindings. D08 is unresolved until existing admission is demonstrated sufficient or a necessary new limit is separately reviewed. No number is invented here.
- **F02 native observation:** inspect pinned DSH package contracts, existing native driver capabilities and question arbitration tests. Identify hooks for broker-only creation/removal, continued/local resolution and owner replacement, and a coherent inventory read. A missing hook can use verified existing refresh; a missing authoritative/native cleanup primitive blocks the affected advertised capability rather than a guessed fallback.
- **F03 lifecycle migration:** inspect SQL request transitions, initial submission wait, channel cleanup callbacks, Host journal admission and current cancellation fixtures. Record exactly how early cancellation is fenced, uncertain status differs from invocation completion, and historical rows are handled without replay. No generic future exploration task replaces this inventory.

These are specific evidence gaps with bounded procedures. Read-only inspection is separated from later executable feasibility/tests; no runtime mutation or implementation occurred during this proposal. No unresolved business/compatibility question is silently selected from an earlier agent-written plan.
