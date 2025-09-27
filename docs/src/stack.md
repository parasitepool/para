Tech Stack
==========

- [Parasite Pool](#parasite-pool) (currently a fork of ckpool)
- [Para](#para) (our ongoing development and glue that holds the backend of parasite together)

ParaPool
-------------
A fork of [ckpool](https://bitbucket.org/ckolivas/ckpool) that utlizes postgres for data storage instead of flat files 
and contains the core logic for calculating the coinbase transaction with our [payout](payout.md) design.

This is an open source pool implementation that we utilized to quickly build the proof of concept for what we want
parasite pool to be.

Longterm it is likely that we will move from our fork of ckpool to a pure Rust pool implementation that will be bundled
with Para.

Para
----
Para is the general purpose tool that backs the majority of pool operations and acts as both the glue and externally
facing part of Parasite Pool. It includes a few early experimental tools/toys as well as a full-featured API and 
share syncing logic for our multi-headed pool node layout.

### Commands
- miner      Run a toy miner
  - This is a Sv1-compatible CPU miner implemented in-house to better understand how miners communicate with pools
- ping       Measure Stratum message ping
  - This tool allows users to easily discover their real-world latency to various stratum servers to better optimize their fleet
- pool       Run a toy solo pool
  - A minimum PoC of a solo mining pool in Rust. This is expected to see continued development to replace the existing node software eventually
- server     Run API server
  - The API server, share syncing receiver, and cross-node aggregator that power Parasite Pool
- sync-send  Send shares to HTTP endpoint
  - Sends shares over the wire to `para server` to allow share aggregation and backup remotely
