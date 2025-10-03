<h1 align=center><code>para</code></h1>

<div align=center>
  <a href=https://crates.io/crates/para>
    <img src=https://img.shields.io/crates/v/para.svg alt="crates.io version">
  </a>
  <a href=https://github.com/parasitepool/para/actions/workflows/ci.yaml>
    <img src=https://github.com/parasitepool/para/actions/workflows/ci.yaml/badge.svg alt="build status">
  </a>
  <a href=https://github.com/parasitepool/para/releases>
    <img src=https://img.shields.io/github/downloads/parasitepool/para/total.svg alt=downloads>
  </a>
</div>
<br>

`para` is a command-line tool for miners and pools. It is experimental
software with no warranty. See [LICENSE](LICENSE) for more details.

This repository includes a modified fork of
[ckpool](https://bitbucket.org/ckolivas/ckpool/src/master/), which currently
runs on `parasite.wtf:42069`. For instructions on how to connect, please visit
[parasite.space](https://parasite.space?help).

In addition to adding a postgres database for share logging and some helpful
flags it modifies the coinbase payout logic found in `stratifier.c`. For more
information go
[here](https://zkshark.substack.com/p/parasite-pool-igniting-the-mining).

```c 
// Generation value
g64 = COIN;
d64 = wb->coinbasevalue - COIN;
wb->coinb2bin[wb->coinb2len++] = 2 + wb->insert_witness;

u64 = (uint64_t*)&wb->coinb2bin[wb->coinb2len];
*u64 = htole64(g64);
wb->coinb2len += 8;

/* Coinb2 address goes here, takes up 23~25 bytes + 1 byte for length */

wb->coinb3len = 0;
wb->coinb3bin = ckzalloc(256 + wb->insert_witness * (8 + witnessdata_size + 2));

if (ckp->donvalid && ckp->donation > 0) {
    u64 = (uint64_t*)wb->coinb3bin;
    *u64 = htole64(d64);
    wb->coinb3len += 8;

    wb->coinb3bin[wb->coinb3len++] = sdata->dontxnlen;
    memcpy(wb->coinb3bin + wb->coinb3len, sdata->dontxnbin, sdata->dontxnlen);
    wb->coinb3len += sdata->dontxnlen;
}
```

`para` is more than just glue code around ckpool though. It implements a Rust
library for the Stratum protocol and includes helpful command-line tools that
measure ping, inspect block templates and mimic mining machines. To see a full
list of available commands just follow the instructions below and do `para
help`.

Setup
-----

### Requirements:

* [Rust](#manual-install)
* [Just](https://github.com/casey/just?tab=readme-ov-file#installation)

#### Manual Install

Rust is required to build, test, and develop. To install: 

``` 
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh 
```

#### Bundled Environment

This repo includes a bundled development environment with
[Hermit](https://github.com/cashapp/hermit) that provides the above
requirements. 

```
. ./bin/activate-hermit
```

Build
-----

Clone the `para` repo:

```
git clone https://github.com/parasitepool/para.git
cd para
```

To build a specific version of `para`, first checkout that version:

```
git checkout <VERSION>
```

And finally to actually build `para`:

```
cargo build --release
```

Once built, the `para` binary can be found at `./target/release/para`.

You can also install `para` directly into your path by doing:

```
cargo install --path .
```

Troubleshooting
---------------

### Build Issues

#### Verify Minimum Versions

`para` requires
- `rustc` version 1.90.0 or later
  - Available from `rustup update`
- `gcc` version 10.5.0 or later.
  - Available from your package manager or [gnu.org](https://gcc.gnu.org)

These versions can be verified with:
```shell
rustc --version
gcc --version
```
