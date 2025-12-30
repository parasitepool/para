set positional-arguments

watch +args='test':
  cargo watch --clear --exec '{{args}}'

fmt:
  cargo fmt --all

clippy:
  cargo clippy --all --all-targets -- --deny warnings

ci: clippy
  cargo fmt -- --check
  cargo test --all

ignored:
  cargo test --all -- --ignored

all: ci ignored

outdated:
  cargo outdated --root-deps-only --workspace

unused:
  cargo +nightly udeps --workspace

doc:
  cargo doc --package para --open

audit:
  cargo audit

coverage:
  # cargo tarpaulin --engine llvm
  # cargo llvm-cov -- --include-ignored
  cargo llvm-cov --html --open

test-without-ckpool:
  cargo test --all -- --skip ckpool

miner stratum_endpoint='127.0.0.1:42069': 
  cargo run --release -- miner \
    {{stratum_endpoint}} \
    --username bc1p4r54k6ju6h92x8rvucsumg06nhl4fmnr9ecg6dzw5nk24r45dzasde25r3.tick \
    --password x \
    --cpu-cores 2

miner-signet: 
  cargo run --release -- miner \
    127.0.0.1:42069 \
    --username tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.tick \
    --password x \
    --cpu-cores 2 \
    --throttle 500K

ping host='parasite.wtf':
  cargo run ping {{host}}:42069

ping-auth host='parasite.wtf' username='bc1p4r54k6ju6h92x8rvucsumg06nhl4fmnr9ecg6dzw5nk24r45dzasde25r3.tick' password='x':
  cargo run ping --username {{username}} --password {{password}} {{host}}:42069

pool: 
  RUST_LOG=info cargo run -- pool \
    --api-port 8080 \
    --chain signet \
    --address 0.0.0.0 \
    --bitcoin-rpc-username satoshi \
    --bitcoin-rpc-password nakamoto \
    --bitcoin-rpc-port 38332 \
    --start-diff 0.00001 \
    --vardiff-window 10 \
    --vardiff-period 1 \
    --zmq-block-notifications tcp://127.0.0.1:28332

pool-mainnet: 
  cargo run --release -- pool \
    --chain mainnet \
    --address 0.0.0.0 \
    --bitcoin-rpc-username satoshi \
    --bitcoin-rpc-password nakamoto \
    --bitcoin-rpc-port 8332 \
    --start-diff 999 \
    --vardiff-window 300 \
    --vardiff-period 5 \
    --zmq-block-notifications tcp://127.0.0.1:28333

server: 
  RUST_LOG=info cargo run -- server \
    --log-dir copr/logs \
    --port 8080

harness: build-bitcoind
  cargo run -p harness

install:
  git submodule update --init --recursive
  sudo apt-get install --yes \
    autoconf \
    automake \
    build-essential \
    capnproto \
    clang-format \
    cmake \
    libboost-dev \
    libcapnp-dev \
    libevent-dev \
    libpq-dev \
    libsqlite3-dev \
    libtool \
    libzmq3-dev \
    pkgconf \
    python3 \
    yasm \

build-bitcoind: install
  #!/usr/bin/env bash
  cd bitcoin
  cmake -B build -DWITH_ZMQ=ON
  cmake --build build -j 21

build-ckpool: install
  #!/usr/bin/env bash
  cd ckpool
  ./autogen.sh
  ./configure
  make

build: build-bitcoind build-ckpool

bitcoind:
  #!/usr/bin/env bash
  ./bitcoin/build/bin/bitcoind \
    -datadir=copr \
    -signet 

mine:
  ./bin/mine

ckpool:
  #!/usr/bin/env bash
  cd ckpool
  make 
  cd ..
  ./ckpool/src/ckpool \
    -B \
    -k \
    --config copr/ckpool.conf \
    --sockdir copr/tmp \
    --loglevel 7 \
    --log-shares \
    --signet \
    --log-txns

lint:
  find ./ckpool/src -type f \( -name "*.c" -o -name "*.h" \) -not -path "**/jansson-2.14/*" -exec clang-format -i {} \;

test: lint
  ./bin/run_tests

psql:
  ./bin/postgres-init

psql-reset:
  ./bin/postgres-reset

prepare-release revision='master':
  #!/usr/bin/env bash
  set -euxo pipefail
  git checkout {{ revision }}
  git pull origin {{ revision }}
  echo >> CHANGELOG.md
  git log --pretty='format:- %s' >> CHANGELOG.md
  $EDITOR CHANGELOG.md
  $EDITOR Cargo.toml
  version=`sed -En 's/version[[:space:]]*=[[:space:]]*"([^"]+)"/\1/p' Cargo.toml | head -1`
  cargo check
  git checkout -b release-$version
  git add -u
  git commit -m "Release $version"
  gh pr create --web

publish-release revision='master':
  #!/usr/bin/env bash
  set -euxo pipefail
  rm -rf tmp/release
  git clone git@github.com:parasitepool/para.git tmp/release
  cd tmp/release
  git checkout {{ revision }}
  cargo publish
  cd ../..
  rm -rf tmp/release
