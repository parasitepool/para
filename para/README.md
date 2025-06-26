# TODO

- [ ] implement `para pool` toy pool server
- [ ] ping pong minimal stratum protocol messages between `para pool` and `para
  miner`
- [ ] "unit" test toy pool and toy miner
- [ ] "integration" test with ckpool, signet bitcoind and `para miner`, within
  `test` dir
- [ ]


## Stratum Messages

|Method|Implemented|Tested|
|------|-----------|------|
|mining.notify|[x]|[x]|
|mining.submit|[x]|[x]|
|mining.set_difficulty|[x]|[x]|
|mining.authorize|[x]|[x]|
|mining.subscribe|[x]|[x]|
|mining.get_transactions|[ ]|[ ]|
|client.reconnect|[ ]|[ ]|
|client.show_message|[ ]|[ ]|
|mining.set_extranonce|[ ]|[ ]|

