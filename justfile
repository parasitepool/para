alpha := 'root@alpha.parasite.dev'
bravo := 'root@bravo.parasite.dev'

install:
  git submodule update --init
  sudo apt-get install --yes \
    autoconf \
    automake \
    build-essential \
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

psql:
  ./bin/postgres-init

psql-reset:
  ./bin/postgres-reset

deploy branch remote chain domain:
  ssh root@{{domain}} '\
    export DEBIAN_FRONTEND=noninteractive \
    && mkdir -p deploy \
    && apt-get update --yes \
    && apt-get upgrade --yes \
    && apt-get install --yes git rsync'
  rsync -avz deploy/checkout root@{{domain}}:deploy/checkout
  ssh root@{{domain}} 'cd deploy && ./checkout {{branch}} {{remote}} {{chain}} {{domain}}'

deploy-signet branch='master' remote='parasitepool/pool': \
  (deploy branch remote 'signet' 'alpha.parasite.dev')

tunnel server='alpha':
  ssh -N -L 5433:127.0.0.1:5432 {{alpha}}

lint:
  find ./ckpool/src -type f \( -name "*.c" -o -name "*.h" \) -not -path "**/jansson-2.14/*" -exec clang-format -i {} \;