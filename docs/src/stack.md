Tech Stack
==========

`ckpool`
--------

A fork of [ckpool](https://bitbucket.org/ckolivas/ckpool) that utlizes postgres
for data storage instead of flat files and contains the core logic for
calculating the coinbase transaction with our [payout](payout.md) design.

This is an open source pool implementation that we utilized to quickly build the
proof of concept for what we want parasite pool to be.

`para`
------

`para` is the general purpose tool that backs the majority of pool operations
and acts as both the glue and externally facing part of Parasite Pool. It
includes a few early experimental tools/toys as well as a full-featured API and
share syncing logic for our multi-headed pool node layout. For a full list of
available commands just do `para help`.
