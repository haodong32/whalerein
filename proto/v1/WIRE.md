# WhaleRein v1 wire contract

`proto/v1/whalerein.proto` is the only wire contract. HTTPS uses the proto3 JSON
encoding of these messages (lowerCamel fields, enum value names as written,
bytes base64). MQTT uses the binary protobuf encoding. There is no peer
OpenAPI or JSON Schema.

## Identifiers

All identifiers are opaque, random, and logged only through a local salted
diagnostic id.

| Field | Producer | Meaning |
|---|---|---|
| `request_id` | client | Correlates one `HostRequest` with its results. |
| `command_id` | client | Idempotency key for control commands. Empty for handshakes, queries and pairing. |
| `payload_digest` | client | `SHA-256(body)` for control commands. Empty otherwise. |
| `channel_id` | client | Random secure-channel id, new for every handshake. |
| `trace_id` | client | Diagnostic correlation only. Never business, never idempotency. |
| History cursor | Source | Opaque, session/view-bound committed position. |
| Attempt reference | Source adapter | Live epoch plus Attempt ID, independent of Message identity. |

Access preserves `command_id` and `payload_digest` after a request expires and
drops only the bodies. The same `command_id` with a different digest is
rejected and is never forwarded.

## Topics

```text
whalerein/v1/h/{host_id}/request
whalerein/v1/h/{host_id}/result
whalerein/v1/h/{host_id}/event
whalerein/v1/h/{host_id}/d/{device_id}/event
whalerein/v1/h/{host_id}/d/{device_id}/ack
```

Access publishes `request`; the host subscribes only
to its own `request` and publishes only its own `result` and `event`. Access
subscribes to `result` and `event` and republishes an event to
`.../d/{device_id}/event` using only the outer `device_id`. A device subscribes
only to its own event topic and publishes transport receipts only to its own
`ack` topic. Access subscribes to device ACKs. Cross-host wildcard subscriptions
are denied by broker ACL.

Client IDs and usernames:

```text
access        username access, password = access broker credential
host          client id whalerein-host-{host_id}, username host:{host_id}
device        client id whalerein-device-{device_id}, username device:{device_id}
```

Hosts authenticate with the deployment provisioning token from configuration.
A host is inserted into `hosts` on its first successful broker authentication.
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
host_fingerprint}`. Access returns it to the client. Nothing is written yet.

The client completes the XX read of msg2, obtains the host static public key
from Noise (not from Access), computes the fingerprint, and asks the user to
confirm. A mismatch or cancellation ends the flow. No `grants` row exists and
the host has not saved the peer key.

### Step 2: FINISH

`POST /v1/pairing/claims` with `phase=FINISH, confirmed=true, noise_message=msg3`.
Access revalidates the token and host readiness, forwards
`PairingClaim{FINISH, msg3}`. The host completes XX, verifies `device_id`,
and returns `PairingClaimResult{device_id, device_public_key}` without
persisting the peer key.

Only now does Access write `devices` and `grants` in one transaction, generating
the device transport token and storing only its SHA-256. Access then publishes
`HostRequest{kind=PAIRING_AUTHORIZE, body=PairingClaim{AUTHORIZE,
device_id, device_public_key}}`; the host persists the peer key and returns
`authorized=true`. Access marks `pairing_tokens.used_at` and returns
`device_token` to the client once. If the host authorization step fails,
Access compensates by revoking the grant it just wrote.

A token with `used_at` set is rejected forever. A failure anywhere increments
`failure_count`; after the configured budget the token is rejected before
forwarding.

## Secure channel

After pairing, both sides run Noise XX with prologue:

```text
whalerein/v1/channel|{host_id}|{device_id}|{channel_id}
```

A channel handshake is two `HostRequest{kind=HANDSHAKE}` calls:

1. `body = msg1`. Host replies `HostResult{status=succeeded,
   result_body=msg2}` after checking the device has an active grant and has
   persisted peer key and no revocation replay is pending.
2. `body = msg3`. Host returns `HostResult{status=succeeded, result_body empty}`.
   Both sides now derive keys.

Noise payloads are `ChannelHello` / `ChannelReady`; they carry the negotiated
protocol version and capabilities. A version mismatch rejects the handshake
with `protocol_version_incompatible` before any command executes.

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

A wrong channel, failed MAC or failed AAD rejects the message without side
effects. Old-channel messages are rejected because only the current
`channel_id` maps to a key. Replays are handled by `request_id`/`command_id`
dedupe, typed history intervals and live Attempt sequences. None is a nonce.

## Requests and results

`POST /v1/hosts/{host_id}/requests` accepts a `HostRequest` from an authorized
device. Access:

1. authenticates the device transport token and an active grant;
2. checks `expires_at` and protocol version;
3. for a command, looks up `(host_id, device_id, command_id)`:
   same digest returns the existing row, different digest returns
   `payload_digest_mismatch` without forwarding;
4. if the host has no broker connection, writes `host_offline` with
   `forward_state=none` and returns;
5. otherwise inserts `received`, `forward_state=pending`, publishes after the
   transaction commits, and records `published_once`.

`GET /v1/requests/{request_id}` returns the status copy. There is no HTTP route
for sessions, messages, interrupts or approvals.

`HostResult` from the host conditionally updates the same row. Allowed
transitions:

```text
received      -> host_offline, rejected, host_accepted, succeeded, failed, unknown
host_accepted -> succeeded, failed, unknown
unknown       -> succeeded, failed
```

`host_offline`, `rejected`, `succeeded` and `failed` are final. For control
commands the host first publishes `host_accepted`, then a terminal result.
`received` never means the host accepted. `host_accepted` never means DSH
finished. `unknown` never means success.

Results are read only by `request_id`. The result body stays encrypted for the
channel that produced it; after a reconnect the client reuses the same
`command_id` (host dedupe) or queries `GetCommandResult` to obtain the terminal
payload over the new channel.

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

## HTTP surface

```http
POST   /v1/pairing/invitations        admin bearer or host signature
POST   /v1/pairing/claims              token in body
GET    /v1/hosts                       device bearer token
POST   /v1/hosts/{host_id}/requests     device bearer token
GET    /v1/requests/{request_id}        device bearer token
DELETE /v1/devices/{device_id}          device bearer token (self) or admin
POST   /v1/hosts/{host_id}/devices/{device_id}/revocation  host signature
GET    /healthz
GET    /readyz
GET    /metrics
```

Internal broker callbacks (bound on the internal listener, fixed shared header):

```http
POST /internal/broker/auth
POST /internal/broker/acl
POST /internal/broker/presence
```

HTTP errors use `ErrorResponse` JSON with a stable `ErrorCode` value. Success
bodies are the messages above; `POST /requests` returns `RequestReceipt`.

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

`--json` emits a stable object with at least `schema: "whalerein/v1"`, the
command name, and either `data` or `error: {code, message}`. Explicit conversation
output shows normalized visible content to its requesting user. Diagnostics and
error output never include message bodies, tool arguments, private keys or pairing
tokens. There is no normal raw-native JSON output mode.

## Typed conversations, watches and synchronization

SessionKey is the Host/session pair. EventPayload contains only SessionUpdate:
a watch-scoped HistoryBatch, LiveFrame, SessionState or HistoryReset. There are
no native type strings, data_json payloads or public integer event sequences.

HistoryRead returns a fixed-cut HistoryPage. Cursor tokens belong to their Source,
session and view; clients compare continuity, not numeric/lexical order. Batches
may advance across filtered records without a visible Message. Snapshot pages
stage until the final complete page and never silently replace a complete cache
with a truncated view. Repeated batch IDs with conflicting canonical content are
rejected. Message origin is transient correlation metadata, not a second authority.

LiveFrame carries an epoch, Attempt ID, block ID and inclusive normalized sequence
interval. Start/delta/reset/end are separate typed payloads. TextDelta appends;
reset replaces a provisional block. Adjacent text can coalesce for 20 ms / 8 KiB.
Preview loss marks an incomplete overlay; it never advances the committed cursor.
The final Message links to its origin Attempt and supersedes provisional content.

Core subscribes MQTT before WatchSession and baseline synchronization. Watch IDs
are channel-scoped; leases last 30 seconds. RefreshWatches every five seconds
returns authoritative history watermarks and current state, repairing a lost last
notification even when no later event arrives. Unwatch is best effort; channel
cleanup/lease expiry bounds abandoned resources. Current Core admits one active
watch per instance so device MQTT client identities cannot compete internally.

Approvals and runtime state are current-state projections outside history cursor
arithmetic. Approval execution always revalidates the native pending action.
The complete schema and Source invariants are in
[the Harness contract](../../docs/architecture/harness-contract.md).

## Approval digest

```text
digest = SHA-256(
  "whalerein/v1/approval|{approval_id}|{session_id}|{tool_name}|{call_id}|" +
  "{reason}|{version}|{expires_at_unix_ms}"
)
```

The digest and version live only inside encrypted commands and events. The
client sends them back with `ResolveApproval`; the plugin checks pending state,
expiry, digest, version and task state, then resolves exactly once. Any mismatch
or late resolve returns `approval_expired`, `approval_version_mismatch` or
`approval_already_resolved` and never reaches DSH.

## Logical MQTT transport

Current source uses transport version 2 independently of application protocol
version 1. HostReady advertises `transport_version`; HostRequest carries
`client_transport_version` and `relay_transport_version` for capability checks.
Legacy small envelopes remain raw protobuf; large messages require a capable peer.

Framed packets start with `WRMQTT2` followed by protobuf `TransportFrame`.
A frame identifies the logical topic, transfer ID, index/count, total bytes,
SHA-256 digest, payload, or receipt/rejection. Receivers check bounds and metadata,
reassemble the entire envelope, and verify the digest before application dispatch.
AEAD verification remains the security boundary; transport receipts acknowledge
transfer, not command execution. Part receipts alone do not confirm success: the
sender also requires `complete=true` after whole-envelope integrity verification.

| Data topic | Receipt topic |
|---|---|
| Host `request` | Host `result` |
| Host `result` or `event` | Host `request` |
| Device `event` | Device `ack` |

Senders retry missing receipts within bounded attempts/time. Disconnect clears
partial transfers. Completed-transfer tracking suppresses duplicate delivery;
command IDs and event sequences still enforce application idempotency.

## Transport size budget

| Limit | Current source/configuration |
|---|---|
| Framed transport packet | 64 KiB budget; 60 KiB payload chunks. |
| Logical envelope | 16 MiB maximum. |
| Concurrent inbound reassembly | Four transfers, 32 MiB reserved bytes, 30-second retention. |
| Access default HTTP request body | 24 MiB, allowing base64 expansion and envelope overhead. |
| Compose broker packet ceiling | 1 MiB; larger live overrides are not required by framing. |

Oversized messages fail explicitly; snapshots must not silently become successful
partial results. Keep clients, broker ACLs, and HTTP/reverse-proxy limits aligned.
The previous 4 MiB envelope / 8 MiB HTTP limits do not describe current source.
This source description does not imply that interoperability has been verified.

Transport version 2 is negotiated through `client_transport_version`,
`relay_transport_version`, and HostReady's `transport_version`. Upgrade Access,
the plugin, and clients together. Receipts confirm fragment delivery, not command
execution.
`payload_too_large` is an explicit logical resource-budget failure (CLI exit 5),
not an authorization failure. Transport-v2 requires at least a 64 KiB broker
packet budget and rejects smaller negotiated budgets before sending frames.
