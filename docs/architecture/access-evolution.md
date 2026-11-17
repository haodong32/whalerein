# Access deployment and identity redesign

Status: Standalone mode is the current implementation scope. External account
mode, multi-instance hosting and regional automation are future design targets;
their descriptions are not shipped capability claims. A clean break in packages, schema and protocol is allowed; no
legacy compatibility layer is required. Existing deployments must explicitly
choose migration or re-enrollment before an implementation removes old data.

[Overview](../architecture.md) · [Current Access](access.md) ·
[Core](core.md) · [Host](dsh-plugin.md) · [Security](../security.md)

## Goals

Run a complete self-hosted installation without an official account, license
server or outbound management connection. Reuse that same open-source runtime
in independent managed regions. Allow a private account and commerce program to
authorize automatic pairing and manage regional policy. Automate replacement
of compute instances without making them owners of durable identity or state.

The initial implementation remains Go and PostgreSQL with Mosquitto. Core remains
self-contained Rust with SecretStore as its only injected capability. Source
operations remain behind the Host adapter. Neither Access nor management gains
access to conversation plaintext or endpoint private keys.

## Current release scope and simplicity

This release delivers Standalone mode for self-hosting: local administration, Host
enrollment, invitation pairing, reconnect, revocation and restart recovery. It
keeps a small runtime with explicit internal responsibilities and versioned
persistence. It does not implement External account mode, client login,
a central account service, commerce, managed regional
synchronization, automatic pairing or cluster orchestration. Sections describing
those capabilities are later design constraints, not current implementation work.

Keep the architecture small and explicit:

- Run one Access process with HTTP and MQTT responsibilities separated internally.
  Separate deployable roles and replica coordination only when implementing
  multi-instance hosting.
- Keep PostgreSQL and Mosquitto as the only server dependencies. Use direct module
  calls and ordinary transactions; add no generic workflow engine, event bus,
  plugin framework or distributed scheduler.
- Expose only operations needed by actual callers. Do not add a public Go
  embedding facade solely for a hypothetical private regional program, mirror
  every internal service, or create unused interfaces and empty adapters.
- Implement the local authorization path first. Keep ownership and grant checks
  separate from authority-specific decisions so a managed implementation can
  be added later without changing protocol safety rules.
- Hard-cut obsolete structures when necessary. Do not maintain parallel old/new
  implementations or rewrite working cryptography and deduplication without a
  concrete requirement. Hard cuts do not authorize deleting deployed state.

Self-hosting acceptance includes initial administrator credential setup without
an official service, authenticated first Host enrollment, invitation pairing,
lost-response recovery and documented credential-loss procedures. Database backup
and restore instructions must account for Host-local peer/revocation state and
client identity. Restore must reconcile revoked enrollments before new channels
are admitted. A healthy process alone is not evidence of successful recovery.

## Terms and ownership

Workspace and account enrollment below describe the later external-account model.
Standalone mode owns resources at installation scope and stores no fixed workspace
row. Migration adds real external ownership only when that mode is implemented.

| Term | Meaning and authority |
| --- | --- |
| Principal | A user or service identity asserted by a configured authority. It is not a device key. |
| Workspace | The ownership and policy scope containing Hosts and enrolled devices. A personal account has a personal workspace. |
| Region | An independent Access installation with its own database and broker. A workspace has exactly one home region initially. |
| Device | A client cryptographic identity with a separate enrollment and transport credential. |
| Host | A Connector's cryptographic endpoint, not an Access server or physical machine. |
| Grant | A regional device-to-Host authorization established through pairing. Membership alone is insufficient. |
| Enrollment | The approved binding of a Host or device key to a workspace. Disabling it prevents new pairing as well as access. |
| Entitlement | An effective permission or limit, rather than an order, price or payment record. |
| Access instance | Replaceable compute running API, relay, or both roles within a region. |

Central management owns accounts, membership, subscriptions and the workspace
region directory. Access owns regional enrollment enforcement, pairing, grants,
request copies, revocation delivery and measured usage. Central subscription
records do not replace regional execution facts.

Device key identity, workspace enrollment and Host grants are distinct. Removing
a member or enrollment revokes access only within the affected workspace; global
device disablement is an explicit separate operation. Reusing a key does not
transfer ownership or grant membership in another workspace. Persist and query
authorization with its service/workspace scope rather than bare external IDs.

## Operating modes

The target design uses the same open-source Access program in two operating
modes. Mode describes the identity and authorization source, not who hosts the
server or whether the deployment is commercial.

| Mode | Authority and connection flow | Account-service dependency | Delivery |
| --- | --- | --- | --- |
| Standalone mode | Local administration and invitation-link/QR pairing, followed by device-authenticated reconnect. | None; no user registration or login is required. | Current release scope. |
| External account mode | A configured compatible account service supplies identity, ownership and policy; login enables Host discovery and automatic pairing. | The selected official or operator-provided account service. | Later release scope. |

Both modes can be self-hosted. External account mode is not an official-only or
commercial-only edition. It uses the same Access program and shared pairing,
grant, encrypted relay, revocation and recovery implementation; enabling it later
does not require a fork or custom compilation. Local versus external authority
must be explicit, with no silent fallback when an external service is unavailable
or its configuration is invalid. A later mode transition needs an explicit
ownership/enrollment migration; changing configuration must not silently attach
local Hosts, device grants or Host pins to an external account. This release
implements Standalone mode only;
do not expose an External account mode option before it functions end to end.

## Deployment shapes

The right-hand example shows a later multi-region deployment of External account
mode. Neither external accounts nor self-hosting require multiple regions or
separate API/relay processes.

```text
Standalone mode                   External account mode

Client + Host                      Account / commerce / region directory
      |                                      | authenticated management
Access (API + relay)                   +-----+-----+
      |                                |           |
PostgreSQL + Mosquitto                    Region A    Region B
                                    API pool    API pool
                                    relay       relay
                                    PG + Mosquitto   PG + Mosquitto
```

Standalone mode uses a local authority scoped to the installation; manual invitations
provide a complete connection path without registration or login. After pairing,
clients reconnect using their local device identity and authorization. Operators
do not have to implement an account system. An operator wanting account-based
discovery and automatic pairing can integrate an existing identity system with
workspace/enrollment authorization; identity-provider login alone does not create
Host grants. Official services and official signing roots are not compiled-in
requirements. Loss of managed configuration must fail readiness, never silently
switch to local unrestricted authorization.

Managed workspaces are assigned to a region on creation. Host registration and
client discovery use that assignment, not client-IP routing to an arbitrary
region. Each region has stable HTTPS and MQTT names independent of instance
addresses. All Hosts in a workspace initially use the same region, keeping
quota allocation local. Cross-region workspace migration is a separate operation.

## Open-source Access and external account services

External account mode will add generic identity integration that validates
external authorizations and enforces regional state. Access does not
register users, accept account passwords, implement account recovery or process
payments. Central account management is the official managed deployment's
authority, not a mandatory global authority for all WhaleRein installations.

Use the same open-source Access program for both operating modes.
The later generic identity integration, authorization verification, management
endpoints, regional policy synchronization and usage reporting are open-source
Access capabilities. Use a local authority in Standalone mode, or
explicitly configure a compatible account service and its trust material for
External account mode. The current release implements only the local
path; it does not add nonfunctional account-service configuration.

No private regional adapter, commercial startup entry point or custom Access
build is required. The private central program runs separately and communicates
with unmodified Access through a documented public protocol. Access does not
implement registration, account recovery, pricing or payment processing.

| Responsibility | Location |
| --- | --- |
| Generic authorization verification, pairing and grant enforcement | Open-source Access |
| Generic management endpoints, policy synchronization and usage reporting | Open-source Access, implemented in the later account-integration phase |
| Registration, login, recovery, accounts, membership, subscriptions and payments | External account service; the official implementation is private |
| Local administration and invitations | The same open-source Access program, with no account-service dependency |

Other operators can implement their own account service against the same public
protocol and configure stock Access to use it. An existing identity provider can
supply login, but still needs the workspace/enrollment and synchronization
integration to implement automatic Host pairing. Official hosting is one
implementation, not a special protocol path. Account services never write Access
tables directly, and Access builds independently of their source code.

Internal modules use narrow interfaces for validating enrollment/pairing authority
and applying versioned effective policy. Policy results can only restrict access;
they cannot bypass proof of key possession, workspace isolation, disabled
enrollment, active grants or protocol validation. A local implementation and a
managed implementation exercise the same interfaces.

For later central synchronization, expose one complete versioned apply operation
per required resource change. The runtime transaction owns inbox deduplication,
version checks and enforcement-state updates, including the applied result. The
management handler must not acknowledge application separately from the state
mutation. Keep this transaction inside Access-owned persistence instead of
exposing SQL pools or inventing a cross-service transaction abstraction.

Enforce those checks on HTTP admission and reads, broker login/ACL callbacks,
queued-work dispatch and Host-to-device event forwarding. A request admitted
before disablement is rechecked before dispatch; define the dispatch/disable
ordering transactionally. Work already sent may complete and must retain its
real outcome. Policy expiry must not prevent revocation delivery or fabricate
command failure. Broker authorization caches and existing subscriptions must be
included in the disable/expiry design, not just new HTTP requests.

Do not add a general plugin loader, remote code execution or a global mutable
service registry. Precise method signatures should follow the first implemented
local and managed flows rather than exposing all current internal services.

## Private repository integration

Keep the official account/commerce implementation and commercial infrastructure
automation in a separate private repository. It deploys released open-source
Access binaries or images and connects over the public management protocol; it
does not need to import or embed Access, fork it or ship a commercial regional
executable. Pin Access image digests or release revisions alongside the private
central release and check protocol capabilities during deployment.

The private repository may include `upstream/whalerein` as a Git submodule for
source inspection, joint development or reproducible source builds. This is
optional, not the runtime integration mechanism. If used, pin the commit and
reject missing, mismatched or dirty inputs for release source builds. Only add
a `go.work` if there is an actual cross-module Go dependency, such as shared
protocol bindings; select it explicitly in CI and keep dependency versions
aligned. Go's `internal` visibility rules still apply.

The public repository must build and test independently without private source
or configuration. Use separate build contexts or allowlisted image inputs so
private configuration does not enter public artifacts. Ordinary image/runtime
inputs exclude agent skills and working documentation unless explicitly needed.

Private root agent instructions govern private work; public subtree instructions
also apply when working in the submodule, subject to the agent's instruction
precedence. Configure skill discovery deliberately. These instructions are work
guidance, not a confidentiality mechanism. Keep commercial source, secrets and
private documentation outside the public subtree. Make public changes in the
public repository and then update the private submodule pointer.

## Fully open-source clients

Keep clients fully open-source, including optional login, service selection,
authorization exchange and Host discovery. Support both standalone invitation
pairing and account-authorized automatic pairing in the same client. Official
hosting can be a preset service; users can add another compatible service.
Login is optional and must not gate standalone conversation operations.

Client application code handles the selected authority's login and account
session, while Core verifies pairing authorization and owns keys, encryption,
synchronization and controls. Core has no official-account-specific behavior or
additional injected identity capability. The selected authority's service
metadata identifies supported enrollment/discovery capabilities; a generic login
provider alone is not a complete managed-hosting implementation.

Keep prices, promotions, invoices and payment workflows in the chosen service's
web interface initially. Public clients and verification keys do not expose
server signing keys or require the commerce implementation to be public.
Trust is service-scoped: selecting an authority cannot replace another service's
Host pins, claim its resources or grant access to an unrelated standalone Host.

## Regional runtime roles

This is the later multi-instance deployment shape. The current release keeps
these responsibilities inside one process without role flags, standby election
or relay epochs. Restart-safe state is required now; distributed coordination
is not.

**API** owns HTTP admission, broker authentication/ACL callbacks, regional
management admission and operation status reads. API instances use PostgreSQL;
they do not need a process-local MQTT waiter or pairing claim to answer a request.

**Relay** owns the regional Access MQTT session, opaque envelope reconstruction,
Host result/event processing, pairing exchanges, forwarding and revocation
replay. Start with one active relay per region and optional standby instances.
Multiple API instances can serve the same region. An `all` role runs API and
relay in one process for self-hosting. Roles share one codebase, not separate
business implementations.

HTTP accepts durable operations and reads their status; relay work is claimed
from PostgreSQL. Pairing and enrollment no longer depend on the HTTP instance
that received the first step. Relay-local fragment and Noise exchange state may
be discarded on restart; the durable operation explicitly requests a fresh
handshake attempt without discarding established identity or grants.

This design scales API capacity and supports relay replacement; it does not
claim horizontal MQTT throughput scaling. Host-based relay shards can be added
after measured demand. Shared MQTT subscriptions alone do not solve waiter,
fragment, presence, ordering or revocation ownership.

Relay failover requires a lease and monotonically increasing ownership epoch.
Database mutations reject stale epochs. MQTT publication and Host readiness /
revocation gates need authenticated epoch enforcement too: a database lease
alone cannot prevent a paused old relay from resuming publication. Define and
test that protocol before enabling automatic standby promotion. Until then,
replacement requires confirmed termination of the previous relay.

Presence is broker-derived observation with a freshness bound, not durable
authority. API instances read a regional observation maintained by the relay or
query broker administration; a restart must not classify every Host as offline
because an instance missed non-retained lifecycle events. Client events remain
recoverable through the existing synchronization/snapshot model.

## Enrollment and automatic pairing

1. A Host proves possession of its key when enrolling into a workspace. Managed
   enrollment uses a scoped account authorization, replacing the deployment-wide
   shared Host provisioning credential. Broker credentials are individually
   scoped and revocable. Local enrollment uses operator-approved authority.
2. A client generates its device key locally and authenticates to its chosen
   account authority. The authority issues a short-lived pairing authorization
   for a specific region, workspace, Host identity and device key.
3. Access validates issuer, audience, purpose, expiry, enrollment state and policy.
   The authorization ID is durably bound to one idempotent pairing operation;
   retries cannot create a second grant or change the bound keys.
4. Core verifies the expected Host key against the actual Noise handshake. Host
   verifies the approved device key and authorization. Managed automatic trust
   explicitly trusts the configured account authority; manual pairing uses user
   fingerprint confirmation. Neither mode treats an unchecked `confirmed=true`
   as evidence of identity.
5. Reuse the security semantics of staged Host authorization and atomic regional
   grant/activation commit. Report ready only after Host activation is confirmed.
   A retry or failed replacement must not revoke an older valid grant.

Standalone pairing binds a client-generated random credential hash into Noise
FINISH. Core retains the confirmed request and Host pin in SecretStore before
sending it. Access persists the exact request digest and a token-free completion
receipt in the same transaction as the grant and activation outbox. `pair-resume`
replays that request after a lost response or restart and requires an unexpired,
active grant generation. Host staging retains its verified FINISH replay binding.
An operation ID or invitation alone cannot substitute another device or credential.
No additional credential-encryption scheme or server plaintext secret is needed.
Historical restore requires explicit credential invalidation and re-enrollment;
see the [backup and restore procedure](../../deploy/README.md#backup-and-restore).

Account authentication stays outside the cryptographic transport. A client shell
can complete login and supply an opaque authorization to Core's supported API;
Core performs protocol validation itself and gains no injected account or HTTP
callback. The generic Host runtime validates pairing authority before any
SourceAdapter call. Endpoint private keys are never synchronized via accounts.

Authorization includes a versioned enrollment binding. Disabling a device or
removing membership invalidates older authorizations, blocks automatic re-pairing,
revokes relevant grants and disconnects active transport. Re-enrollment requires
a fresh authenticated approval. Personal-account auto-pairing does not imply that
all future organization members can access every Host; role policy stays explicit.

An offline Host produces a waiting state with a deadline. An expired approval
requires renewal, not an indefinite queued capability. Existing Host pins cannot
be silently replaced by an account refresh. Key rotation and ownership transfer
are explicit re-enrollment operations.

## Central-to-region interaction

There are two distinct paths:

```text
Client -> central account service: login and request scoped authorization
Central -> client: regional endpoint discovery and short-lived authorization
Client -> regional Access: present authorization and start pairing

Account service -> Access management API: membership, policy, disable/revoke changes
Access -> account service: applied versions, revocation progress and usage batches
```

The first path carries a user/device authorization. The second is authenticated
service communication for durable state synchronization. Do not accept a user's
login token as a regional management credential, or treat a service certificate
as authority to pair an arbitrary device. Regions validate pairing assertions
locally against explicitly configured issuer keys and current regional state.

Use versioned HTTPS management operations, separately authenticated from broker
callbacks and device APIs. mTLS authenticates service identities. Authorization
maps each identity to its region and permitted operations. Each region has its
own credentials and cannot write another region's observations or billing data.
Provision trust during deployment, support overlapping rotation keys, and reject
unknown issuers. Do not reuse the current broker callback token or admin bearer.

Central management sends effective membership/enrollment changes, entitlements
and disable/revoke operations. Regions report applied versions, revocation
progress and usage batches. Pairing assertions are separately signed because
clients can carry them; ordinary authenticated management calls do not also
require custom per-message signatures.

Persist a central change and its outbound delivery record together. Regional
inbox deduplication and state updates commit together. An operation has a stable
ID, immutable payload binding, target region, per-resource version and expiry
where applicable. Reject conflicting duplicate IDs and stale updates. Persist
application acknowledgment before replying. Retry after lost responses and
periodically reconcile snapshots after missed changes or retention gaps.

Regions durably record usage with unique batch IDs. Central ingestion deduplicates
before billing. Global quota totals are not transactionally strict across regions;
initial workspace limits are allocated atomically in its home region. Snapshots
must not restore a revoked grant or reset consumed quota accidentally.

No regional runtime request should synchronously consult central management.
In managed mode, cached identity/policy authorizations have explicit validity
deadlines. New enrollment needs fresh authority. Existing operations may continue
only within the last approved validity window. After it expires, deny new
forwarding and protected event delivery while permitting safe teardown and
revocation; keep persisted command outcome records for later recovery. Define
and test refresh cadence, maximum stale-policy duration and revocation delivery
targets before release. A partition cannot provide both immediate central
revocation and unrestricted regional availability.

## Regional identity data

Access stores the minimum identity projection needed to enforce local decisions,
not a complete copy of the central account database. An external principal is
identified by both issuer and subject; identical subject strings from different
authorities are different principals.

| Regional Access data | Central account and commerce data |
| --- | --- |
| Issuer/subject references and workspace IDs | Complete account profiles and login credentials |
| Relevant membership permissions, versions and validity deadlines | Organization and membership administration |
| Host/device ownership bindings, enrollment state and grants | Account recovery and login session administration |
| Effective entitlements, limit allocations and consumed usage | Pricing, subscriptions, orders and payment records |

Email addresses, phone numbers, account passwords and central login/refresh
tokens are not required regional enforcement data. The central integration uses
scoped service identities for synchronization, not user login sessions.
Regional identity projections still
require deletion, retention and backup handling. Removal must retain sufficient
versioned revocation state to reject stale authorizations and delayed updates;
it must not erase the evidence needed to prevent re-enrollment by a removed device.

## Persistence and protocol

The implementation uses explicit versioned migrations and schema ownership.
Standalone resources belong to their installation; the former fixed `local`
workspace table and duplicate device hash have been removed. External ownership
bindings must be added only with a real account-integration implementation.
Open-source runtime migrations own resource bindings,
enrollment status/version, grants, pairing operations, request work, management
inbox/application state, generic usage records/export bookkeeping and later relay
ownership. All regional Access tables and migrations are open-source and owned
by Access. The external account service owns its account and commerce migrations
in a separate database. Commerce rules produce effective entitlements for Access;
there is no private regional schema or shared cross-service database transaction.

Use PostgreSQL for durable work and deduplication; do not introduce Redis, Kafka
or a distributed database for this design. Do not put management operations into
the encrypted Host request table merely to preserve the old table count.

Retain `proto/v1/whalerein.proto` as the canonical open wire schema, extending it
for enrollment authority, pairing operation status, endpoint discovery and relay
epochs as required. Update the wire contract and all consumers together. Private
payment APIs stay private; there is no second hand-maintained schema for the same
open messages. Reject unsupported capabilities explicitly.

For the initial clean break, deploy a coherent new schema/protocol generation
and explicitly re-enroll test installations. Preserve command IDs, payload-digest
checks and uncertain-outcome semantics in the new design. Subsequent automated
rolling replacements require an explicit mixed-version compatibility window;
unsupported versions must not silently join a region.

## Deployment automation

Keep Compose as the standalone path. A managed deployment controller provisions
compute, networking, regional dependencies and certificates through infrastructure
automation; it is outside Access's conversation runtime. Access never receives
cloud-account credentials to create machines.

Adding an API instance means provision, inject scoped identity/configuration,
verify schema/protocol compatibility, pass role-specific readiness, then register
with the load balancer. Removing one drains HTTP admission and waits for bounded
in-flight work. Relay replacement also transfers ownership, rebuilds broker
observations, replays revocations and resolves/restarts incomplete exchanges.
Readiness is role-specific: an API can serve pending/status responses while relay
availability is reported separately. Liveness alone never admits traffic.

Creating a region additionally provisions PostgreSQL, Mosquitto, TLS endpoints,
backups and directory registration. Compute replacement does not migrate the
database or change a workspace's home region. Cross-region disaster recovery
requires fencing the old region, choosing acceptable replication loss, preserving
revocations and reconciling uncertain commands; it is not DNS failover alone.

## Implementation sequence and acceptance

| Slice | Work | Required evidence |
| --- | --- | --- |
| 1. Current: standalone foundation | Small single-process runtime, installation-scoped identity/enrollment model, versioned migrations, local administration and stable endpoints. | Initial setup, Host enrollment and invitation pairing complete with no central service or official trust root; isolation and migration checks pass. |
| 2. Current: standalone recovery | Single-process durable pairing operations, credential recovery, queryable status, disable-before-revoke and backup/restore procedures. | Restart or lost responses during pairing/activation recover without changing bound keys, reviving revoked access, invalidating unrelated grants or repeating a control. |
| 3. Later: external identity | Open-source generic client login/discovery and Access account integration, public management protocol, service identities, atomic management apply operations, minimal identity projections, scoped signed approvals and Core/Host automatic trust. The official account/commerce service remains private. | Stock Access works with an independently implemented compatible account service; same-account login pairs without manual confirmation; wrong region/key/issuer, stale policy and removed devices are rejected; standalone pairing works with no account service. |
| 4. Later: multi-instance hosting | API/relay deployment roles, readiness/draining, relay fencing, credential rotation and controlled rollout/rollback. | Multi-API admission, paused-old-relay takeover, broker reconnect and rolling replacement preserve revocation gates and outcome recovery. |
| 5. Later: regional automation | Independent stacks, infrastructure automation, directory routing, usage ingestion and recovery procedures. | Regions operate independently within policy validity; lost/duplicate/out-of-order updates reconcile without cross-region access. |

For each slice use staged affected-project validation and final package coverage
gates. Select integration groups by the changed protocol, persistence, transport
or recovery behavior under the [testing policy](../testing.md#choose-checks-for-a-change).
Production TLS, service certificate rotation, real broker administration and
infrastructure replacement require deployment acceptance. These are future gates,
not verification claims for this design document.

## Deferred decisions

Select the identity provider, payment provider and infrastructure automation tool
when implementing managed hosting. Specify supported signature algorithms, key
distribution, policy validity budgets and regional recovery targets in the
corresponding implementation contracts. Defer cross-region workspace movement,
global active-active account writes and relay sharding until required. None is a
prerequisite for account-authorized pairing or replaceable regional API instances.
