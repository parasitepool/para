Developer Quick Start
===
---

First Step
----------
Install dependencies/tools

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
--------

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

`para` requires `rustc` version 1.90.0 or later. Run `rustc --version` to ensure
you have this version. Run `rustup update` to get the latest stable release.


Install
--------

Complete the instructions in [Build](#build) and then either:
#### Install with Cargo
```shell
cargo install --path .
```
#### Install Manually
```shell
cp ./target/release/para /usr/local/bin/para
```

Build (Docs)
-----------------

```
cargo install mdbook mdbook-linkcheck
just build-docs
just serve-docs
```

Then you can customize CSS and javascript by following [this
guide](https://github.com/rust-lang/mdBook/tree/master/guide/src/format/theme)
and doing:

```
just init-mdbook-theme
```

This will create the default `mdbook` layout and CSS files inside
`docs/tmp/theme`, which you can then pick, chose and adapt and then copy into
`docs/theme` to tweak the defaults.

