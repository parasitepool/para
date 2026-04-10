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

It implements a Rust library for the Stratum protocol and includes helpful
command-line tools that measure ping, inspect block templates, mimic mining
machines and run pool logic. To see a full list of available commands just
follow the instructions below and do `para help`.

This repository includes a modified fork of
[ckpool](https://bitbucket.org/ckolivas/ckpool/src/master/), which currently
runs on `parasite.wtf:42069`. For instructions on how to connect, please visit
[parasite.space](https://parasite.space?help). The modifications to the C
codebase of ckpool are:

- postgres database for share logging 
- custom coinbase [logic](https://zkshark.substack.com/p/parasite-pool-igniting-the-mining).
- support for signet
- miscellaneous helpful config flags

Setup
-----

### Requirements:

* [Rust](https://rust-lang.org/tools/install/)
* [Just](https://github.com/casey/just?tab=readme-ov-file#installation) (optional)

#### Manual Install

Rust is required to build, test, and develop. To install with `curl`:

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

`para` requires:
- `rustc` version 1.90.0 or later
  - Available from `rustup update`
- `gcc` version 10.5.0 or later.
  - Available from your package manager or [gnu.org](https://gcc.gnu.org)

These versions can be verified with:
```shell
rustc --version
gcc --version
```

#### Linux Builds
To compile software on Linux systems, you may need additional packages which
are not always installed by default. For Debian based systems (Ubuntu, Kali,
etc), you can install these dependencies with `apt`:
```
sudo apt install build-essential pkg-config libssl-dev
```

#### Windows Builds
To build Rust programs on Windows, you need one of two ABI configurations:
  1. MSVC
     - On Windows, `rustup` will configure Rust to target this ABI by default
     - [Visual Studio](https://visualstudio.microsoft.com/downloads/) with
     [Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)
     is required for building with MSVC
       - this can be a very large install (~4GB)
  2. GNU (GCC)
     - Available from the `rustup toolchain install stable-gnu` command
     - Requires [MinGW/MSYS2](https://www.msys2.org/)
       ```
       # Run from within the MSYS terminal
       pacman -S --needed base-devel mingw-w64-ucrt-x86_64-toolchain \
       mingw-w64-ucrt-x86_64-nasm
       ```

[The Rustup Book](https://rust-lang.github.io/rustup/installation/windows.html)
provides more details on Windows builds.
