build-bitcoind:
  #!/usr/bin/env bash
  git submodule update --init
  cd bitcoin
  sudo apt-get install build-essential cmake pkgconf python3 libevent-dev libboost-dev libsqlite3-dev libzmq3-dev
  cmake -B build
  cmake --build build -j 21

build-ckpool:
  #!/usr/bin/env bash
  git submodule update --init
  cd ckpool
  sudo apt-get install build-essential yasm autoconf automake libtool libzmq3-dev pkgconf libpq-dev
  ./autogen.sh
  ./configure
  make

build: build-bitcoind build-ckpool

bitcoind:
  #!/usr/bin/env bash
  ./bitcoin/build/bin/bitcoind -datadir=./copr -signet 

mine:
  #!/usr/bin/env bash
  CLI="./bitcoin/build/bin/bitcoin-cli -datadir=./copr -signet"
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
  ./ckpool/src/ckpool -B -k --config ./copr/ckpool.conf --loglevel 7 --log-shares

psql:
  #!/usr/bin/env bash
  # Install PostgreSQL if not installed
  if ! dpkg -l | grep -q postgresql; then
    sudo apt-get install -y postgresql postgresql-contrib
  fi

  # Check if PostgreSQL service is running
  if ! systemctl is-active --quiet postgresql; then
    sudo systemctl start postgresql
  fi

  # Create user and database if they don't exist
  if ! sudo -u postgres psql -tAc "SELECT 1 FROM pg_roles WHERE rolname='satoshi'" | grep -q 1; then
    sudo -u postgres psql -c "CREATE USER satoshi WITH PASSWORD 'nakamoto' SUPERUSER;"
  fi

  if ! sudo -u postgres psql -tAc "SELECT 1 FROM pg_database WHERE datname='ckpool_db'" | grep -q 1; then
    sudo -u postgres psql -c "CREATE DATABASE ckpool_db OWNER satoshi;"
  fi

  # Modify pg_hba.conf to use md5 authentication for local connections
  PG_HBA_PATH=$(sudo -u postgres psql -t -c "SHOW hba_file;" | xargs)

  # Backup the original file
  sudo cp $PG_HBA_PATH ${PG_HBA_PATH}.bak

  # Update the authentication method for local connections
  sudo sed -i '/^local.*all.*all.*peer/c\local all all md5' $PG_HBA_PATH
  sudo sed -i '/^host.*all.*all.*127.0.0.1\/32/c\host all all 127.0.0.1/32 md5' $PG_HBA_PATH

  # Reload PostgreSQL to apply changes
  sudo systemctl reload postgresql

  # Create table if it doesn't exist (now using -h localhost to force TCP connection)
  PGPASSWORD="nakamoto" psql -h localhost -U satoshi -d ckpool_db -c "
    CREATE TABLE IF NOT EXISTS shares (
      id SERIAL PRIMARY KEY,
      data JSONB NOT NULL,
      created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
    );
  "

  echo "PostgreSQL is running"
  echo "Database: ckpool_db"
  echo "User: satoshi"
  echo "Password: nakamoto"
  echo "Table: shares with columns (id, data, created_at)"
  echo "Connection string: dbname=ckpool_db user=satoshi password=nakamoto host=localhost"