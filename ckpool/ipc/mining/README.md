# Bitcoin Core mining IPC shim

This directory contains a small C++ shim that lets the (otherwise pure C)
ckpool codebase talk to a Bitcoin Core `bitcoin-node` over its Cap'n Proto
mining IPC interface (`interfaces::Mining`, obtained via `Init.makeMining`).
The protocol is described in `../../ipc-protocol.md`.

## Layout

- `*.capnp` — the Bitcoin Core IPC schemas (`common`, `mining`, `init`, `echo`,
  `rpc`) plus `mp/proxy.capnp` (the libmultiprocess proxy schema), vendored
  verbatim.
- `*.capnp.h` / `*.capnp.c++` — the client stubs generated from the schemas with
  `capnpc -o c++` (Cap'n Proto 1.1.0). Checked in so a `capnpc` is not required
  at build time.
- `mining_ipc.h` — the pure C API exposed to ckpool.
- `mining_ipc.cpp` — the C++ implementation.

## Transport

The shim talks to the socket directly with capnp's `EzRpcClient` and the
generated stubs, performing the libmultiprocess handshake by hand: it exchanges
thread maps via `Init.construct()` and supplies a `Proxy.Context{thread,
callbackThread}` on every `Mining` call. This deliberately avoids a build
dependency on libmultiprocess itself; only `capnp-rpc` is required.

An `EzRpcClient` runs its kj event loop on the thread that constructs it, and
every call on it must be made from that same thread. A context is therefore
connected and used entirely from one caller thread — in ckpool that is the
`ipcnotify` thread in the stratifier.

## Build

The shim is compiled into `ckpool` only when `configure` detects `capnp-rpc`
(`HAVE_CAPNP`). Everything in `mining_ipc.h`/`.cpp` and its callers in
`stratifier.c` is guarded by `#ifdef HAVE_CAPNP`, so ckpool still builds and
runs as before when Cap'n Proto is absent.

## Status

### Tip notification

`mining_ipc.h` exposes `connect`, `disconnect`, `get_tip`, `wait_tip_changed`,
and `try_obtain_mining`. When the `btcmining` config option points at an
existing socket, the stratifier drives block updates from
`Mining.waitTipChanged` instead of ZMQ (with the block-poll thread still
acting as a backstop).

### Template service (workgen / submit)

`mining_ipc_service_connect` opens a second connection with its own service
thread. Callers may invoke `create_new_block`, template getters, and
`submit_solution` from any thread; calls are marshalled onto that thread.
ckpool stores this as `btc_template_svc` whenever a valid `ipcmining` socket
is configured.

### Phase 2 start — `checkBlock` (SV2 Job Declaration validation)

`mining_ipc_check_block` binds Cap'n Proto `Mining.checkBlock` with
`BlockCheckOptions` (`check_merkle_root`, `check_pow`). This is the normative
primitive for validating a fully serialised candidate block before accepting
custom work (not `testmempoolaccept`).

For JD, ckpool opens a **third** connection (`btc_validation_svc`) so a slow
`checkBlock` cannot head-of-line-block workgen/submit marshalling. Use
`mining_check_opts_template()` (PoW off, merkle on) for unpaid templates.

Next Phase 2 steps: assemble candidate blocks from `DeclareMiningJob`, call
`mining_ipc_check_block` on `btc_validation_svc`, then
`SetCustomMiningJob` / pool-first switch.
