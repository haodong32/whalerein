# External account protocol v1

Status: **contract ratified; Host enrollment operational in external mode;
device account mode not implemented.** The operator approved decisions R1–R10 on
2026-11-14, so this document fixes the v1 public contract that Access, Core and
an independent account service implement. Host enrollment and its status reads
exist and verify signatures and Host proof, but external Host enrollment is not
operationally usable and no account-authorized device pairing is implemented.
Standalone behavior described in [the wire contract](../../proto/v1/WIRE.md)
therefore remains the only usable operating mode.

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

## Current implementation status

Verified against the pinned code on 2026-11-16. This is a status record, not an
acceptance claim.

- **Host enrollment routes exist**: `POST /v1/enrollments/hosts`,
  `GET /v1/enrollments/hosts/{host_id}` and `GET /v1/operations/{operation_id}`
  are wired and verify issuer, audience, purpose, region, key window, signature,
  expiry and Host proof of possession. See `access/internal/external/` and
  `access/internal/httpapi/external_handlers.go`.
- **Enrollment is operational** in `mode: external`: the broker accepts the
  per-enrollment credential hash, `HostReady` advances the enrollment to ready
  and records the operation outcome, and monitored readiness is what the console
  projects. Managed authorizations now carry a monotonic `enrollmentRevision`
  allocated by the account service before the decision commits.
- **Status reads have a Host-proof path**: `GET /v1/hosts/{host_id}/enrollment`
  accepts the frozen `whalerein/v1/host-proof-v1` proof and returns no
  credential. Admin status reads remain for operators.
- **Removal and credential replacement exist**: `POST
  /v1/enrollments/hosts/{host_id}/disable` (admin bearer, Cloud only) durably
  disables, revokes grants and closes the live Host session; `POST
  /v1/enrollments/hosts/{host_id}/credential` replaces a lost credential once
  with a fresh `host_credential_recovery` authorization plus Host proof.
- **Device account pairing, directory, device registration, management apply and
  regional enforcement are absent.** Only purpose constants and fixture
  issuance exist for `device_pairing`/`device_recovery`.
- **Cloud mints Host enrollment authority only**; it has no device approval,
  scoped directory, membership enforcement or capacity model, and its regional
  client still uses a deployment admin bearer rather than a scoped service
  identity.
- **CLI account authentication and the read-only Host directory** use
  provider-issued bearer access tokens addressed to the Cloud CLI API.
  Credentials are scoped to a local profile and stored in its SecretStore.
  Device registration and account-authorized regional pairing remain separate
  from the CLI account session.

### Host ownership proof (v1)

Host-scoped reads and credential replacement use a signed JSON proof instead of
extending the canonical protobuf schema, so Standalone wire messages are
unchanged. The Host signs this newline-joined text with the signing key bound to
its current enrollment:

```text
whalerein/v1/host-proof-v1
|<purpose>
|<hostId>
|<subjectId>
|<issuedAtUnixMs>
|<nonce base64url>
```

`purpose` is `enrollment_status` (subject: operation id) or
`credential_replacement` (subject: authorization id). The proof travels in
`X-Whalerein-Host-Proof` as base64url JSON
`{"hostId","issuedAt","nonce","signature"}`. Issued-at is limited to ±5 minutes
and each nonce is single-use for 15 minutes. Neither endpoint ever returns a
credential except the one-time replacement response.

A regional service therefore exposes:

| Operation | Endpoint | Authentication |
| --- | --- | --- |
| Enrollment | `POST /v1/enrollments/hosts` | portable authorization + enrollment proof |
| Host status | `GET /v1/hosts/{host_id}/enrollment` | Host ownership proof |
| Credential replacement | `POST /v1/enrollments/hosts/{host_id}/credential` | Host ownership proof + `host_credential_recovery` authorization |
| Disable | `POST /v1/enrollments/hosts/{host_id}/disable` | regional management identity (Cloud only) |

The account service separately publishes its own versioned metadata document
(service identity, environment, protocol capabilities, approval origin, create
path and bounded display/locale limits). The Host validates that document,
the signed authorization and the assigned route before it sends any secret.

Treat the operations table below as the target contract, not current behavior.

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

Each pinned issuer key may carry a validity window as RFC 3339 timestamps:

```yaml
external:
  verification_keys:
    - id: "issuer-2026"
      public_key: "..."
      not_before: "2026-01-01T00:00:00Z"   # optional
      not_after: "2030-01-01T00:00:00Z"    # optional
```

An omitted bound is open on that side, so a key with neither bound accepts any
issue time and a rotation window needs only the bounds that matter. Bounds are
instants, not durations, and an inverted window is a configuration error.

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
