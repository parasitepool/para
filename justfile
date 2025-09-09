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
  cargo test --all -- --ignored

outdated:
  cargo outdated --root-deps-only --workspace

unused:
  cargo +nightly udeps --workspace

doc:
  cargo doc --workspace --open

miner: 
  RUST_LOG=info cargo run --release -- miner \
    --host 127.0.0.1 \
    --port 42069 \
    --username bc1p4r54k6ju6h92x8rvucsumg06nhl4fmnr9ecg6dzw5nk24r45dzasde25r3.tick \
    --password x

miner-signet: 
  RUST_LOG=info cargo run --release -- miner \
    --host 127.0.0.1 \
    --port 42069 \
    --username tb1qkrrl75qekv9ree0g2qt49j8vdynsvlc4kuctrc.tick \
    --password x

ping host='parasite.wtf':
  cargo run ping {{host}}:42069

ping-auth host='parasite.wtf' username='bc1p4r54k6ju6h92x8rvucsumg06nhl4fmnr9ecg6dzw5nk24r45dzasde25r3.tick' password='x':
  cargo run ping --username {{username}} --password {{password}} {{host}}:42069

pool: 
  RUST_LOG=info cargo run -- pool \
    --chain signet \
    --address 0.0.0.0 \
    --bitcoin-rpc-username satoshi \
    --bitcoin-rpc-password nakamoto

server: 
  RUST_LOG=info cargo run -- server \
    --log-dir copr/logs \
    --port 8080

install:
  git submodule update --init
  sudo apt-get install --yes \
    autoconf \
    automake \
    build-essential \
    clang-format \
    cmake \
    libboost-dev \
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
  cmake -B build
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
    --sockdir copr/tmp
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
