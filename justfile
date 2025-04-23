set positional-arguments

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
    --loglevel 7 \
    --log-shares

lint:
  find ./ckpool/src -type f \( -name "*.c" -o -name "*.h" \) -not -path "**/jansson-2.14/*" -exec clang-format -i {} \;

test: lint
  ./bin/run_tests

psql:
  ./bin/postgres-init

psql-reset:
  ./bin/postgres-reset

setup branch remote chain domain:
  ssh root@{{domain}} '\
    export DEBIAN_FRONTEND=noninteractive \
    && mkdir -p deploy \
    && apt-get update --yes \
    && apt-get upgrade --yes \
    && apt-get install --yes git rsync'
  rsync -avz deploy/checkout root@{{domain}}:deploy/checkout
  ssh root@{{domain}} 'cd deploy && ./checkout {{branch}} {{remote}} {{chain}} {{domain}}'

deploy branch remote chain domain: \
  (setup branch remote chain domain) 
  ssh root@{{domain}} 'cd deploy/{{remote}} && ./deploy/deploy-bitcoind'
  ssh root@{{domain}} 'cd deploy/{{remote}} && ./bin/postgres-init'
  ssh root@{{domain}} 'cd deploy/{{remote}} && ./deploy/deploy-ckpool'
  ssh root@{{domain}} 'cd deploy/{{remote}} && ./deploy/deploy-para'

deploy-bitcoind branch remote chain domain: \
  (setup branch remote chain domain)
  ssh root@{{domain}} 'cd deploy{{remote}} && ./deploy/deploy-bitcoind'

deploy-postgres branch remote chain domain: \
  (setup branch remote chain domain)
  ssh root@{{domain}} 'cd deploy{{remote}} && ./bin/postgres-init'

deploy-ckpool branch remote chain domain: \
  (setup branch remote chain domain)
  ssh root@{{domain}} 'cd deploy{{remote}} && ./deploy/deploy-ckpool'

deploy-para branch remote chain domain: \
  (setup branch remote chain domain)
  ssh root@{{domain}} 'cd deploy{{remote}} && ./deploy/deploy-para'

tunnel server='zulu.parasite.dev':
  ssh -N -L 5433:127.0.0.1:5432 {{server}}
