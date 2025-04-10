build-bitcoind:
  #!/usr/bin/env bash
  git submodule init
  git submodule update
  cd bitcoin
  sudo apt-get install build-essential cmake pkgconf python3 libevent-dev libboost-dev libsqlite3-dev libzmq3-dev
  cmake -B build
  cmake --build build -j 21

build-ckpool:
  #!/usr/bin/env bash
  cd ckpool
  sudo apt-get install build-essential yasm autoconf automake libtool libzmq3-dev pkgconf
  ./autogen.sh
  ./configure
  make

build-ckstats:
  #!/usr/bin/env bash
  cd ckstats
  pnpm install
  DATABASE_URL="postgresql://username:password@server:port/your_database_name"
  SHADOW_DATABASE_URL="postgresql://username:password@server:port/your_shadow_database_name"
  API_URL="http://192.168.0.197"

build: build-bitcoind build-ckpool

bitcoind:
  #!/usr/bin/env bash
  ./bitcoin/build/bin/bitcoind -datadir=./copr -signet 
  rm -rf ./copr/signet

mine:
  #!/usr/bin/env bash
  CLI="./bitcoin/build/bin/bitcoin-cli -signet"
  MINER="./bitcoin/contrib/signet/miner"
  GRIND="./bitcoin/build/bin/bitcoin-util grind"
  ADDR=tb1q73me2ten2cwphzdpl60js6p0vgex8c2e5fqm6m
  NBITS=1d00ffff
  $CLI createwallet copr
  for i in {1..16}; do
    $MINER --cli="$CLI" generate --grind-cmd="$GRIND" --address="$ADDR" --nbits=$NBITS
  done

ckpool:
  #!/usr/bin/env bash
  cd ckpool
  make 
  cd ..
  ./ckpool/src/ckpool --config ./copr/ckpool.conf
  rm -rf logs
