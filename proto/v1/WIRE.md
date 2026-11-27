# WhaleRein v1 wire contract

`proto/v1/whalerein.proto` is the only wire contract. HTTPS uses the proto3 JSON
encoding of these messages (lowerCamel fields, enum value names as written,
bytes base64). MQTT uses the binary protobuf encoding. There is no peer
OpenAPI or JSON Schema.

## Compatibility and version window

The application protocol version is carried as `protocol_version` on
`HostRequest`, `HostResult`, `CapabilitySet`, `ChannelHello`, `ChannelReady`,
`PairingHello`, `PairingHelloReply`, `PairingClaimRequest` and
`PairingClaimResponse`. This build supports the closed window
**1..=1** (`MIN_SUPPORTED_PROTOCOL_VERSION`..`MAX_SUPPORTED_PROTOCOL_VERSION`).
A zero or absent version is not a wildcard; it is an old peer that must
upgrade. `proto/v1/error_codes.json` and `proto/v1/compat/` are checked-in
golden artifacts and `tests/integration/drivers/protobuf/proto_compat.py`
fails a build that removes a field, reuses a tag number, changes a type or
JSON name, changes oneof membership, or reuses an enum number.

The MQTT relay carries a separate, required `relay_version` whose only
supported value is **5** (`RELAY_EPOCH_V5`). It is independent of the
application protocol version, which stays 1. `relay_version` appears on
`HostRequest`, `HostResult`, `EventEnvelope`, `HostReady`,
`ReceiveAdvertisement`, `RelayObjectReserveRequest`,
`RelayObjectFinishRequest`, `RequestStateNotice` and the relay
`application/json` error surface. A peer that does not advertise epoch 5 is
rejected explicitly with `protocol_version_incompatible`; there is no
unframed v2 acceptance, no fragment fallback and no mixed-epoch route.
Epochs 3 and 4 are retired: their numbers are reserved, they are never
accepted and no compatibility value or fallback exists for either.

This revision is the single revised conversation and transport contract inside
version 1. It intentionally replaces the earlier text-only live frame, flattened
tool/notice blocks, flattened send reply, cursor-only history read, compiled
transport ceilings, MQTT fragmentation and application receipts.
`WatchSession.interests` is required, `WatchReady.questions`,
`SessionState.questions`, `SessionUpdate.questions` and the `QuestionDescriptor`
and `QuestionStamp` payloads remain part of the only supported contract, and the
expanded content, attachment and budget families below are now mandatory where
their capability is advertised. There is no old-contract reader, compatibility
fallback, dual projection or hidden second version. A matching
Access/Host/Core/CLI/mobile artifact set is deployed together. The unchanged
numeric version is not evidence that arbitrary older version-1 builds remain
compatible; installed iOS builds on the prior contract cannot use the upgraded
endpoints and must be stopped rather than handed uninterpreted queued or signed
material.

The immutable protobuf baseline records this hard cut through the repository
compatibility path (`--update-baseline --rationale`) because fields and message
shapes are replaced, not added. The same cut removes `TransportFrame`, the
`WRMQTT2` prefix, fragment geometry and every receipt/acknowledgement topic and
replaces them with one inline-or-reference relay epoch. Epoch 5 hard-cuts epoch
4 and keeps the inline-or-reference control relay. It moves post-pairing Noise
establishment and session queries onto the MQTT 5 request/reply query lane,
routes previews directly from the Host as QoS 0, retires the Host `.../live`
relay topic, and replaces watch renewal with a read-only version check. The
durable command lane keeps its HTTPS admission, exact heartbeat and result
read. Keys, pins, grants,
Source sessions, pager keys and durable command recovery identities are
retained. Content positions,
cursors and receipt namespaces are bound to a projection identity: Core fences
old projected rows from readable history and atomically installs a rebuilt
snapshot over a fixed Source cut before it serves the new projection.

Readers follow one policy:

1. **Unknown field names and unknown binary tags are ignored.** Adding a field
   is additive. A new writer and an old reader stay compatible in both
   directions, and both proto3 JSON and protobuf implementations already skip
   unknown fields.
2. **Unknown enum values are rejected explicitly** with
   `protocol_version_incompatible`. A reader never falls back to the zero value
   and never renders a number as a name.
3. **A oneof must carry a known variant.** An absent payload variant is
   `invalid_argument`, not an empty success. A variant this build does not know
   rejects the frame, because its meaning cannot be recovered.
4. **A version mismatch names the side that must upgrade.** If the peer is
   older than the window, upgrade the peer; if it is newer, upgrade this
   component. `protocol_version_incompatible` is never retryable.
5. **proto3 JSON omits zero values.** Readers therefore must not distinguish an
   absent field from its default; absent means default. Explicit empty strings
   are emitted as `""` only where the schema marks a field `optional`.
6. **64-bit integers are decimal strings in JSON.** A JavaScript reader keeps an
   unsafe value (|value| > 2^53-1) as a string/BigInt instead of a `Number`, so
   `grant_generation`, `version` and time values never lose precision.
7. **Time values are Unix milliseconds** (`*_unix_ms` is milliseconds since the
   Unix epoch, UTC; `*_ms` without `unix` is a duration in milliseconds).
   Expiry comparisons are exact and never gain grace from clock skew. The only
   accepted clock window is the pairing signature window, two minutes in the
   past through one minute in the future; a peer outside it is rejected with
   actionable text and the `clock_skew` diagnostic label.

## Identifiers

All identifiers are opaque, random, and logged only through a local salted
diagnostic id.

| Field | Producer | Meaning |
|---|---|---|
| `request_id` | client | Correlates one `HostRequest` with its results. |
| `command_id` | client | Idempotency key for control commands. Empty for handshakes, queries and pairing. |
| `payload_digest` | client | `SHA-256` of the complete command body. For a referenced body it equals `body_reference.sha256`. Empty otherwise. |
| `channel_id` | client | Random secure-channel id, new for every handshake. |
| `trace_id` | client | Diagnostic correlation only. Never business, never idempotency. |
| `object_id` | Access | Opaque temporary ciphertext identity, exactly 32 lowercase hexadecimal characters. Never authority. |
| `owner_node_id` | Access | Opaque identity of the Access node that owns one ciphertext object. |
| `part_id` | Host/client | Attachment occurrence or part identity bound at reservation. Empty for non-part purposes. |
| History cursor | Source | Opaque, session/view-bound committed position. |
| Attempt reference | Source adapter | Live epoch plus Attempt ID, independent of Message identity. |
| `delivery_id` | Source adapter | Opaque acceptance identity; it does not establish execution or a committed Message. |

Identifiers are bounded so a peer cannot amplify logs or database rows, and no
identifier may contain the `|` AAD separator, which would make two different
AAD inputs ambiguous. The shared validator is `whalerein_proto::validate_id`.

| Field | Maximum encoded bytes | Allowed content |
|---|---|---|
| `request_id`, `command_id`, `channel_id`, `claim_id`, `approval_id`, `delivery_id`, `message_id`, `block_id`, `batch_id`, `item_id`, `call_id`, `transfer_id`, `projection_id` | 128 | printable ASCII, no `|`; `command_id` may be empty |
| `host_id`, `device_id` | 200 | printable ASCII, no `|` |
| `trace_id`, `watch_id`, `wait_id` | 64 | printable ASCII, no `|` |
| `session_id`, `attachment_ref`, `part_id` | 512 | UTF-8 without control characters or `|` |
| `object_id` | 32 | exactly 32 lowercase hexadecimal characters |
| `owner_node_id` | 128 | printable ASCII, no `|` |
| History cursor, `source_locator`, `continuation` | 4096 | UTF-8 without control characters or `|`; a cursor may be empty at the origin |

`attachment_ref` is an opaque Source occurrence identity, not a path or digest.
`source_locator` and `continuation` are opaque Core/Source evidence and are never
parsed by a client. A transfer is additionally bound to the authenticated
device, Host, session, channel and occurrence by the Host; the transfer handle
alone is never authority.

`object_id`, `owner_node_id`, a digest and a length locate an object only. They
never authorize a read, replace a grant, or extend an expiry. Access resolves
the owning node from `owner_node_id` and rechecks the authenticated principal
and stored scope before releasing any ciphertext.

A request whose identifier violates these bounds is `invalid_argument` and is
not forwarded. An identifier near the limit is never truncated on the wire.

Access preserves `command_id` and `payload_digest` after a request expires and
drops only the bodies. The same `command_id` with a different digest is
rejected and is never forwarded.

Once expiry cleanup has cleared a request's bodies, later Host results do not
restore them, even if a clock moves backwards. Status and independently verified
pairing confirmation may still advance. For a pre-upgrade row with unknown
retention history and an empty request body, Access conservatively withholds
the result body; callers must not infer business failure from that absence.

## Topics

```text
whalerein/v1/h/{host_id}/request
whalerein/v1/h/{host_id}/result
whalerein/v1/h/{host_id}/event
whalerein/v1/h/{host_id}/d/{device_id}/event
whalerein/v1/h/{host_id}/d/{device_id}/notice
whalerein/v1/h/{host_id}/d/{device_id}/ready
whalerein/v1/h/{host_id}/d/{device_id}/query
whalerein/v1/h/{host_id}/d/{device_id}/reply
whalerein/v1/h/{host_id}/d/{device_id}/live
```

Every topic carries exactly one complete message per MQTT PUBLISH. There is no
device acknowledgement topic, receipt topic or fragment topic.

The relay has three logical uses of the same authenticated broker connection.
The class is selected by topic, QoS and the operation that admitted the message,
never by a second connection or a second publish path:

| Class | Topics | QoS | Semantics |
|---|---|---|---|
| control | `.../request`, `.../result`, `.../event`, `.../d/{device_id}/event`, `.../d/{device_id}/notice`, `.../d/{device_id}/ready` | 1 | Ordered, acknowledged and durable intent. Every control message is submitted in order and retried from the same complete-message identity. |
| query | `.../d/{device_id}/query`, `.../d/{device_id}/reply`, and the existing `.../request` / `.../result` topics while a query is admitted | 1 | Deadline-bound MQTT 5 request/reply for post-pairing Noise and session queries. Access holds one transient waiter; there is no durable request row, outbox or application acknowledgement. |
| live | `.../d/{device_id}/live` | 0 | Fire-and-forget derived state published by the Host directly to the granted Device. A live preview may drop, coalesce or be repaired; it never enters an outbox or receipt, never uses the HTTPS reference carrier, and the final committed message supersedes it. |

Live is never used for control or query intent. Control and query are never
carried on a live topic. All classes use the same authenticated broker
connection and the same sealed body encoding. The former Host `.../live` topic
is retired in epoch 5 and no longer appears in the topic list.

| Topic | Publisher | Subscriber | Payload |
|---|---|---|---|
| `.../request` | Access | that Host | `HostRequest`, including `REQUEST_KIND_ACCESS_READY` and admitted queries |
| `.../result` | Host | Access | `HostResult`, including `HostReady` and query results |
| `.../event` | Host | Access | `EventEnvelope` (control class) |
| `.../d/{device_id}/event` | Access | that Device | `EventEnvelope` (control class) |
| `.../d/{device_id}/notice` | Access | that Device | `RequestStateNotice` |
| `.../d/{device_id}/ready` | that Device | Access | `ReceiveAdvertisement` |
| `.../d/{device_id}/query` | that Device | Access | `HostRequest` (`HANDSHAKE` or `QUERY`) on the query lane, QoS 1 |
| `.../d/{device_id}/reply` | Access | that Device | `HostResult` for the admitted query, QoS 1 |
| `.../d/{device_id}/live` | Host | that Device | `EventEnvelope` (live class, QoS 0) |

Access publishes only `request`, the Device control topics and the Device reply
topic; a Host publishes only its own `result`, `event` and direct Device `live`
topics; a Device publishes only its own `ready` and `query` topics. The Host
subscribes only to its own `request`. A Device subscribes only to its own event,
notice, reply and live topics and publishes only to its own ready and query
topics. Access publishes a control event to `.../d/{device_id}/event` using only
the authenticated outer `device_id`. It does not subscribe to or republish
previews. Cross-host wildcard subscriptions are denied by broker ACL. The Host
publishes a preview directly to the granted Device's
`.../d/{device_id}/live`; the Broker policy re-checks the publisher's Host
identity, the exact Host/Device topic and the current grant before delivery.

Live publication never waits for an acknowledgement and never uses the ordered
control queue. A live preview that is lost, duplicated or coalesced leaves
derived display state incomplete; it does not affect control delivery,
authority or committed content. The final committed message travels on the
control path (or is read over authoritative HTTPS) and repairs or replaces the
preview.

`.../d/{device_id}/ready` is the Device receive-advertisement path. The Device
publishes its own `ReceiveAdvertisement` after each successful broker connect
and whenever its effective budget changes. Access registers it against the
broker-derived connection generation for that authenticated Device connection.
An old generation never refreshes a newer one, and Access never treats a
Device-supplied value as authority to raise a limit. The Host learns the Device
receive budget through the authenticated Noise `ChannelHello.receive`, and
learns Access's own Host-to-Access budget through the Access
`REQUEST_KIND_ACCESS_READY` reply described below.

Client IDs and usernames:

```text
access        username access, password = access broker credential
host          client id whalerein-host-{host_id}, username host:{host_id}
device        client id whalerein-device-{device_id}, username device:{device_id}
```

Hosts authenticate with the deployment provisioning token from configuration.
External Host enrollment issues a broker credential that this build does not
yet accept; the deployment token remains the only working Host broker
credential. A host is inserted into `hosts` on its first successful broker
authentication.
Devices authenticate with the transport token issued at pairing. ACLs are
computed from the live grants row; a revoked grant denies both HTTP and MQTT.

## Pairing

`POST /v1/pairing/invitations` has two callers. A host sends no bearer token.
It signs `whalerein/v1/host-auth|invitation|{host_id}||{signed_at_unix_ms}`
with Ed25519, using its Noise X25519 private key as the seed, and puts
`signedAtUnixMs` and `signature` on `PairingInvitationRequest`. Access checks
that signature against the Ed25519 public key from the host's latest
`HostReady` (`authorization_public_key`) on the already-authenticated MQTT
session. The first key is kept. An administrator sends the configured admin
bearer and may name any existing host; the signature is ignored. A bearer
that is present but invalid is rejected and does not fall through to the host
signature. On the host path, a missing host or a missing key is unauthorized.
The signature time must fall between two minutes in the past and one minute
in the future. Reuse inside that window is unauthorized. An Access restart
can accept a signature that is still inside the window. The database has no
user table. The invitation token is random (32 bytes, base64url), stored only
as `SHA-256(token)`, bound to one `host_id`, expires after a few minutes, and
is rejected after a bounded number of failed claims.

A host revokes one of its own grants with
`POST /v1/hosts/{host_id}/devices/{device_id}/revocation` and
`HostGrantRevocation`. The signed message is
`whalerein/v1/host-auth|revoke|{host_id}|{device_id}|{signed_at_unix_ms}`.
That route revokes the one grant and disconnects the device. It does not use
the admin bearer and publishes an incarnation-bound notice to an online host. Global
`DELETE /v1/devices/{device_id}` stays the administrator and device
self-revoke path.

`whalerein://pair?v=1&access=<url-encoded public URL>&host=<host_id>&token=<token>&mqtt=<url-encoded MQTT URL>`

The optional `mqtt` parameter is the external MQTT 5 TLS endpoint advertised by
Access (for example `mqtts://access.example:443` behind the official ALPN
entry, or `mqtts://host:8883` for a self-hosted instance). If it is absent the
client derives `mqtts` on the Access URL host with the Access port and ALPN
`mqtt`; self-hosted deployments should always advertise it explicitly.

The client generates its device id and X25519 identity key once, then runs
Noise XX with prologue:

```text
whalerein/v1/pairing|{host_id}|{token_hash_hex}|{claim_id}
```

### Step 1: HELLO

`POST /v1/pairing/claims` with `PairingClaimRequest{phase=HELLO, claim_id,
device_id, noise_message=msg1, protocol_version}`. Access verifies the token,
host binding, expiry and failure budget, requires the host to be online, and
forwards a `HostRequest{kind=PAIRING_CLAIM, body=PairingClaim{HELLO,
token_hash, noise_message}}` to the host. The host consumes msg1, returns
`PairingClaimResult{noise_message=msg2, host_public_key,
host_fingerprint}`. HELLO is relayed with legacy framing so an unknown peer can
bootstrap; Access then requires the host to advertise pairing support before
it will create a grant. Access persists the public reply and request digest in
`pairing_operations`; no device or grant is activated yet.

The client completes the XX read of msg2, obtains the host static public key
from Noise (not from Access), computes the fingerprint, and asks the user to
confirm. A mismatch or cancellation ends the flow. No `grants` row exists and
the host has not saved the peer key.

### Step 2: FINISH

Core creates a random 32-byte base64url `device_token` and includes
`SHA-256(UTF-8(device_token))` in encrypted `PairingFinish.device_auth_hash`.
Before sending message 3 it persists the exact confirmed request, device key
binding, endpoint and confirmed Host fingerprint in SecretStore.

`POST /v1/pairing/claims` sends `phase=FINISH`, `confirmed=true`, message 3 and
`device_token`. Access forwards only the hash in `PairingClaim.device_auth_hash`.
The Host completes XX, checks the credential hash against the encrypted FINISH,
and returns the authenticated key and hash. It durably stages the peer and
FINISH replay binding without activating the peer. Changed FINISH ciphertext,
invitation hash or credential hash cannot reuse that staged receipt.

After AUTHORIZE confirms the staged peer, Access atomically consumes the
invitation, writes identity/credential/grant state, saves the request digest and
credential-free completion response, and queues generation-bound COMMIT. Only
confirmed COMMIT completes pairing. FINISH responses echo the credential already
held by the client; Access never persists its plaintext. A failed pre-commit
attempt rolls back rather than revoking an older valid grant.

Access commits the grant before it forwards COMMIT, and it persists the exact
FINISH digest with the grant in that same transaction. The sequence is
therefore: Access transaction commits, then COMMIT reaches the Host, then the
Host ACK is stored. A lost FINISH response, a lost COMMIT ACK, or an Access
restart between those steps leaves an outcome that `operation recover <id>` can still
classify as committed, unconfirmed, or unavailable.

An exact FINISH retry after restart recovers the same receipt and COMMIT, subject
to operation expiry, current credential validity and the exact active grant
generation. The request digest covers every FINISH field, including the supplied
credential. A consumed invitation alone is insufficient to recover or create a
new grant. Revocation and grant replacement invalidate old completion receipts.
A FINISH that fails before the commit consumes the invitation, so the same
Device can only retry with a new invitation; nothing about the earlier Device's
grants has changed.
Use `operation list` to find the retained pairing ID. The client command
`operation recover <id>` reloads the request from SecretStore; it never
reconstructs a credential from the operation ID. A lost SecretStore requires
explicit operator revocation and a fresh enrollment.

Concurrent pairing for the same invitation is serialized in Access. There is no
Host-side retry ticket for an authenticated FINISH: the Host refuses to decrypt
ciphertext again once its pending handshake is consumed, so a client that reuses
the original Noise message without Access replaying the recorded result receives
a Host rejection rather than a deduplicated success.

This credential-binding requirement is a coordinated protocol cutover: upgrade
Access, Core/CLI and Host together. Missing or mismatched hashes fail pairing.


## Secure channel

After pairing, both sides run Noise XX with prologue:

```text
whalerein/v1/channel|{host_id}|{device_id}|{channel_id}
```

A channel handshake is two `HostRequest{kind=HANDSHAKE}` calls on the MQTT query
lane, after the authorized subscription and readiness exist on the admitted
Broker connection:

1. `body = msg1`. Host replies `HostResult{status=succeeded,
   result_body=msg2}` after checking the device has an active grant and has
   persisted peer key and no revocation replay is pending.
2. `body = msg3`. Host returns `HostResult{status=succeeded, result_body empty}`.
   Both sides now derive keys.

Noise payloads are `ChannelHello` / `ChannelReady`; they carry the negotiated
application protocol version, capabilities and the endpoint's
`ReceiveAdvertisement` (including its own `relay_version`, which must be 5).
`ChannelHello.receive` is the Device receive budget the Host uses for preview
splitting; `ChannelReady.receive` is the Host budget the Device uses. A
version or relay-epoch mismatch rejects the handshake with
`protocol_version_incompatible` before any command executes.

Key schedule:

```text
handshake_hash = Noise XX handshake hash after msg3
root           = HKDF-SHA256(ikm=handshake_hash, salt=prologue, info="whalerein/v1/channel-keys", L=64)
c2h            = root[0..32]   device -> host
h2c            = root[32..64]  host -> device
```

`channel_id` is the client-chosen random secure-channel id from the handshake
request. Both sides bind it through the Noise prologue, so a successful
handshake authenticates it; no second derived id is used. Every frame and event
for this channel carries that same `channel_id`.

Application messages use `EncryptedFrame{channel_id, nonce, ciphertext}` where
`nonce` is 12 random bytes and `ciphertext` is ChaCha20-Poly1305. AAD is:

```text
request: whalerein/v1/request|{request_id}|{host_id}|{device_id}|{kind}|{command_id}|{expires_at_unix_ms}
event:   whalerein/v1/event|{host_id}|{device_id}|{channel_id}|{created_at_unix_ms}
```

A Host can keep multiple channels for the same Device. Each channel has its
own keys and observation state. A new handshake leaves other channels active.
The Host bounds live channels per Device and globally and reclaims channels
after an idle interval. An authenticated in-flight request retains its channel
through the response. Revoking a grant closes all channels for that grant.
A wrong or closed channel, failed MAC or failed AAD rejects the message without
side effects. Replays are handled by `request_id`/`command_id` dedupe, typed
history intervals and live Attempt sequences. None is a nonce.

## Requests and results

`POST /v1/hosts/{host_id}/requests` accepts a `HostRequest` from an authorized
device for the durable command lane. Every request validates the application
`protocol_version` (1) and `relay_version` (5). A non-matching relay epoch is
rejected with `protocol_version_incompatible` before admission; Access never
accepts a v2 frame in place of a `HostRequest`. The retired post-pairing HTTPS
handshake and session-query path is not an alternative ingress: `HANDSHAKE` and
`QUERY` on this route fail before forwarding. Access:

1. authenticates the device transport token and an active grant;
2. checks `expires_at`, `protocol_version` and `relay_version`;
3. validates exactly one body representation. `HANDSHAKE`, `PAIRING_CLAIM` and
   internal Access notices must carry inline `body` and no `body_reference`.
   `QUERY` and `COMMAND` carry exactly one of `body` and `body_reference`; both
   or neither is `invalid_argument`. When `body_reference` is set,
   `payload_digest` must equal its `sha256`;
4. for a command, looks up `(host_id, device_id, command_id)`:
   same digest returns the existing row, different digest returns
   `payload_digest_mismatch` without forwarding;
5. if the host has no broker connection, writes `host_offline` with
   `forward_state=none` and returns;
6. otherwise inserts `received`, `forward_state=pending`, publishes after the
   transaction commits, and records `published_once`.

The representation is per hop, not per message. Access may receive an inline
`body` and publish the same complete ciphertext as a `body_reference` on the
Host hop, or receive a `body_reference` and reuse it. It may also stage an
inline body under the original `request_id`, `command_id`, digest and expiry
before publishing the reference. The same rule applies in reverse. The Host
always validates and dispatches the same complete message; it never sees a
fragment and cannot tell which hop changed representation except from the
carrier fields.

`GET /v1/requests/{request_id}` returns the status copy for a durable command.
There is no HTTP route for sessions, messages, interrupts or approvals, and an
admitted query has no durable row to read. When the complete result body
was stored rather than inlined, `RequestState.result_reference` locates it.

`HostResult` from the host conditionally updates the same command row. It
carries the same required `relay_version=5`. Allowed transitions:

```text
received      -> host_offline, rejected, host_accepted, succeeded, failed, unknown
host_accepted -> succeeded, failed, unknown
unknown       -> succeeded, failed
```

`host_offline`, `rejected`, `succeeded` and `failed` are final. For control
commands the host first publishes `host_accepted`, then a terminal result.
`received` never means the host accepted. `host_accepted` never means DSH
finished. `unknown` never means success.

`HostResult` carries exactly one of `result_body` and `result_reference` when a
body exists. `HostReady`, pairing results and internal control results are
small bootstrap bodies and stay inline. The Host and Access publish a complete
result inline when it fits the receiver's advertised inline bound and the hop
packet, and otherwise stage the exact ciphertext under `PAYLOAD_PURPOSE_RESULT`
and publish a `result_reference` with the same request and command identity. A
query result is therefore admitted whole and never fails or shrinks for a
transport-size reason; `payload_too_large` means the complete message cannot be
represented even by reference (it exceeds the logical envelope budget). A
packet-sized terminal status may carry neither representation when the complete
body could not be admitted; the recorded outcome, error code and command
identity are unchanged, and the client reports confirmed completion with scoped
recovery.

The device transport is Host-scoped, not session-scoped. The device
subscribes to one topic per Host (`whalerein/v1/h/<host>/d/<device>/event`);
every watch reads that one subscription and filters by session. A Host
connection is therefore opened by the client, may outlive any watch, and
reports `connecting`, `connected`, `reconnecting` or a terminal `offline` with
a code. A session ending never implies the Host is offline.

Durable command results are read only by `request_id`. A query result instead
returns on the correlated `.../d/{device_id}/reply` topic. The result body stays
encrypted for the channel that produced it; after a reconnect the client reuses
the same `command_id` (host dedupe) or queries `GetCommandResult` to obtain the
terminal payload over the new channel.

### Access receive advertisement

`HostReady` (Host to Access) carries the Host receive budget for the
Access-to-Host `request` hop. After Access registers a valid epoch-5
`HostReady`, it publishes one `HostRequest{kind=REQUEST_KIND_ACCESS_READY}` on
`.../request` whose inline `body` is Access's own `ReceiveAdvertisement`. The
Host records it as the Access receive budget for the Host-to-Access
`result`/`event` hop. Access republishes this control notice when its effective
budget or connection generation changes. The Host does not answer it and does
not treat it as a business request. This is how the Host obtains the middle-hop
budget; `ChannelHello.receive` supplies the Device budget used for preview
splitting. A Device cannot submit `REQUEST_KIND_ACCESS_READY`.

### Host route attachment

A managed Host announces its serving node in the `HostCredential` it receives
at enrollment (`node_id`, `incarnation`). It keeps a durable monotonic
`route_generation`, increments it before each runtime start, and adds a proof to
`HostReady`: `route_generation`, `route_challenge`, `route_signed_at_unix_ms`,
`route_signature`, `route_service`, `route_environment`,
`route_signing_public_key` and `route_noise_public_key`.

The signature is Ed25519 over the UTF-8 canonical string
`whalerein/v1/host-route|attach|service|environment|host_id|node_id|incarnation|challenge|generation|base64url(noise_public_key)|signed_at_unix_ms`.
Access verifies the announced keys against the Host enrollment, fills its own
node identity and forwards `HostRouteAttach` to Central. Central re-verifies the
signature and owns the route decision; Access never invents or caches one. A
`HostReady` without route fields is still accepted for older Hosts but creates
no managed route.

### Result notices

Access commits durable notice intent in the same transaction as the
authoritative outcome, before any MQTT publication. A publication failure is
retried from that intent; a lost notice never loses the recorded fact. A
reference is eligible for delivery only after its complete matching ciphertext
is readable, and a notice never advertises an unfinished upload.

After an outcome and its durable notice intent commit, Access publishes a
`RequestStateNotice` on `.../d/{device_id}/notice`. The notice is always a
single packet-sized metadata message. It carries `request_id`, `command_id`,
status, update time and, only after the complete matching ciphertext is
readable, `result_reference`. It carries no plaintext content, preview text or
business payload. Duplicate notices are harmless. A receiver that missed one
reconciles through `GET /v1/requests/{request_id}` and the original command
identity, and performs one final authoritative lookup before an authorized,
unconfirmed wait expires.

The same rules apply to control observation: Access forwards a validated
`EventEnvelope` or its reference only after the durable event copy exists, and
a reconnecting receiver reconciles from its original watch and history cursor.
A live preview on the live topic has no durable event copy, no reference and no
replay; losing it leaves derived display state incomplete until the committed
control message or an authoritative read repairs it. A stale notice never
reopens expired content, a revoked scope or a closed execution. No new plaintext
content is added to MQTT; previews remain sealed, notices carry metadata only,
and complete bodies travel inline when they fit or through the HTTPS directory.

## Query lane

Post-pairing Noise establishment and session queries use one MQTT 5
request/reply lane. It reuses the existing Host `request`/`result` topics and
adds one Device ingress and one Device reply topic. The command lane is
unchanged: durable command admission, exact heartbeat and authoritative result
read stay on HTTPS.

1. **Device to Access.** The Device publishes one binary `HostRequest` with
   `kind=QUERY` or `kind=HANDSHAKE` and a non-zero `expires_at_unix_ms` on
   `whalerein/v1/h/{host}/d/{device}/query` at QoS 1. A QUERY carries exactly
   one of `body` and `body_reference`; a HANDSHAKE carries inline `body` only.
   MQTT 5 properties:
   Correlation Data is the UTF-8 `request_id`; Response Topic is exactly
   `whalerein/v1/h/{host}/d/{device}/reply`; Message Expiry Interval is the
   positive ceiling of the remaining deadline in whole seconds, or the
   publication is skipped. The Broker stamps `whalerein-connection` on the
   external ingress.
2. **Access admission.** Access rejects a wrong relay epoch, a topic/body scope
   mismatch, a missing or foreign Response Topic, a missing or mismatched
   Correlation Data, an elapsed deadline, or a stale connection/grant/enrollment
   scope. It keeps one transient waiter keyed by
   `(host, device, connection_ref, request_id)` with the immutable admitted
   scope and the original deadline. It creates no durable request row and no
   body object for an ordinary query.
3. **Access to Host.** Access publishes the same `HostRequest` on the existing
   `whalerein/v1/h/{host}/request` at QoS 1 with `connection_ref`,
   `grant_generation` and `enrollment_generation` filled from the admitted
   scope. Properties mirror step 1 with Response Topic
   `whalerein/v1/h/{host}/result` and the remaining Message Expiry Interval.
4. **Host to Access.** The Host publishes `HostResult` with the same
   host/device/request identity on the existing
   `whalerein/v1/h/{host}/result` at QoS 1. Correlation Data is the request id,
   there is no Response Topic, and Message Expiry Interval carries the remaining
   deadline. A result whose deadline elapsed before publication is dropped.
5. **Access reply.** If the result carries `result_reference`, Access verifies
   that the object is ready, still bound to the original waiter scope and the
   current grant, and marks the reference issued before publishing it. Access
   publishes the `HostResult` on
   `whalerein/v1/h/{host}/d/{device}/reply` at QoS 1 with Correlation Data equal
   to the request id, `whalerein-target=<waiter connection_ref>`, and the
   remaining Message Expiry Interval, then removes the waiter.
6. **Core waiter registry.** Core keeps a per-connection waiter registry keyed
   by request id and connection generation. It subscribes to `.../reply` before
   publishing a query. A reply for another request, device, Host or connection
   generation never completes a waiter. Reconnect fails admittees with a
   retryable transport error; a safe read is reissued within the remaining
   caller deadline.
7. **Command boundary.** `kind=COMMAND` is not admitted by the query ingress.
   Command submission, `POST /v1/requests/{request_id}/heartbeat` and the
   authoritative command result read keep their HTTPS contract.

Access restart drops transient waiters. A Device then sees no reply and may
retry the safe read. An object that was already issued remains fetchable within
its original deadline and current grant.

### Query-lane MQTT 5 properties

| Property | Set by | Rule |
|---|---|---|
| `whalerein-connection` | Broker | Reserved User Property stamped on every external ingress PUBLISH as `<broker-epoch>:<connection-id>`. A client-supplied copy is rejected. Access reads the stamp at admission and fills `HostRequest.connection_ref`; a trusted Access publication never forwards a Device stamp. |
| `whalerein-target` | Access | Reserved User Property naming the admitted Device connection reference. Access sets it on a query reply. The Broker checks it against the receiving connection's current `<broker-epoch>:<connection-id>` at final delivery, including queued bytes. A client-supplied copy is rejected. |
| Correlation Data | Device / Host / Access | UTF-8 `request_id` on both request and reply. It must match the admitted waiter. |
| Response Topic | Device / Access | Must equal the fixed topic of the lane: `.../d/{device_id}/reply` on the Device request and `.../result` on the Access-to-Host hop. A missing or different value is rejected. |
| Message Expiry Interval | Device / Access / Host | Required on every deadline-bound query-lane PUBLISH in both directions. Positive whole seconds, rounded up, capped by any shorter upstream value, and omitted with no publication when the remaining time has elapsed. |

The pinned Broker owns Message Expiry Interval as its own MQTT 5 behavior. It
stores the property separately and consumes it before plugin callbacks, so the
plugin cannot read or enforce it at ingress; the Broker discards a queued copy
whose delivery has not begun by then. Access, Host and Core therefore enforce
the original application deadline from `HostRequest.expires_at_unix_ms`
independently, including packets already in flight and the subsecond rounding
window. A shorter Broker expiry never extends the application deadline.

`HostRequest.transport.enrollment_generation` records the managed enrollment
generation captured at submission. The epoch-5 top-level `HostRequest` fields
`connection_ref = 17`, `grant_generation = 18` and `enrollment_generation = 19`
carry the admitted query scope to the Host. They are Access-filled; a Device
cannot set them because a Device never publishes on the Access-to-Host lane.
The Host binds observation ownership, channel admission and reply affinity to
that exact scope. A re-added Host or re-paired client with a newer generation
cannot attach to an older query.

## Pairing and revocation replay

A device revocation is one access transaction writing `grants.revoked_at`,
after which ACLs computed from the grants table deny HTTP, MQTT publish and
MQTT subscribe. Access then disconnects the device's broker client and
publishes a `RevocationNotice` to the host. `replayed_at` is written only
after the host acknowledges. If the host is offline or Access restarts first,
the pending notice stays `replayed_at IS NULL` and is replayed on the next host
readiness signal. The host sets `revocations_pending=true` when it connects and
rejects every new channel handshake until it has received a
`RevocationNotice{final=true}` (an empty final batch when nothing is pending).

Each `(host_id, device_id)` grant has an Access-owned monotonic generation.
Pairing COMMIT carries `PairingClaim.grant_generation`; the direct notice and
its durable outbox record use the same committed value. Revocation notices carry
`grant_generations[device_id]` independently for each listed device. An omitted
entry means legacy generation zero, never a wildcard. `revoked_at_unix_ms` is
diagnostic timing, not authorization ordering.

Hosts retain the greatest revoked generation per device across repairs. A notice
for an older generation acknowledges processing but preserves newer peer/channel
state. A COMMIT at or below that watermark, or older than an installed peer, is
rejected. Access marks replay only when the grant generation still matches the
ACKed notice. Access and Host must be upgraded together for these guarantees.

The host never calls Access to delete an ACL and never persists an independent
authorization list. Access is always authoritative on conflict.

## External account mode

Standalone mode never sends these messages. When an operator configures the
service identity, issuer and pinned verification keys for external account
mode, Access admits an enrollment only after two independent checks:

1. **The authorization.** The issuer signs the canonical text form, and no
   JSON is part of the signed bytes:

   ```text
   whalerein/v1/ext-auth-v1
   |{auth_version}|{authorization_id}|{issuer}|{audience}|{purpose}
   |{workspace_id}|{region_id}|{operation_id}|{enrollment_revision}
   |{precondition}|{expected_revision}
   |{host_id}|{sha256(host_pubkeys, sorted, length-prefixed)}
   |{device_id}|{sha256(device_pubkeys, sorted, length-prefixed)}
   |{issued_at_unix_ms}|{expires_at_unix_ms}|{base64url(nonce)}
   ```

   Field order is fixed. Empty values stay empty. Key sets hash as sorted,
   length-prefixed values, so ordering never changes the signature. Access
   checks the issuer, the audience against its own service identity, the
   purpose, the region, the expiry, and that the signing key was valid at
   `issued_at_unix_ms`. An untrusted issuer, an unknown key id, a failed
   signature or a key outside its window refuses the request.

2. **The Host's proof of possession.** The Host signs the enrollment proof
   input with the approved Host **signing** key:

   ```text
   whalerein/v1/enrollment|{authorization_id}|{base64url(nonce)}|{host_id}|{base64url(host_signing_public_key)}|{operation_id}
   ```

   Access requires the asserted signing and Noise keys to be exactly the
   approved pair, and it verifies the proof against that signing key. A
   changed key, a stale nonce or a proof lifted from another operation is
   refused.

`POST /v1/enrollments/hosts` commits the operation row, the enrollment row and
a hashed broker credential in one transaction, so a lost reply is recoverable:
the same authorization with the same payload returns the recorded status and
**no** second credential, while a changed payload reports
`operation_conflict`. The broker credential is delivered only by the call that
commits the enrollment, and the enrollment stays `enrollment_pending` until an
authenticated connection is observed.

`GET /v1/enrollments/hosts/{host_id}` and `GET /v1/operations/{operation_id}`
report state, bindings and revisions. Neither ever returns a credential.

The admin enrollment response includes `WhaleRein-Host-Presence` with
`online`, `offline`, or `unavailable`. This is the Broker-derived observation,
repaired by ordered lifecycle events and a sequence-bounded snapshot. It is not
derived from a Device Will, and a device-only network loss does not change the
Host directory presence. An older Access without this header leaves live
presence unknown.

`GET /v1/enrollments/hosts/{host_id}/clients` requires an admin bearer and the
`workspaceId` and `enrollmentRevision` query parameters. It returns
`{"presence":"known","devices":[{"deviceId":"device-1","connected":true}]}`
for confirmed grants in that exact enrollment. Access obtains the active
Device IDs from an internal `REQUEST_KIND_HOST_CLIENTS` request and intersects
them with the confirmed grants. The Host reports Devices with a live Session
observation lease. An offline Host has no connected Devices. Missing Host
observation returns `presence: unavailable` with the confirmed grants.
Device request submission cannot invoke the internal request kind.

### Typed send follow-up

The encrypted `ReadMessageReply` query uses the session ID and the submitted
message identity from the authenticated submission receipt.
`MessageReply.submittedMessageId` echoes that scope. The result is a typed state,
never flattened content:

| State | Payload | Meaning |
|---|---|---|
| `MESSAGE_REPLY_PENDING` | none | No association or settlement evidence yet. |
| `MESSAGE_REPLY_COMMITTED` | `committed` | The Source associated the exact submission with a canonical committed message. |
| `MESSAGE_REPLY_INPUT_REQUIRED` | `pending_input` | The owning active work waits for one or more evidenced `InputKind` values. |
| `MESSAGE_REPLY_UNCERTAIN` | none | Evidence cannot bind the submission to one reply. |

`CommittedMessageRef` carries the Host-qualified `SessionKey`, canonical
`message_id`, opaque `source_locator` and `projection_id`. Resolve it through the
exact `HistoryRead` selection; the reply body stays in history. `committed` is
present exactly for `COMMITTED`, and `pending_input` is present exactly for
`INPUT_REQUIRED`. Pending and uncertain results carry neither. The kinds use the
same `InputKind` vocabulary as `WaitSession`; they are informational and never
answer an approval or question. An input claim belongs to the same active
submitted operation, and a late reply from another send cannot satisfy it. A
query never admits a new message and never resubmits one.

## HTTP surface

```http
POST   /v1/pairing/invitations        admin bearer or host signature
POST   /v1/pairing/claims              token in body
GET    /v1/hosts                       device bearer token
POST   /v1/hosts/{host_id}/requests     device bearer token
GET    /v1/hosts/{host_id}/authority    device bearer token
GET    /v1/requests/{request_id}        device bearer token
POST   /v1/requests/{request_id}/heartbeat  device bearer token
DELETE /v1/devices/{device_id}          device bearer token (self) or admin
POST   /v1/hosts/{host_id}/devices/{device_id}/revocation  host signature
POST   /v1/enrollments/hosts           portable authorization + Host proof
POST   /v1/enrollments/devices/pairing  Cloud authorization and Noise proof
GET    /v1/enrollments/hosts/{host_id}/devices/{device_id}  admin bearer
POST   /v1/enrollments/hosts/{host_id}/devices/{device_id}/revocation  admin bearer
GET    /v1/enrollments/hosts/{host_id} admin bearer (device/Host proof pending)
GET    /v1/enrollments/hosts/{host_id}/clients  admin bearer
GET    /v1/operations/{operation_id}   admin bearer (original-endpoint proof pending)
POST   /v1/relay/objects               Host authority or Device bearer token
PUT    /v1/relay/objects/{object_id}   Host authority or Device bearer token
POST   /v1/relay/objects/{object_id}/finish  Host authority or Device bearer token
GET    /v1/relay/objects/{object_id}   Host authority or Device bearer token
DELETE /v1/relay/objects/{object_id}   Host authority or Device bearer token
GET    /healthz
GET    /readyz
GET    /metrics
```

### Temporary ciphertext directory

`/v1/relay/objects` is the Access-owned temporary ciphertext directory. The
routes use a node-scoped owner path under the configured Access origin: the
caller resolves `owner_node_id` to that node's configured base path (for
example an ingress prefix or node host). A single node serves its own path
directly. No route accepts a filename, MIME type or content metadata outside
the sealed ciphertext.

| Route | Request | Success | Purpose |
|---|---|---|---|
| `POST /v1/relay/objects` | `RelayObjectReserveRequest` | `RelayObjectReservation` | Reserve exact bytes, scope and expiry; return object id, owner and upload path |
| `PUT /v1/relay/objects/{object_id}` | raw ciphertext, exact `Content-Length` | `204` | Upload exactly the reserved length |
| `POST /v1/relay/objects/{object_id}/finish` | `RelayObjectFinishRequest` | `RelayObject` | Publish the object only after complete validated upload |
| `GET /v1/relay/objects/{object_id}` | no body | raw ciphertext stream | Fetch the complete object |
| `DELETE /v1/relay/objects/{object_id}` | no body | `RelayObjectDeleteResult` | Bounded cancel and release |

Every request carries the relay epoch. `POST`/`finish` carry it in the JSON
body; `PUT`/`GET`/`DELETE` require `WhaleRein-Relay-Epoch: 5`. A missing or
different value is `protocol_version_incompatible`.

Authentication is one of:

- **Host authority.** The Host uses the Ed25519 signing key registered on its
  authenticated `HostReady`. `POST`/`finish` carry `signed_at_unix_ms` and
  `signature` in the body. `PUT`, `GET` and `DELETE` carry
  `WhaleRein-Host-Signed-At-Unix-Ms` and `WhaleRein-Host-Signature`
  (base64url). The signed text forms are fixed:

  ```text
  whalerein/v1/relay|reserve|{relay_version}|{host_id}|{device_id}|{session_id}|{channel_id}|{purpose}|{part_id}|{ciphertext_bytes}|{hex(sha256)}|{expires_at_unix_ms}|{signed_at_unix_ms}
  whalerein/v1/relay|upload|{relay_version}|{object_id}|{ciphertext_bytes}|{hex(sha256)}|{signed_at_unix_ms}
  whalerein/v1/relay|finish|{relay_version}|{object_id}|{ciphertext_bytes}|{hex(sha256)}|{signed_at_unix_ms}
  whalerein/v1/relay|object|{relay_version}|{object_id}|{signed_at_unix_ms}
  ```

  `hex(sha256)` is 64 lowercase hexadecimal characters. Every component is
  `|`-free by the identifier rules. The signature time window matches pairing:
  two minutes in the past through one minute in the future.

- **Device token.** A Device sends its transport bearer. Access derives the
  Device principal and requires an active grant plus the bound session,
  channel, purpose and part scope. A Device cannot use the directory to read
  another Device's object.

The checked purposes are `REQUEST` (complete query/command body), `RESULT`
(complete result body), `EVENT` (complete control event payload),
`ATTACHMENT_PART` (one bounded attachment part) and `HISTORY_PAGE` (one
complete history page). A fetch for a different purpose is denied.

The reservation binds the object to `host_id`, `device_id`, `session_id`,
`channel_id`, `purpose`, `part_id`, `ciphertext_bytes`, `sha256`, the fixed
`expires_at_unix_ms`, `owner_node_id`, the original `grant_generation` and
`revocation_generation`, and a single reserved upload. A query-scoped
reservation additionally carries `request_id = 13`, `enrollment_generation =
14` and `connection_ref = 15`; these bind a referenced query result to the
original admitted waiter and Broker connection. They are empty for durable
command objects and attachment parts. `sha256` and `ciphertext_bytes` describe
the ciphertext, never plaintext. `finish` requires
the uploaded bytes to equal the reserved length and digest. On every ciphertext
release Access rechecks the active grant, both stored revisions and the exact
scope. A re-grant or revocation leaves an old reference fenced; an independently
authorized new read can create a new object. No release decision is cached.

The wrong-owner path answers `409 conflict` with `ErrorCode.conflict`,
`ErrorResponse.reason = "wrong_owner_path"`, and a `WhaleRein-Owner-Node`
header naming the authoritative owner. It is distinct from `404 not_found`
(no such object on the owner) and `410 expired` (object lifetime ended).
Terminal relay errors use existing codes only:

| Condition | Status | `ErrorCode` |
|---|---|---|
| Malformed scope, length or epoch | `400` | `invalid_argument` / `protocol_version_incompatible` |
| Missing or bad Host signature or Device token | `401` | `unauthorized` |
| Foreign scope, revoked grant or revision mismatch | `403` | `forbidden` |
| Object absent for this authorized scope | `404` | `not_found` |
| Wrong owner path, incomplete upload or digest mismatch | `409` | `conflict` / `payload_digest_mismatch` |
| Fixed expiry reached | `410` | `expired` |
| Object above the per-body or part bound | `413` | `payload_too_large` |
| Node, Host or active-upload quota refused this attempt | `429` | `rate_limited` (`reason=quota_exceeded`) |
| Filesystem headroom reserve refused | `503` | `unavailable` (`reason=storage_pressure`) |

`PUT` requires `Content-Length`; a missing, shorter or longer body is rejected
without reading past the reserved bound. `GET` returns the exact ciphertext
with `Content-Length`, `Digest: sha-256=...` and
`WhaleRein-Object-Expires-At-Unix-Ms`; a partial or truncated stream is never a
complete object. `DELETE` is bounded and idempotent for the same authorized
scope. Body budgets stay aligned with `TransportBudget`:

```text
reserve/finish JSON body  <= envelope_bytes
upload/resolved ciphertext <= envelope_bytes and <= reserved bytes
active uploads             <= bulk_concurrency
one upload/fetch           <= transfer_timeout_ms
```

`POST /v1/requests/{request_id}/heartbeat` renews the non-terminal request
claim while a one-shot CLI command waits for a terminal result. It carries no
command body and cannot change a terminal result. A silent claim lease is
bounded at 10 seconds; after it expires Access publishes `request_stale`, and
the Host aborts the request scope.

Internal broker callbacks (bound on the internal listener, fixed shared header):

```http
POST /internal/broker/connect
```

HTTP errors use `ErrorResponse` JSON with a stable `ErrorCode` value. Success
bodies are the messages above; `POST /requests` returns `RequestReceipt`.

`ErrorResponse.reason` carries a stable authority reason for a denied
long-lived observation. A non-empty reason is terminal: a client must not
reconnect, and must surface it instead of retrying. The values are protocol
constants, never free text:

| Reason | Meaning |
| --- | --- |
| `session_revoked` | The Cloud App session was revoked or expired. |
| `grant_revoked` | This Device's grant for the Host was revoked. |
| `host_removed` | The Host enrollment no longer exists. |
| `host_disabled` | The Host enrollment was disabled. |
| `authority_unavailable` | Authority could not be proven; the observation is closed rather than extended on stale state. |

A Host-authored terminal close uses `EventPayload.watch_closed` with
`{watch_id, session, reason}`. The reason is stable (`session_deleted`,
`session_expired`, `unauthorized`). The client treats it exactly like an Access
authority reason: terminal, no reconnect. The Host also writes the close to the
device outbox so a disconnected client receives it on reconnect; a stale
`watch_id` is ignored by a later observation.

`GET /v1/hosts/{host_id}/authority` is the authoritative authority check for a
live observation. A live grant, session and enrollment answer `204`; an ended
authority answers the documented `403`/`503` with `reason`. It remains part of
the public surface and is still called by Core. It does not renew a watch lease
and does not extend observation ownership; `CheckVersions` is likewise
read-only. The encrypted observation stream is end-to-end between Host and
Device, so Access cannot author a close frame in it; this check is the
authoritative lifetime signal.

## CLI exit codes

```text
0  success
1  internal / unexpected
2  invalid usage or invalid input
3  not found
4  unauthorized or revoked
5  host offline or unavailable
6  host rejected the request/command
7  timeout or unknown result
8  conflict: busy, digest mismatch, approval version/digest/state conflict
9  unsupported capability or incompatible protocol version
```

Every `ErrorCode` and every client action is listed in
`proto/v1/error_codes.json`; this table is the golden source for the CLI, Core
and Host mappings and must stay identical to it.

| `ErrorCode` | Exit | Client action | Meaning |
|---|---|---|---|
| `error_code_unspecified` | 1 | `none` | No error code was supplied. |
| `invalid_argument` | 2 | `fix_input` | The request is malformed or violates a wire constraint. |
| `unauthorized` | 4 | `reauth` | The caller is not authenticated; refresh pairing or credentials. |
| `forbidden` | 4 | `reauth` | The caller is authenticated but not authorized. |
| `not_found` | 3 | `refresh` | The named request, session, or object does not exist. |
| `conflict` | 8 | `refresh` | The request conflicts with current state; re-read state before retrying. |
| `expired` | 7 | `retry` | The request expired before it completed; submit a new request. |
| `unsupported_capability` | 9 | `upgrade` | The peer does not support the required capability. |
| `protocol_version_incompatible` | 9 | `upgrade` | The peers cannot agree on one protocol version; upgrade the named side. |
| `payload_digest_mismatch` | 8 | `refresh` | The same command id carried a different payload; use a new command. |
| `approval_expired` | 8 | `refresh` | The approval expired; request a fresh approval list. |
| `approval_version_mismatch` | 8 | `refresh` | The approval version is stale; request a fresh approval list. |
| `approval_already_resolved` | 8 | `refresh` | The approval was resolved elsewhere; refresh the list. |
| `approval_unavailable` | 8 | `retry` | The approval cannot be resolved right now; retry later. |
| `snapshot_required` | 8 | `resync` | The cursor can no longer continue; resynchronize from a snapshot. |
| `revocations_pending` | 8 | `retry` | The Host has not replayed pending revocations yet. |
| `rate_limited` | 8 | `retry` | A rate or concurrency limit was reached. |
| `internal` | 1 | `retry` | An unexpected local failure occurred; retry once, then collect diagnostics. |
| `unavailable` | 5 | `retry` | The service is temporarily unreachable; retry with backoff. |
| `payload_too_large` | 5 | `fix_input` | The logical message exceeds the transfer budget. |
| `pairing_fenced` | 8 | `restart_pairing` | The unfinished pairing intent can no longer commit. |
| `pairing_committed_unavailable` | 8 | `restart_pairing` | The FINISH committed but its grant is gone; pair again. |
| `pairing_committed_unconfirmed` | 8 | `review_pairing` | The FINISH committed but Host confirmation is unavailable. |
| `route_stale` | 8 | `refresh` | The serving route is closed or superseded; refresh the directory and retry on the current node. |
| `policy_denied` | 4 | `reauth` | Current managed policy denies this exact scope; do not retry on another node. |
| `node_unavailable` | 5 | `retry` | The authenticated node is not accepting placements or has no current Host authority. |
| `channel_replaced` | 8 | `retry` | The Host replaced the channel that owned the observation. |

`upgrade` is terminal for the current attempt: a client must surface an upgrade
prompt (or upgrade the peer) and must never map it to `unavailable`, which is
retryable. Pairing actions listed above are distinct: `restart_pairing` starts a
new confirmed invitation, `review_pairing` retains the pending FINISH until the
user reviews the evidence.

`channel_replaced` (exit 8 if recovery fails) identifies an observation whose
Host channel was replaced. Read-only waits may establish a fresh channel and
retry within their original deadline; source unavailability, device revocation,
explicit cancellation and request expiry retain their own error semantics.

Source failures from an authenticated wait or passive state read are carried
inside the encrypted response. They retain their source error code and do not
by themselves revoke the device or authorize a channel-replacement retry. If
the response cannot be authenticated, the Host reports unavailability instead.

`--json` emits a stable object with at least `schema: "whalerein/v1"`, the
command name, and either `data` or `error: {code, message}`. Explicit conversation
output shows normalized visible content to its requesting user. Diagnostics and
error output never include message bodies, tool arguments, private keys or pairing
tokens. There is no normal raw-native JSON output mode.

## Typed conversations, watches and synchronization

SessionKey is the Host/session pair. EventPayload contains only SessionUpdate:
a watch-scoped HistoryBatch, LiveFrame, SessionState, HistoryReset or an
independent QuestionDescriptor. There are no native type strings, data_json
payloads or public integer event sequences.

`WatchSession.interests` is required and selects conversation, questions or
both. An absent or empty selection is `invalid_argument`; there is no implicit
conversation default. A questions-only observation may omit `SessionState` and
must not read history. A conversation observation keeps its existing history,
live Attempt, runtime and Approval behavior. Questions readiness is independent:
a failed or unsupported Questions facet never ends a supported conversation.

`QuestionDescriptor` is `unsupported`, `unavailable` with a stable code, or
`available` with a `QuestionStamp`. An available descriptor with an empty epoch
or revision zero is invalid. A descriptor is not an answer guard and never
authorizes a native submission. Unchanged normalized inventory keeps the same
stamp; token output, runtime updates and repeated reads do not advance it.

Question pages carry the stamp of the complete inventory they were derived from
and certify completion only on the final page of that same inventory. A client
stage must reject a page with another stamp, a different epoch, a duplicate, or
a non-progressing continuation and restart from the first page against the
current descriptor. Only a complete same-stamp collection establishes Current,
including the genuinely empty collection. Mixed or partial pages are never
presented as complete. The authenticated mutable list cursor remains a
continuation cursor for explicit listing, not a future-event cursor.

### Committed content, reasoning and notices

`ConversationMessage` is committed content with stable identity, authorship and
ordered `ContentBlock` values. A block carries exactly one typed variant:
`text`, `reasoning`, `tool_activity`, `notice` or `attachment`. `ContentBlock`
reserves retired tags 3 and 4 together with the names `tool_call` and
`tool_result`; `tool_activity` uses tag 8. An old peer therefore skips an
unknown field instead of silently decoding the retired message type.
Reasoning is displayable content ordered beside answers; a reasoning-disabled
model emits no
synthetic reasoning block. `MessageRole` distinguishes genuine `USER` input,
`ASSISTANT`, `TOOL`, `NOTICE`, recognized `MESSAGE_ROLE_CONTEXT` and
`MESSAGE_ROLE_UNKNOWN`/missing authorship. A context or unknown message projects
only a `NOTICE_CATEGORY_CONTEXT` or `NOTICE_CATEGORY_OMISSION` notice and never a
user body. Private prompts, provider replay state, raw tool arguments and
unmapped payload bodies never appear in a normalized block.

`SourceCapabilities` independently declares `live_reasoning`, `tool_activity`,
`conversation_notices`, `attachments`, `history_page`, `history_exact` and
`send_followup`; an absent field means that feature is unsupported and must be
reported explicitly rather than simulated. Reasoning still follows the existing
conversation read authorization; a display preference never changes authority.

`NoticeContent` carries a typed `NoticeCategory` and known `NoticeState`, with an
optional Source-approved `summary`. Categories cover retry
(`SCHEDULED`/`STARTED`/`CANCELLED`), max output (`REACHED`), compaction
(`STARTED`/`COMPLETED`/`FAILED`), slash commands (`STARTED`/`COMPLETED`/`FAILED`),
recognized context and omission. Its Source position is its ordered block
position in the Message (or live block sequence); state is present only when the
Source established it. A missing predecessor or summary leaves a truthful
partial view. Raw command arguments and compaction input are never carried.

`ToolActivity` is one session-scoped `call_id` activity. It carries only
evidenced `ToolStage` values in ascending order (`PREPARATION`, `EXECUTION`,
`PROGRESS`, `SETTLEMENT`), an optional display-safe `summary`/`progress`, an
ordered `ToolResultPart` list, a `ToolOutcome`, and an optional stable failure
code. Result parts may carry text or an attachment occurrence reference, never
bytes. A prep/dispatch/result sequence for the same `call_id` describes one
activity: Core merges repeated occurrences by `(SessionKey, call_id)` across
live output, history and combined pages with no native correlation. A committed
result takes precedence over delayed progress or dispatch; abandoning an
Attempt clears only its unexecuted preparation.

### Live frames

LiveFrame carries an epoch, Attempt ID, block ID and inclusive normalized sequence
interval. Start/end are separate typed payloads. `delta` is a `LiveBlockDelta`
that appends one block; `reset` is a `LiveBlockReset` that replaces that same
provisional block. Both carry a checked `BlockKind`, and only `BLOCK_KIND_TEXT` or
`BLOCK_KIND_REASONING` is valid for live output. A reader never merges reasoning
into an answer block or the reverse. Adjacent deltas can coalesce for 20 ms /
8 KiB without crossing Attempt, block, kind or order boundaries. Preview loss
marks an incomplete overlay; it never advances the committed cursor. The final
Message links to its origin Attempt and supersedes provisional content.

### Independent history reads

`HistoryRead` selects exactly one mode:

- `page` is a latest-first bounded selection. An empty `page.before_cursor`
  starts at the most recent authorized committed message at the fixed cut and
  later pages step toward older history.
- `exact` is one `CommittedMessageRef` resolved with the same scope, content and
  resource policy as paged history.
- `after_cursor` is forward traversal for synchronization. It is mutually
  exclusive with `page`, `exact` and `continuation`.
- `continuation` resumes a Core-owned traversal. It binds the validated scope,
  projection, fixed cut, next position and read source; a caller never parses it.

An empty selection is a forward read from the origin. `source` selects online or
local cache history; cache reads use the same selection semantics and never
reach a Host. `HistoryPage` returns `continuation` (empty when no next page
exists at the cut), `projection_id` and the concrete `source`. Messages within a
page are in Source order even when pages step backward. A wrong-scope, expired,
projection-mismatched or non-progressing continuation fails with
`snapshot_required`; an individually oversized canonical message fails with
`payload_too_large` before the cursor or receipt advances. Cursor tokens belong to
their Source, session, projection and view; clients compare continuity, not
numeric/lexical order. Batches may advance across filtered records without a
visible Message. Snapshot pages stage until the final complete page and never
silently replace a complete cache with a truncated view. Repeated batch IDs with
conflicting canonical content are rejected. Message origin is transient
correlation metadata, not a second authority.

A public page read owns its HTTPS request budget, has no watch lease or MQTT
subscription and does not write the synchronized cache. A cached read uses the
eligible local cache and takes a fixed local cut; replacement or eviction
invalidates its continuation explicitly. `HistoryBatch.projection_id` and
`SessionState.projection_id` bind cursor, receipt and live namespaces to the same
content projection.

Core subscribes MQTT before WatchSession and baseline synchronization. Watch IDs
are channel-scoped and bound to the authenticated Broker connection reference
and observation generation that admitted the channel. There is no lease
renewal and no `RefreshWatches`, `WatchLease` or `WatchStates` operation.

Core issues `CheckVersions{session_id}` on the query lane every five seconds as
one bounded read-only check per actively observed Host, with at most one check
in flight. `CheckVersions` never renews or extends ownership. Host coalesces
overlapping checks for the same session and facet and returns `VersionChecks`
with one `VersionCheck` per active watch: `watch_id`, `revision`, `run_phase`,
`live_epoch`, `history_head`, `projection_id` and the question descriptor,
never `SessionState` or content. Each requester still checks its own
authorization and observation generation before using a shared read. A
conversation-only check does not read the questions facet, and a Questions-only
check does not read the conversation facet.

The marker read is authoritative, so a lost last notification is still
repaired, but the periodic message cannot grow with the transcript. Content
travels only on the event stream and the paged history read. A structural marker
change re-attaches for a fresh baseline; a moved history head repairs the
transcript through paged reads; a newer revision rebuilds state from markers
plus the approvals query. Deterministic refusals (`payload_too_large`,
`invalid_argument`, or an authoritative denial) end the affected observation
with their own code; transient failures leave the affected state stale until
the next check. Unwatch is best effort. Explicit close, ordered disconnect,
snapshot repair or a Broker epoch change closes only that connection instance's
observations. Current Core admits one active watch per instance so device MQTT
client identities cannot compete internally.

Approvals and runtime state are current-state projections outside history cursor
arithmetic. `SessionState` is a bounded transition summary (session, epoch,
revision, history head, run phase, projection identity and question descriptor):
pending approvals are content and travel only through the bounded `ApprovalList`
query, so neither a baseline, a state update nor a version check can grow with
the pending approval set. Approval execution always revalidates the native
pending action.
The complete schema and Source invariants are in
[the Harness contract](../../docs/architecture/harness-contract.md).

### Attachment reads

Committed `AttachmentContent` carries a display name, media type, declared size,
availability and an opaque `attachment_ref` occurrence identity scoped to the
session. Bytes never travel in conversation events, history pages or the
encrypted transcript cache. An explicit read uses the existing encrypted
query/result path:

| Query | Result | Purpose |
|---|---|---|
| `OpenAttachment{session, attachment_ref}` | `OpenAttachmentResult{attachment}` | Authorize the exact occurrence and return authoritative metadata plus a short-lived transfer handle. |
| `PullAttachment{session, attachment_ref, transfer_id, offset, max_bytes}` | `AttachmentPart` | Read one contiguous bounded part. |
| `CloseAttachment{session, attachment_ref, transfer_id, cancel}` | `CloseAttachmentResult` | Dispose or cancel the transfer. |

`AttachmentMetadata` returns `SessionKey`, `attachment_ref`, `media_type`, a
display-only `name`, authoritative `size`, the effective `AttachmentPolicy`
(total bytes, chunk bytes, concurrent reads, idle and overall time) and the
expires-at timestamp of the opaque `transfer_id`. The effective chunk is the
smallest of attachment policy and the directional encoded transport, relay,
HTTP representation and receiver limits. A declared size above the effective
total fails with `payload_too_large` before any native byte read; unsupported or
missing native storage is `unsupported_capability` or `not_found`.

Each `AttachmentPart` echoes the session, occurrence and transfer, and carries
`offset`, declared `length` and `data`. Only one read is in flight per transfer.
A duplicate of the last completed offset is the only idempotent retry; every
other gap, overlap or conflicting part is `conflict`, and an offset or length
outside the effective budget is `invalid_argument`. The final part carries
terminal `total_size` and `sha256`; before final those fields are absent. A
duplicate, gap, wrong length or wrong digest is `payload_digest_mismatch` and the
client publishes no completed file. Cancel, channel loss, revocation, idle
expiry or deadline disposes the iterator and releases reservations; an explicit
fresh read starts again at offset zero.

The transfer is scoped to the authenticated device, Host, session, channel and
occurrence. Substituting any of them fails with a stable scoped error and reaches
no native byte read. The Host-verified occurrence is resolved in hot or cold
session history before the reader opens. Filenames are display suggestions; the
client validates a safe local name, stages data under its own temporary file and
only publishes the final name after a successful save. A failed save preserves
existing data and removes only the download's temporary data.

### Contract surface snapshot

`proto/v1/compat/surface.json` pins the public contract surface: HTTP routes,
MQTT topic patterns, protobuf message and enum names, the `ErrorCode`
vocabulary, and the harness type aliases below. A route or type rename must
update the snapshot, WIRE.md and the Harness contract in one reviewed change;
`proto/v1/bindings/rust/tests/compat.rs` fails otherwise.

| Harness contract name | Wire type |
|---|---|
| `Message` | `ConversationMessage` |
| `HistoryBatch` | `HistoryBatch` |
| `LiveFrame` | `LiveFrame` |
| `RuntimeState` | `SessionState` |
| `ApprovalState` | `Approval` (and the `ApprovalState` enum) |
| `HistoryReset` | `HistoryReset` |

The snapshot also records the zero-value policy (`omitted`), so a reader cannot
silently start distinguishing an absent field from its default.

## Approval digest

```text
escape(field) = field with "\" -> "\\" and "|" -> "\|"
digest = SHA-256(
  "whalerein/v1/approval|{escape(approval_id)}|{escape(session_id)}|" +
  "{escape(tool_name)}|{escape(call_id)}|{escape(reason)}|" +
  "{version}|{expires_at_unix_ms}"
)
```

Every string field is escaped before joining, so a reason or tool name that
contains `|` or `\` cannot shift a field boundary. Values without either byte
keep their historical digest. Both Core (`core::security::approval_digest`) and
the Host connector (`approvalDigest`) must match the vectors in
`proto/v1/approval_digest_vectors.json`; the Rust and Node unit tests read the
same file.

The digest and version live only inside encrypted commands and events. The
client sends them back with `ResolveApproval`; the plugin checks pending state,
expiry, digest, version and task state, then resolves exactly once. Any mismatch
or late resolve returns `approval_expired`, `approval_version_mismatch` or
`approval_already_resolved` and never reaches DSH.

## Logical MQTT transport

One coordinated relay epoch (`relay_version=5`) replaces the retired epoch-4 relay and transport v2. On the
control class a complete application message travels in exactly one MQTT
PUBLISH. Each hop picks the representation for its own outgoing packet:

- **inline** — the serialized outer envelope carries the complete opaque body
  (`HostRequest.body`, `HostResult.result_body`, or `EventEnvelope.ciphertext`);
- **reference** — the same envelope carries an opaque `PayloadReference` to the
  exact ciphertext stored on its owning Access node. An `EventEnvelope` keeps
  its `nonce` in either representation; only the ciphertext moves. The reader
  fetches the complete ciphertext over the HTTPS directory, verifies length and
  SHA-256, and then applies the same AEAD and business validation as for an
  inline packet.

Only routing, expiry and size metadata travel outside the seal. Application
message identity, AEAD keys, channel binding and authoritative ordering do not
change with the representation. A business handler receives one resolved
complete body.

```text
HostRequest      body                    | body_reference
HostResult      result_body              | result_reference
EventEnvelope    nonce + ciphertext      | nonce + payload_reference
```

### Per-hop packet fit

`TransportBudget.packet_bytes` is the largest complete encoded MQTT PUBLISH a
receiver accepts. The full packet counts, not just ciphertext:

```text
remaining_length = 2 + len(topic) + 2 + varint(len(properties))
                 + len(properties) + len(payload)
encoded_publish  = 1 + varint(remaining_length) + remaining_length
```

The leading `1` is the PUBLISH fixed header with QoS 1; the `2` before the topic
is its length prefix; the following `2` is the packet identifier required by
QoS 1; `properties` are the MQTT 5 PUBLISH properties; `payload` is the binary
protobuf outer envelope. The outer envelope already contains every protocol
field, the reference or inline body, nonce, and the 16-byte ChaCha20-Poly1305
tag. A sender computes `encoded_publish` with the actual topic and properties it
will use; it never estimates from ciphertext length alone.

A QoS 0 live packet has no packet identifier: its `encoded_publish` is
`1 + varint(remaining_length) + remaining_length` with
`remaining_length = 2 + len(topic) + varint(len(properties)) +
len(properties) + len(payload)`. The receiver budget and the packet-fit rule
below apply identically; the QoS 0 packet identifier is not part of the count.

For one hop:

```text
limit = min(local send packet limit,
            broker packet limit (brokerPacketLimitBytes),
            receiver ReceiveAdvertisement.budget.packet_bytes)
```

The sender then applies exact semantics:

| Complete encoded packet | Action |
|---|---|
| `< limit` (below) | Publish one inline packet. |
| `== limit` (at) | Publish one inline packet; the boundary is inclusive. |
| `> limit` with a ready object | Publish one reference packet only. |
| `> limit` and reference packet also `> limit` | Publish nothing; use a bounded route with a larger limit or fail explicitly before publish. |

A reference is never published before its complete ciphertext is readable.
`envelope_bytes` is checked separately on the complete serialized message; a
message above it fails with `payload_too_large` and no partial representation is
sent. The relay never truncates, fragments, reassembles or retries at the
application packet layer. A receiver with a smaller limit than the minimum
reference envelope makes that route ineligible instead of receiving a broken
packet.

### Per-hop rewrite

Access is a ciphertext relay. It can receive an inline envelope and publish a
reference for the next hop, or reuse a reference:

1. keep the same message identity (`request_id` + `command_id` +
   `payload_digest`, or the event channel and creation identity);
2. store the exact received ciphertext if no readable object exists, under the
   matching `PayloadPurpose`, owner, scope and fixed expiry;
3. publish a `PayloadReference` only after length, digest and publication
   complete.

The reverse rewrite is also allowed when the complete ciphertext is readable
and the inline packet fits. Access never inspects, re-encrypts or reorders the
body, and the Host always dispatches the same complete message once.

### Ordered control delivery

The control class is QoS 1 with the broker's negotiated receive maximum and
connection flow control. Each publisher uses ordered submission and bounded
native in-flight work; Access uses a flow-preserving forwarding queue. There are
no application fragment, transfer or receipt acknowledgements, and no device ACK
topic. A failed publication is retried only at the same complete-message
identity, so duplicate MQTT delivery is handled by the existing `request_id`,
`command_id`/digest and typed event-sequence rules. A blocked bulk HTTPS
transfer cannot stall an eligible small MQTT packet.

### Live delivery class

The live class is QoS 0 on the dedicated live topics. It is not ordered by the
control queue, never waits for an acknowledgement, never enters a durable
outbox or receipt and never uses the HTTPS reference carrier. A live publication
failure is a drop, not a retry. Control messages are never queued behind a live
preview.

**Host live publication.** The Host publishes each packet-sized preview as one
QoS 0 `EventEnvelope` directly on the granted Device's
`whalerein/v1/h/{host_id}/d/{device_id}/live` topic. Access neither subscribes
to nor republishes it. The packet-fit and UTF-8-safe split rules still apply, so
a preview above the route packet budget is split into ordered pieces before
publication; a live preview never becomes a reference. There is no ACK wait and
no per-watch or per-topic live serialization chain: QoS 0 preserves per-client
order and has nothing to acknowledge. A bounded per-connection live queue (for
example 256 KiB or 64 packets) protects memory; when it is full the Host drops
the oldest pending preview or coalesces text and never blocks or displaces a
control or query message. If the connection or broker is not ready, the preview
is dropped. Previews are never published on the control `event` topic.

The Broker delivery policy for that topic requires an authenticated Host
publisher, the exact Host/Device topic and a current grant for the receiving
Device connection. A preview that would outlive a grant is fenced or dropped
rather than delivered under a newer grant. Access adds no durable row, outbox
entry, receipt or notice for a live preview.

**Core live reception.** Core subscribes to its Device live topic at QoS 0 in
addition to the control event topic and delivers live frames to the same
observation pipeline with the same sequence and duplicate checks. A gap or loss
marks the preview incomplete; it never fails the observation, never retries and
never sends an ACK, and it never blocks control delivery. The committed message
arriving on the control path or an explicit authoritative HTTPS read supplies
the complete state and repairs the derived preview.

## Transport budgets and configuration

`proto/v1/transport_profiles.json` is the single packaged profile vocabulary
shared by Access, Host and Core. `default` preserves the measured operating
profile: a 64 KiB complete MQTT packet and a 16 MiB logical envelope.
`compact` and `roomy` are valid alternate profiles.

`TransportBudget` is packet-based:

| Field | Meaning |
|---|---|
| `packet_bytes` | Largest complete encoded MQTT PUBLISH accepted, including fixed header, topic, MQTT 5 properties, outer envelope and ciphertext framing. |
| `envelope_bytes` | Largest complete encoded message accepted in any representation. |
| `bulk_concurrency` | Concurrent HTTPS ciphertext transfers admitted per receiver. |
| `reserved_bytes` | Ciphertext bytes the receiver may hold reserved or in flight. |
| `transfer_timeout_ms` | One bound for a complete HTTPS upload or fetch. |

The retired fragment geometry (`fragment_payload_bytes`, `fragment_count`,
`fragment_idle_timeout_ms`) no longer exists. The profile document may still
carry a local `packetOverheadBytes` planning reserve, but it is not a wire
budget: the actual encoded packet is authoritative.

| Limit | Default | Compact | Roomy | Validation |
|---|---|---|---|---|
| Complete MQTT packet | 64 KiB | 32 KiB | 128 KiB | Positive, representable, and `<= brokerPacketLimitBytes` (1 MiB). |
| Logical envelope | 16 MiB | 8 MiB | 32 MiB | Positive, representable, and `>= packet_bytes`. |
| Concurrent HTTPS transfers | 4 | 2 | 8 | Positive. |
| Reserved bytes | 32 MiB | 8 MiB | 64 MiB | `>= envelope_bytes`. |
| Transfer timeout | 120 s | 120 s | 300 s | Positive and representable in `uint32`. |

The default profile must retain the 64 KiB packet / 16 MiB envelope semantic.
A budget may carry validated operator overrides instead of matching a packaged
profile exactly. Values are never clamped: a sender takes the minimum of its
local limit, the broker limit and every advertised receiver limit. Invalid or
contradictory settings fail before readiness or connection use. Every value is
validated at admission, never repaired by truncation.

`proto/v1/transport_profiles.json` is authoritative. `./repo proto` generates
three committed runtime artifacts from it, and `./repo check proto` fails on any
drift:

| Runtime | Committed artifact | Loaded through |
|---|---|---|
| Access (Go) | `proto/v1/bindings/go/transport_profiles_gen.go` | `whalereinv1.TransportProfileValuesFor` and `ValidateTransportProfiles` |
| Host (Node) | `host/runtime/src/generated/transport-profiles.js` | `host/runtime/src/contracts/transport.js`, which re-exports the generated values |
| Core (Rust) | `proto/v1/bindings/rust/src/transport_profiles.rs` | `transport_profile_values` and `validate_transport_profiles` |

Edit the JSON, then run `./repo proto`; never hand-edit a generated profile
artifact.

### Authenticated receive advertisements

`ReceiveAdvertisement` is the only exchange of receive limits. It carries a
named `profile`, the validated `budget`, an authenticated
`connection_generation` and the required `relay_version=5`. A generation is
scoped to the leg that carries the advertisement; generations from different
legs are never compared. A delayed or replayed advertisement cannot update a
newer connection, and a peer cannot raise another peer's limit.

| Leg | Carrier | Registered against |
|---|---|---|
| Access -> Host request hop | `HostReady.receive` | Host's authenticated broker connection generation |
| Host -> Access result/event hop | `REQUEST_KIND_ACCESS_READY` body | Access's advertised generation for that Host connection |
| Access -> Device event/notice hop | Device `ReceiveAdvertisement` on `.../d/{device_id}/ready` | Device's authenticated broker connection generation |
| Host <-> Device end-to-end | `ChannelHello.receive` / `ChannelReady.receive` | That authenticated Noise channel generation |

A shared event route uses the minimum eligible receiver budget for its hop. For
preview splitting, the Host uses the smallest usable payload budget across:

```text
min(Host local send packet limit,
    Access REQUEST_KIND_ACCESS_READY receive budget,
    Device ChannelHello.receive budget)
```

It splits only on valid UTF-8 boundaries, assigns contiguous normalized preview
intervals, and rechecks the fully encoded packet against that minimum before
publication. Reconnect binds a fresh advertisement to the new generation.
Access registers only receive information across these seams; it never lets a
Device, Host or stored reference raise a local or broker limit.

`HostRequest.transport` binds four request-scoped values at submission:
`result_max_bytes` is the device's validated HTTPS result bound, while
`grant_generation`, `revocation_generation` and `enrollment_generation` are the
durable Access revisions for the `(host_id, device_id)` grant and its managed
enrollment. Access captures them from durable state when it admits the request
and keeps all four values attached to the exact durable command request across
restarts.

For a query-lane request, Access additionally fills the top-level
`HostRequest.connection_ref`, `HostRequest.grant_generation` and
`HostRequest.enrollment_generation` from the admitted Broker connection and
current authority. Those fields are immutable for the life of the transient
waiter; Host binds observation ownership and reply affinity to them and rejects
a query or handshake that lacks them.

At ciphertext release Access requires exact equality of both revisions against
the current durable grant row, and the grant must still be active. It applies
that rule on every release path: a first GET, a repeat GET, a GET after
revocation, and a GET after a later re-grant. A stored request admitted before
a revoke stays fenced after that Host-scoped revoke, which bumps only
`revocation_generation`, and it stays fenced through any re-grant that bumps
`grant_generation`; a request admitted after the re-grant carries the new stamp
and can release. No positive release decision is cached, and the comparison
uses the two integer revisions rather than a packed or cached revision.

A QUERY, including attachment parts, whose request metadata is missing either
revision fails closed and releases no ciphertext. A COMMAND applies the same
no-body rule on a missing or unequal stamp but keeps metadata-only recovery:
status, error code, command ID and payload digest stay readable so
deduplication and durable command recovery continue to work. Access never
inspects the encrypted body; the revisions and the result bound are metadata
only. Previously delivered bytes are outside revocation's reach.

A complete canonical message that cannot fit an effective result or event budget
fails explicitly with `payload_too_large`; the last cursor and receipt stay
valid, the batch does not advance, and a later read with sufficient capacity can
progress. Packing and admission never truncate canonical content or rewrite a
committed message.

| Limit | Value | Validation |
|---|---|---|
| Access default HTTP request body | 24 MiB, allowing base64 expansion and envelope overhead | Independent of profile bytes. |
| Broker packet ceiling | `brokerPacketLimitBytes`, 1 MiB in the packaged profiles | Every profile packet budget must not exceed it; ingress limits remain independently enforced. |
| Relay object upload | Reserved exact length, `<= envelope_bytes` | Missing, shorter, longer or digest-mismatched bodies never publish. |
| Relay object fetch | Exact stored length | A truncated stream is never a complete object. |

Oversized messages fail explicitly; snapshots must not silently become
successful partial results. Keep clients, broker ACLs, and HTTP/reverse-proxy
limits aligned. This source description does not imply that interoperability
has been verified.

## Relay cutover

The cutover is one coordinated hard cut to epoch 5:

- **One required epoch.** `relay_version=5` is the only accepted MQTT relay.
  Epochs 3 and 4 are retired, their enum numbers and names are reserved, and
  they are rejected with `protocol_version_incompatible`. There is no v2
  acceptance, no unframed fallback, no `WRMQTT2` prefix, no dual epoch and no
  compatibility bridge. `TransportFrame`, fragment fields, transfer receipts
  and every acknowledgement topic are removed from the contract.
- **One delivery contract.** Epoch 5 keeps the QoS 1 control topics, adds the
  `query`/`reply` request-reply pair for post-pairing Noise and session queries,
  and moves previews to direct QoS 0 Host-to-Device publication. The Host
  `.../live` relay topic is retired. A preview is never published on the
  control `event` topic or through Access; a query is never admitted on the
  retired HTTPS handshake/query route; `RelayTransport` never carries a live
  frame. There is no old-contract reader, no feature flag and no dual publish
  path.
- **Readiness gate.** Access marks a Host ready only after a valid
  `HostReady` with `relay_version=5` and a valid advertisement. Access rejects
  a Device or Host advertisement with any other epoch, including epochs 3 and
  4. Admission of a `HostRequest`, `HostResult`, `EventEnvelope`,
  `RequestStateNotice`, query-lane message or relay object request requires the
  matching epoch and application `protocol_version=1`; direct live delivery
  additionally requires an active grant for the receiving Device and a current
  Broker connection reference.
- **Legacy artifact rejection.** An older Access, Host, Core, CLI, mobile build
  or broker ACL that attempts v2 framing, a removed receipt/ack topic, a retired
  epoch-4 query or preview path, an epoch-3 or epoch-4 advertisement, or the
  retired Host `.../live` topic fails explicitly with
  `protocol_version_incompatible` before business delivery. A mixed artifact
  set is not a supported release.
- **Unchanged application version.** The application protocol version stays 1.
  The epoch is the transport cut: it changes the query ingress, removes the
  preview relay and watch renewal, and does not change the semantics of grants,
  keys, sessions, history, approvals or command identity. Existing pairing
  credentials, Noise identities, pins, grants and durable command recovery
  identities remain valid across the cut.
- **Operator coordination.** Access, Host, broker policy, Core and supported
  clients deploy together. Quiesce old admissions; drain or expire old
  in-flight queries; preserve original command identities, ambiguity and
  durable command rows; add the query/reply pair and direct Device live policy
  to the broker ACL in the same release; then gate readiness on the new
  contract. A retired Device Will is not a presence or revocation fallback:
  ordered Broker lifecycle events, the broker epoch and snapshot reconciliation
  establish scoped disconnects. Rollback requires its own fenced admission and
  verified recovery; a live mixed-epoch rollback is outside the contract.

## Model operations

The encrypted payload has separate catalog and passive session-model queries,
and a journaled `select_session_model` command. A `ModelChoice` distinguishes
an absent effort from an explicitly supplied value. The selection receipt
carries the accepted choice, its source-default attempt scope, and an
`unconfirmed` persistence value; it does not indicate that a prompt ran.
Missing `SourceCapabilities.model_selection` means selection is unsupported.
The shared `get_host_capabilities` query returns `HostCapabilities`; absent
`model_catalog` and `model_read` fields mean unsupported. Clients treat an
older Host that rejects the query as having neither operation. Catalog and
passive reads are independently gated by the Host. Native DSH exposes `next`
and `last_used`; `pending_known=false` means that projection cannot prove
whether a pending record exists, even when a next choice is present.
For model queries, the complete encoded `HostResult`, including the encrypted
response envelope, is checked against the effective `envelope_bytes`; an
oversized result fails with `payload_too_large`.
### Session lifecycle receipts

`ForkSession` is a session-scoped control with a source session ID and no native
history boundary. Creation and fork return a `LifecycleReceipt` with one of four
dispositions:

- `READY` requires a confirmed `session_id` and carries no warning.
- `READY_WITH_WARNING` requires a confirmed `session_id` and a `warning_code`;
  the operation succeeded and must not be retried or deleted.
- `FAILED` proves no native effect; it carries `reason_code`, `retryable`, and
  no identity.
- `UNKNOWN` means a native effect may have happened. It never carries identity
  and always names a `reconcile_mode` (`OPERATION_ID`, `PREFERRED_SESSION_ID`,
  `PARENT_SESSION_ID`, or `MANUAL`) plus an optional `reconcile_key`.

`phase` is coarse (`PREFLIGHT`, `NATIVE`, `ATTACH`, `PERSIST`, `COMPLETE`).
`operation_id` is a stable correlation identity independent of the command
journal. The warning code `workspace_attach_failed` preserves a child identity
when native workspace attachment failed after creation. Neither working
directory, workspace data, nor raw native diagnostics belong in the stored
receipt. Host and source capabilities must both advertise fork support before
invocation; absent fields mean unsupported.

`pairing_fenced` (exit 8) is an Access rejection proving that an unfinished
pairing intent cannot commit. Revocation or migration of an older unfinished
intent can invalidate it. A client may discard that exact pending FINISH and
start a newly confirmed invitation. Transport errors and other pairing failures
do not establish this fact.

`pairing_committed_unavailable` (exit 8) proves the exact FINISH was committed,
but its original grant/credential is no longer active. It does not identify why
the grant became unavailable. Clients retain a redacted completion record before
replacing the pending FINISH; it is distinct from an uncommitted fenced intent.

`pairing_committed_unconfirmed` (exit 8) proves Access committed the exact FINISH
and the original grant remains active, but its Host COMMIT confirmation is not
available. It does not assert Host activation. The client retains its original
pending FINISH and credential, as well as redacted evidence, for user review.
If neither a durable matching proof nor sufficient retained evidence exists,
the client does not automatically start a new pairing.

After Access has durably recorded an authenticated Host COMMIT acknowledgement
for the exact grant and public key, with its persisted COMMIT binding verified,
a matching original FINISH may be recovered
after cleanup while that original grant remains active. This does not resubmit
a new grant. If confirmation was never durably recorded, the outcome remains
`pairing_committed_unconfirmed`.

An old active outcome whose retained data cannot prove its COMMIT binding and
matching, timely authorized Host acknowledgement also remains
`pairing_committed_unconfirmed`; its prior confirmation flag alone is not
sufficient. The binding record is derived in the Access database under its
device fence, not an independent cryptographic signature.

Access serializes pairing, acknowledgement, and revocation for one Device under
a database device fence, and it rechecks the invitation and operation deadlines
after a fence wait so contention cannot extend them. A claim that cannot take
its in-process invitation serialization before its own deadline fails with
`conflict` (HTTP 409) and changes no durable state; it never reports a Host
transport failure. The Core CLI maps `unavailable` to exit 5 and `conflict` to
exit 8.

## Independent Access nodes

One Access node serves one Host at a time. Central owns the durable route row,
node health and bounded managed authority; an Access node owns its local SQLite
database, temporary ciphertext and local Mosquitto broker. Node control messages
carry metadata only and never contain request bodies, prompts, results or
temporary ciphertext.

- **Node report.** Access sends `NodeReport` every 10 seconds with a strictly
  increasing sequence for its incarnation. Central uses its own receipt time.
  A duplicate or stale sequence does not refresh health. At 30 seconds without
  a fresh report Central excludes the node from new placements.
- **Drain.** An operator marks one live incarnation drained through
  `NodeDrain` with the node credential. A drained node keeps its established
  routes but is never advertised for a new placement; a heartbeat does not
  clear the flag. Resume only after the node is healthy again.
- **Attach.** The Host increments a persistent generation before starting an
  attachment. Access forwards `HostRouteAttach` with proof that it observed an
  authenticated Host transport and the Host signature over the exact service,
  environment, Host, node, incarnation, generation, challenge and keys. Central
  returns `committed`, `idempotent`, `route_stale` or `conflict`. A greater
  proved generation replaces the route; an equal open tuple is idempotent; a
  closed or lower generation is stale and never reopens.
- **Close.** A graceful Host shutdown publishes `HostReady{route_closing=true}`
  with the same attach proof; Access forwards the exact tuple as
  `HostRouteClose`. A transient disconnect does not close the route, so the same
  process can reattach without a new generation. `HostRouteClose` for a
  generation at or above the high-water mark
  persists a terminal closed row even when its attach has not arrived. A close
  for a greater generation needs the same Host proof as an attach. A forged
  close changes nothing. Duplicate closes are idempotent.
- **Discovery.** Only an open route is advertised. A directory answer reports
  the node address, route generation and Central freshness separately from the
  endpoint's local liveness. A stale hint never grants access.
- **Authority.** `ScopedAuthority` binds issuer, environment, workspace, Host,
  Device, App session, both grant generations, serving node, route generation,
  the immutable App-session proof key, policy revision and an absolute expiry
  of at most 60 seconds from Central confirmation. Access and Host verify the
  signature, scope, node and expiry independently. A renewal never replaces the
  proof key or extends beyond the committed maximum.
- **Revocation.** Central records the revocation first and refuses issuance
  under the affected revision. The `RevocationStatus` state is
  `pending_enforcement` until the frozen maximum affected deadline and
  `effective` afterwards. Node acknowledgements record delivery progress only.
- **Denial vocabulary.** `route_stale` means the directory may supply a newer
  route and a Device may retry on the current node; `policy_denied` is terminal
  for the scope and must never trigger a retry elsewhere.
