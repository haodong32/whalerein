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

CLI account authentication uses the provider's OAuth device authorization
grant. The short-lived, audience-scoped access token and rotating refresh
token are bearer credentials kept in the profile's SecretStore. Possession
of a valid access token is sufficient to read the account's CLI directory
until that token expires. This account session does not grant regional
Access authority or replace the device proofs used for pairing.

Profile state is isolated by local directory and device identity; account
credentials, device keys, pins, cache and pending recovery do not cross
profiles. Staging selection is a local testing choice, not server access
control. Official staging is hidden from ordinary setup and must be explicitly
added with the testing flag; it never auto-activates or acts as a fallback, and
its active output stays labeled. A profile switch refuses while another client
can still send traffic. Older binaries ignore the profile registry, so managed
selections are not rollback-compatible without stopping those clients.

Access/broker see routing IDs, timing and sizes, not session plaintext or channel
keys. Endpoint/OS compromise is outside this protection. Cached client payloads
are encrypted; memory/file secret backends are test tools. Compose plaintext
loopback listeners require separate production TLS configuration.

## Configuration and trust

The client-environment file is non-secret. It contains environment names,
public origins, native registration ids and CA references. Managed route fields
(`access_url`, `mqtt_url`, broker/region keys) and credential-shaped fields are
rejected as `unknown_field` rather than stored. Secrets stay in Core
`SecretStore` and in Host native credential records under their existing names;
configuration never reads, copies, moves or rekeys them.

TLS trust is scoped to the selected environment: the platform store, the fixed
per-user mkcert root for built-in `local`, or an explicit public CA file. The
configuration path never installs a CA, copies a CA private key, runs a shell
helper, or changes a process-global TLS store. A missing or wrong CA fails with
`invalid_tls` and does not change the selected environment. Existing Standalone
`WHALEREIN_CA_FILE` and `WHALEREIN_MQTT_URL` transport overrides are separate
and outside this contract.

Legacy import is read-only until confirmation. Preview reports literal secrets
only as a conflict or secure-handoff requirement and never prints their values;
it retains identity and state by path, profile id and credential namespace, and
never moves, copies or rekeys a profile or credential. The import journal and
the `<config>.lock.sqlite` sidecar are non-secret coordination state: they
record revisions, paths, names and step timestamps, never tokens or private
keys, and a normal configuration open never reads them.

## Pairing and revocation

Core confirms the Noise host fingerprint before FINISH. Invitations are
host-bound, short-lived, single-use and failure-limited. The Host writes the
grant at COMMIT and acknowledges its exact opaque grant ID; Access admits the
Device only after that acknowledgement and keeps the ID on the durable outcome.
Core generates the transport credential before FINISH and binds its hash into
the encrypted payload. Access stores only hashes and a token-free operation
receipt. A confirmed pending FINISH is retained in SecretStore and can be
resumed after restart within its validity window; changed keys, changed requests
and revoked generations fail.

The grant identity is bound into the secure channel. The Device names the grant
it expects in `ChannelHello`; the Host snapshots the one active grant for the
pinned device key at message 1 and installs the channel only for that same grant
at message 3, echoing it in `ChannelReady`. A non-empty `ChannelHello.grant_id`
that differs from the snapshot fails the handshake closed: no channel, no data
and no action. An empty value is accepted only as a legacy-Client compatibility
path and never relaxes the Host snapshot or the finish re-check. A retried
FINISH replays the durable Host grant ID without a new Host round trip, and an
old Host that reports no grant leaves the response field empty.

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

Three lifetimes are deliberately distinct. Losing an observation connection or
replacing its secure channel does not by itself revoke authorization or cancel a
separately live HTTP answer. Exact request-lease expiry closes renewal and
forwarding for that one request and schedules its exact Host cancellation, but it
is not proof that no native action occurred: a confirmed receipt or conservative
`unknown` outcome remains recoverable, and no path retries the answer
automatically. Applicable grant/device revocation fences every affected
observation and execution regardless of a fresh heartbeat. Cancellation notices
carry bounded host/device/request/command/digest/deadline metadata and no question
or answer content.

Revocation clears client pairing/cache but retains key identity; explicit `forget`
requires no active pairing. Local file deletion does not revoke remote grants.
For lost credentials, revoke the confirmed old device ID through host/admin
access. Automatic host-key rotation has no documented procedure.

## Diagnostics

Logs/exports exclude message bodies, tool parameters, drafts, private keys,
invitation URIs/tokens, device tokens, full ciphertext, Host/device identities
and directory names. Operator-facing session output is separate.

Core captures one allowlisted, redacted report per explicit capture. Preview,
the saved file and a platform share action consume those same UTF-8 bytes; the
preview only reads local state and never writes a file or uploads. A failed
capture returns an error and exposes no file or stale share action. Only an
explicit refresh creates a new capture.

A new local CLI export path is created exclusively with owner-only (0600)
permissions in one action, including noninteractive use without `--yes`.
Replacing an existing regular file needs affirmative overwrite consent (`--yes`
or an explicit interactive answer); refusal, missing noninteractive consent, a
symbolic link, a directory or another unsafe target leaves the existing content
untouched. A file that appears after the new-target check is preserved unless
replacement was explicitly authorized, and a confirmed replacement is staged so
a failed write keeps the prior contents. The CLI never follows a link. On
mobile, the explicit Share action opens one system destination picker for the
inspected bytes and cleans up its private temporary copy afterwards. Neither
local save nor preview sends a network report.

Security changes need failure tests for affected keys, pairing/grants, AEAD,
approvals, ACLs and replay rules. Transport hashes do not replace AEAD. Exact
algorithms/framing belong in the [wire contract](../proto/v1/WIRE.md). Keep
verification records under `.local-docs/acceptance/`.
