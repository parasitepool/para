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

bitcoind:
  #!/usr/bin/env bash
  ./bitcoin/build/bin/bitcoind \
    -datadir=copr \
    -signet

pool:
  cargo run --features reload -- \
    pool \
    --chain signet \
    --address 0.0.0.0 \
    --bitcoin-rpc-username satoshi \
    --bitcoin-rpc-password nakamoto \
    --bitcoin-rpc-port 38332 \
    --http-port 8080 \
    --start-diff 0.00001 \
    --vardiff-window 10 \
    --vardiff-period 1 \
    --zmq-block-notifications tcp://127.0.0.1:28332

proxy:
  cargo run --features reload -- \
    proxy \
    --chain signet \
    --bitcoin-rpc-username satoshi \
    --bitcoin-rpc-password nakamoto \
    --bitcoin-rpc-port 38332 \
    --address 0.0.0.0 \
    --port 42070 \
    --username tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.proxy \
    --password x \
    --http-port 8081 \
    --start-diff 0.00001 \
    --vardiff-window 10 \
    --vardiff-period 1 \
    --upstream localhost:42069 

# Mine to anyone-can-spend P2WSH(OP_TRUE)
miner port='42069': 
  cargo run --release -- miner \
    127.0.0.1:{{port}} \
    --username tb1qft5p2uhsdcdc3l2ua4ap5qqfg4pjaqlp250x7us7a8qqhrxrxfsqaqh7jw.tick \
    --password x \
    --cpu-cores 2 \
    --throttle 500K

miner-mainnet stratum_endpoint='127.0.0.1:42069': 
  cargo run --release -- miner \
    {{stratum_endpoint}} \
    --username bc1p4r54k6ju6h92x8rvucsumg06nhl4fmnr9ecg6dzw5nk24r45dzasde25r3.tick \
    --password x \
    --cpu-cores 2

pool-mainnet: 
  cargo run -- pool \
    --http-port 8080 \
    --chain mainnet \
    --address 0.0.0.0 \
    --bitcoin-rpc-username satoshi \
    --bitcoin-rpc-password nakamoto \
    --bitcoin-rpc-port 8332 \
    --start-diff 999 \
    --vardiff-window 300 \
    --vardiff-period 5 \
    --zmq-block-notifications tcp://127.0.0.1:28333

harness: build-bitcoind
  cargo run -p harness -- spawn

flood:
  cargo run -p harness -- flood

flood-continuous:
  cargo run -p harness -- flood --continuous 5000000

mempool:
  docker compose -f copr/mempool/docker-compose.yml up

mempool-down:
  docker compose -f copr/mempool/docker-compose.yml down -v --remove-orphans

server: 
  RUST_LOG=info cargo run --features swagger-ui -- server \
    --log-dir copr/logs \
    --port 8080

openapi:
  cargo run --example openapi > openapi.json

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
