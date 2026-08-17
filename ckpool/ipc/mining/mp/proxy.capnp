# Minimal stub of libmultiprocess /mp/proxy.capnp
# Goal: allow capnpc to compile Bitcoin Core's mining.capnp for *client-only* use.

@0x8e8e8e8e8e8e8e8e;

using Cxx = import "/capnp/c++.capnp";
$Cxx.namespace("mp");

struct Context {
  thread @0 :UInt64;
}

annotation wrap(ann :Text) :Void $Cxx.name("wrap");
annotation include(ann :Text) :Void $Cxx.name("include");
annotation includeTypes(ann :Text) :Void $Cxx.name("includeTypes");
annotation name(ann :Text) :Void $Cxx.name("name");