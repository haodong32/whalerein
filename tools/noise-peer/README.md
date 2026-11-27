# noise-peer

A small Rust peer for WhaleRein's Noise XX interoperability checks. It is a
test tool, not a WhaleRein client.

```bash
cargo build -p noise-peer
node tools/noise-peer/interop.mjs target/debug/noise-peer
```

The Node script uses the shared Host's `host/runtime/src/security/noise.js` responder helper
and the Rust binary as the other participant. It checks:

- `Noise_XX_25519_ChaChaPoly_SHA256` handshake with the WhaleRein channel
  prologue,
- SHA-256 fingerprint of the remote static key,
- Noise Split `c2h` / `h2c` application keys (a non-reversible split
  fingerprint is printed on the `ready` line; raw keys are never logged),
- one ChaCha20-Poly1305 record in each direction with a request AAD and the
  v2 monotonic record-counter nonce.

The binary also accepts `initiator` and can be driven by other peers. Its line
protocol is documented at the top of `src/main.rs`; `ready` is
`ready <fingerprint> <handshake-hash> <channel-id> <split-fingerprint>`.

The Core dependency opts into `protocol-testing` and imports only its explicit
`testing` exports. `tests/integration/drivers/noise/rust-peer/` is a different,
standalone peer: it validates fixed transcripts independently of Core. Keeping
that independent implementation is intentional.
