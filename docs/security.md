# Security

This page describes Standalone mode. The
[Access design](architecture/access-evolution.md) specifies later external identity
trust, account-authorized pairing, regional service authentication and relay
ownership; these are not implemented security guarantees.

## Trust and storage

| Boundary/material | Protection / authority |
|---|---|
| Core ↔ plugin | Noise XX authenticates keys and encrypts application payloads. |
| Public HTTPS/MQTT | TLS; deployment configures endpoints/trust. |
| Device ↔ Access | Transport token and live host grant. |
| Host ↔ broker | Deployment provisioning credential. |
| Plugin ↔ DSH | Adapter owns API/credential access; DSH executes actions. |
| Client private key, token, cache key | Core SecretStore; OS keyring by default. |
| Host private key | DSH credentials through the adapter. |
| Access device/invitation credentials | Hashes in PostgreSQL. |
| Deployment secrets | Configuration outside Git. |

Access/broker see routing IDs, timing and sizes, not session plaintext or channel
keys. Endpoint/OS compromise is outside this protection. Cached client payloads
are encrypted; memory/file secret backends are test tools. Compose plaintext
loopback listeners require separate production TLS configuration.

## Pairing and revocation

Core confirms the Noise host fingerprint before FINISH. Invitations are
host-bound, short-lived, single-use and failure-limited. Access creates the grant
after FINISH and completes only after Host activation. Core generates the
transport credential before FINISH and binds its hash into the encrypted payload.
Access stores only hashes and a token-free operation receipt. A confirmed pending
FINISH is retained in SecretStore and can be resumed after restart within its
validity window; changed keys, changed requests and revoked generations fail.

Core persists the confirmed Host fingerprint and checks it on each fresh channel
before sending Noise message 3. A mismatched key preserves local authorization
and pending recovery metadata but cannot open a channel. Existing paired profiles
without a confirmed pin require explicit revocation and re-pairing; they are not
silently trusted. Pending controls are bound to their original device, Access
endpoint and confirmed Host identity, so later profile changes cannot recover a
different principal's command.

Global revocation affects all device grants and deletes all its credential hashes.
Host settings revokes one grant; credentials remain usable for other live grants,
and are removed when the last grant is revoked. Later re-enrollment cannot revive
those deleted credentials.
Access commits first, disconnects the device and delivers applicable notices.
Hosts wait for replay completion before new channels. Reconnect uses fresh keys.

Revocation clears client pairing/cache but retains key identity; explicit `forget`
requires no active pairing. Local file deletion does not revoke remote grants.
For lost credentials, revoke the confirmed old device ID through host/admin
access. Automatic host-key rotation has no documented procedure.

## Diagnostics

Logs/exports exclude message bodies, tool parameters, private keys, invitation
URIs/tokens and full ciphertext. Operator-facing session output is separate.
Export requires preview and confirmation.

Security changes need failure tests for affected keys, pairing/grants, AEAD,
approvals, ACLs and replay rules. Transport hashes do not replace AEAD. Exact
algorithms/framing belong in the [wire contract](../proto/v1/WIRE.md). Keep
verification records under `.local-docs/acceptance/`.
