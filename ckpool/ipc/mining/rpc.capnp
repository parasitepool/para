# Local copy for ckpool IPC shim, based on Bitcoin Core IPC protocol.
# This is a minimal schema for the Rpc interface described in ipc-protocol.md.

@0x9f2e5a1c3b8d4e7f;

using Cxx = import "/capnp/c++.capnp";
$Cxx.namespace("ipc::capnp::messages");

using Proxy = import "/mp/proxy.capnp";

interface Rpc $Proxy.wrap("interfaces::Rpc") {
    executeRpc @0 (context :Proxy.Context, request :Text, uri :Text, user :Text) -> (result :Text);
}
