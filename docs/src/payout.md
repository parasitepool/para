Pool Payout
===========

### Summary
The shares (difficulty adjusted) of the non-winning miners, between the last block mined (or pool genesis) and the newly
mined block, are used to calculate users contribution to that block.

Users are then paid, via Lightning, proportionally to their effort. This amount is equal to the block rewards minus 1 BTC.

The additional 1 BTC is paid directly as part of the coinbase to the miner who found the block.

In this way, miners are able to still participate in 'lottery mining' for a meaningful windfall while getting payouts 
when other pool participants find a block.

### Details (Lightning Payouts)

#### Address Validity / Recovery
In the case where an invalid or malformed address is provided we plan to provide self-service solutions to enable users
to update/set/change their lightning payout address by signing with their associated L1 address. Any rewards that would 
have been allocated for a block find will be left in the lightning channel for later disbursement if we are unable to payout.

#### Amount
The amount disbursed over lightning will be the total amount of the coinbase reward minus 1 BTC

#### Timing
We expect lightning payments to be very quick, but have manual validation enabled for the first few blocks to make sure
that users funds are managed safely. After these controls are fully validated, future payouts will happen almost immediately
when the mined block is buried.

### Details (L1 Payouts)

#### Use as authentication
The private key associated with your miners can easily be used to verify identity by signing a BIP 322 message and 
as such acts as the best way to assert that miners are yours without the need to reveal personal information to us or 
others.

#### Amount
Exactly 1 BTC paid as part of the coinbase, you can verify this by checking the template at [parasite.space](https://parasite.space/template)
the destination of the first output will be directly to the miner who mined the block.