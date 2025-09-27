Miner Quick Start
===
---

Connecting Your Miners
===
### Miner Setup Guide
- Set up a new private key on Xverse wallet:
  - Create a new wallet (not just a new account)
  - Ensure there are no Bitcoin or Ordinals on it
  - Securely store your private key
- Visit parasite.sati.pro and connect your new Xverse wallet:
  - Complete the wallet connect signature 
  - Copy your generated "static ln address" for later use
- Connect your Bitcoin miner and go to the "Pool Settings" tab
  - Configure your mining settings:
    - Stratum URL: parasite.wtf
    - Port: 42069 (high-diff port, for non-home mining is 42068)
    - Set your username using this format:
      - `YourL1Address.workername.staticlnaddress@staticdomain`
      - !!! Important: Use the same ending/domain (@sati.pro or @parasite.sati.pro) that appears in your generated static ln address !!!
      - Examples:
        - `bc1qnotarealaddress.steveMiner.d1a7a1bef2@sati.pro`
        - `bc1qnotarealaddress.jillAxe.a5f9b2c8e1@parasite.sati.pro`
    - Use any password (it's not checked)
  - Save your configuration and restart if needed
- Verify on your miner that shares begin to accumulate after a few minutes

### Disclaimer

```Participation in Bitcoin mining, including through Parasite Pool, which is still considered in beta testing, involves risks such as market volatility, hardware failure, and changes in network difficulty. Parasite Pool is in beta and has not yet found a block; there is no assurance of future block discoveries or payouts. Users should exercise caution and consider their financial situation before engaging in mining activities.```