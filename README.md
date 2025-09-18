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

## Contributing

"If I had more time, I would have written you a shorter letter." - Mark Twain

## Hermit Environment

A full development/build environment is bundled using hermit and can be activated as follows:
```
. ./bin/activate-hermit
```


## Building the docs

```
cargo install mdbook mdbook-linkcheck
just build-docs
just serve-docs
```

Then you can customize CSS and javascript by following [this
guide](https://github.com/rust-lang/mdBook/tree/master/guide/src/format/theme).
