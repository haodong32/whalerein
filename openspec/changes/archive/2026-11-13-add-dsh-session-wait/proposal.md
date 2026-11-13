# Native session waiting

## Why

Remote callers need the same stopping semantics as DSH: native quiescence, pending human input, or a deadline. Sampled `running=false` and queue length cannot reproduce `agent.whenIdle()`.

## What Changes

- Add a typed read-only wait operation through the existing encrypted query path. Only the DSH adapter calls native `sessionController.wait(request, signal)`.
- Core owns the overall monotonic deadline, transport recovery and cancellation. The source owns the stopping decision; the adapter also accounts for active input requests already claimed by WhaleRein and therefore absent from the native gateway's pending-event list.
- Return `idle`, `input_required`, or `timeout` with scoped state. Default CLI timeout exits 0; `--fail-on-timeout` exits 7, preserving the same typed observation in both modes.
- Use the native 30-second default and supported positive integer-millisecond bounds. Derive each remote wait slice from the existing request lifetime rather than adding a product-level 300-second cap.
- Keep history/tail APIs separate. Native wait internally reads bounded assistant output; the adapter discards that output for this state-only public API and must account for its cost, errors and cancellation.

## Capabilities

### New Capabilities

- `session-wait`: native-backed, read-only waiting and normalized pending-input state.

### Modified Capabilities

None. Existing local specs are planning context, not proof of implementation.

## Impact

Extend the source adapter, Host query handling, typed protocol, Rust Core and CLI. No new service, database, server task queue, Core injection boundary or public native cursor. Shared input types can be implemented with either wait or questions; neither feature requires the other's full implementation. See [design and decisions](design.md), [testing policy](../../../docs/testing.md), and [Harness boundary](../../../docs/architecture/harness-contract.md).
