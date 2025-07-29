# Parasite Pool

## Stratum Messages

|Method|Implemented|Tested|
|------|-----------|------|
|mining.notify|[x]|[x]|
|mining.submit|[x]|[x]|
|mining.set_difficulty|[x]|[x]|
|mining.authorize|[x]|[x]|
|mining.subscribe|[x]|[x]|
|mining.get_transactions|[ ]|[ ]|
|client.reconnect|[ ]|[ ]|
|client.show_message|[ ]|[ ]|
|mining.set_extranonce|[ ]|[ ]|


## Local development
```
just build
just bitcoind
just mine
just psql
just ckpool
cd para
just run
```

## Hermit Environment

A full development/build environment is bundled using hermit and can be activated as follows:
```
. ./bin/activate-hermit
```
