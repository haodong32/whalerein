# External account protocol v1

Status: **contract ratified, implementation pending.** The operator approved
decisions R1–R10 on 2026-11-14, so this document fixes the v1 public contract
that Access, Core and an independent account service implement. No endpoint of
this protocol is shipped yet: Standalone behavior described in
[the wire contract](../../proto/v1/WIRE.md) remains the only operating mode.

Ratified v1 shape, in one place:

- portable authorizations are Ed25519 signatures over the canonical text form
  with no JSON in the signed bytes (R1);
- credential recovery proves possession by replaying the exact committed
  FINISH within its recovery window, not by a fresh Noise exchange (R2);
- enrollment asserts both the Host signing and Noise keys and refuses
  contradictions (R3);
- re-enrollment revokes only the affected key's grants (R4);
- unknown fields are tolerated on management apply and rejected on endpoint
  messages (R5);
- recovery and validity windows carry no hard-coded numbers; task 21 owns them
  (R6);
- operation status is readable by an admin without a credential (R7);
- credentials are delivered inline exactly once (R8);
- v1 offers recovery and scoped replacement, never rotation (R9);
- the acceptance fixture imports only the protobuf bindings (R10).

Written by task 05 (Cloud workspace) additively: no existing statement in this
repository is changed by this file. Where this document and the current
Standalone wire contract differ, the wire contract wins for current
deployments.

## Scope

Covers scoped external authority, Host enrollment, discovery, automatic
pairing, operation status and recovery, versioned regional management and
revocation. It does not cover billing, shared workspace roles, endpoint key
rotation, multi-region transfer, or the choice of identity provider.

## Three credential classes

Keep them separate and never interchangeable:

| Class | Where it stays | What it may do |
| --- | --- | --- |
| User account login | Client shell and account service | Obtain an account-scoped authorization. Never a regional management credential. |
| Portable authorization | Carried by the endpoint, verified by Access | Authorize one enrollment, pairing, recovery or disable operation. Short-lived, purpose-bound, key-bound. |
| Regional service identity (mTLS) | Deployment trust material | Regional management changes and acknowledgments. Never authority to pair an arbitrary device. |

External account mode is configured explicitly (`mode: external`) with a service
identity and issuer keys. Unknown issuer, region, version or capability is
rejected; the runtime never falls back to Standalone.

## Operations (proposed)

| Operation | Purpose | Auth | Idempotency | Status reported |
| --- | --- | --- | --- | --- |
| Service metadata | Choose and verify a service | public TLS | none | n/a |
| Host discovery | List eligible Hosts and endpoints for one workspace | account login or paired Device | read-only | route expiry, contiguous journal position |
| Host enrollment | Bind an approved Host key to a workspace and region | portable authorization + Host proof | authorization and operation IDs | enrolled vs ready, revision |
| Device pairing claim | Pair with an account authorization instead of a fingerprint prompt | portable authorization + Noise peers | claim and operation IDs | waiting, committed pending activation, ready |
| Operation status | Read recorded progress | original endpoint proof | read-only | state, barrier, bindings, never a credential |
| Credential recovery | Recover a credential after a lost reply | original endpoint proof + current authority | operation ID | recovery window, scoped credential |
| Management apply | Apply one versioned resource change | regional mTLS identity | operation ID + payload digest | recorded, applied, enforced, revision, contiguous position |
| Standalone invitation pairing | Unchanged | admin bearer or Host signature | unchanged | unchanged |

## Invariants carried from the current implementation

These already hold in the pinned Standalone code and the proposed contract keeps
them:

- The authorization ID and operation ID bind one immutable intent; a changed
  payload is a conflict and the original record is preserved.
- The client generates the transport credential and binds its hash into the
  encrypted FINISH exchange; Access stores only the hash and never persists
  plaintext.
- Access commits the grant, the credential and the Host activation work
  together and reports ready only after the Host confirms activation.
- An operation ID alone conveys no authority; status reads return no credential.
- A revoked enrollment cannot use existing grants, cannot create new pairings,
  and cannot be revived by an older approval or a new login.
- Revocations are reported as pending until regional enforcement blocks affected
  access through HTTP admission, broker ACL and forwarding checks.
- Existing authorized traffic may continue only while regional authority
  remains valid; after it expires, protected forwarding and event delivery pause
  while safe teardown and revocation continue.

## New wire fields (proposed names)

Proposed names, not frozen: an authorization envelope
(version, issuer, audience, purpose, authorization ID, operation ID,
workspace/region scope, enrollment revision, key bindings, nonce, issued/expiry
times, signature); an operation status resource (state, barrier, immutable
bindings, grant generation, credential scope reference, deadlines, outcome); a
service metadata document (service identity, region, protocol version,
supported operations, capabilities, endpoints, trusted issuers with rotation);
and a management apply document (operation ID, resource version, workspace
journal position, intent, target keys, payload digest).

Exact encodings, error codes and numeric values belong to the ratification list
and to tasks 06, 08, 10, 15 and 22. Task 06 reconciles proof and credential
delivery fields with the Standalone recovery work before freezing them.

## Compatibility

Adding External account mode is additive for Standalone: the existing invitation
flow, reconnect, revocation and outcome recovery keep their current wire
semantics and configuration. Deployments that do not configure an account
service are unaffected. Publishing this contract does not create a second
maintained schema: `proto/v1/whalerein.proto` remains the canonical wire schema.

## Open ratification items

Encoding and signature profile; the scope of the "fresh proof" requirement for
recovery; enrollment key assertions; re-enrollment versus existing grants;
unknown-field handling on management apply; recovery and policy-window values
owned by the regional recovery targets; who may read operation status;
Host credential delivery; credential replacement and rotation scope; and the
independent-provider fixture boundary.
