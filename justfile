install:
  git submodule update --init
  sudo apt-get install --yes \
    autoconf \
    automake \
    build-essential \
    cmake \
    libboost-dev \
    libevent-dev \
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
  #!/usr/bin/env bash
  CLI="./bitcoin/build/bin/bitcoin-cli -datadir=./copr -signet"
  MINER="./bitcoin/contrib/signet/miner"
  GRIND="./bitcoin/build/bin/bitcoin-util grind"
  ADDR=tb1q73me2ten2cwphzdpl60js6p0vgex8c2e5fqm6m
  NBITS=1d00ffff
  $CLI loadwallet copr || $CLI createwallet copr
  HEIGHT=$($CLI getblockcount)
  while [ $HEIGHT -lt 16 ]; do
    $MINER --cli="$CLI" generate --grind-cmd="$GRIND" --address="$ADDR" --nbits=$NBITS
    HEIGHT=$($CLI getblockcount)
  done

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
