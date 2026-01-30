# CKPool Node Mode Summary

## Overview

CKPool's **node mode** (`-N` flag) is a hybrid configuration where ckpool acts as **both a pool AND a proxy simultaneously**:
- Connects to bitcoind locally (like a pool)
- Connects upstream to another pool (like a proxy)
- Generates its own work with its own coinbase
- Submits shares upstream with trust-based validation

## Architecture

```
┌─────────────────────────────────────────┐
│         ckpool in NODE mode             │
│  ┌──────────────┐    ┌──────────────┐  │
│  │   POOL side  │    │  PROXY side  │  │
│  │              │    │              │  │
│  │  Accepts     │    │  Connects    │  │
│  │  miners      │    │  upstream    │  │
│  │              │    │  (sends      │  │
│  │  Generates   │◄───┤  mining.node)│  │
│  │  work from   │    │              │  │
│  │  bitcoind    │    │              │  │
│  └──────────────┘    └──────────────┘  │
└─────────────────────────────────────────┘
```

## Mode Comparison

| Mode | bitcoind | Upstream | Generates Work | Coinbase | Validation |
|------|----------|----------|----------------|----------|------------|
| Regular Pool | ✓ | ✗ | ✓ | Local | Local |
| Proxy (`-p`) | ✗ | ✓ | ✗ | Upstream | Upstream |
| Passthrough (`-P`) | ✗ | ✓ | ✗ | Upstream | Upstream |
| **Node (`-N`)** | **✓** | **✓** | **✓** | **Local** | **Local** |

## Key Differences from Proxy Mode

### 1. **Work Generation**
- **Proxy**: Forwards upstream's work (upstream's coinbase)
- **Node**: Generates own work from bitcoind (your coinbase = you keep block rewards!)

### 2. **Share Validation**
- **Proxy**: Upstream validates every share
- **Node**: You validate locally, upstream trusts your accounting

### 3. **Trust Model**
- **Proxy**: Cryptographic validation (same template)
- **Node**: Network-based trust (IP/port + method + auth)

## Trust & Authentication Model

### Three-Layer Security:

1. **Network Layer (IP/Port)**
   ```json
   "nodeserver": ["10.0.0.1:3335"]  // Only these accept mining.node
   ```

2. **Protocol Layer (Stratum Method)**
   - Connect to `nodeserver` port
   - Send `mining.node` (not `mining.subscribe`)
   - Server marks `client->node = true`

3. **Authentication Layer**
   - Send `mining.authorize` with username/password
   - Standard stratum auth

### Connection Flow:

```
1. TCP connect to nodeserver port
   ↓
2. Send: {"method": "mining.node", "params": []}
   ↓
3. Server validates port is in nodeserver list
   ↓
4. Send: {"method": "mining.authorize", "params": ["user", "pass"]}
   ↓
5. Server accepts shares from this trusted node
```

## Configuration

### Upstream CKPool (Receiving Node):

```json
{
    "nodeserver": ["0.0.0.0:3335"],     // Node connections here
    "serverurl": ["0.0.0.0:3333"],      // Regular miners here
    "trusted": ["0.0.0.0:3336"]        // Trusted remote servers
}
```

### Downstream CKPool (Node Mode Client):

```json
{
    "proxy": "127.0.0.1:3335",
    "proxyauth": ["node_username"],
    "proxypass": ["node_password"],
    "btcaddress": "your_btc_address_for_rewards"
}
```

Or command line:
```bash
./ckpool -N -o 127.0.0.1:3335 -u node_username -p node_password -b your_btc_address
```

## Share Submission Details

### What Gets Submitted Upstream:

**In node mode, ALL valid shares are submitted** (not just block solves):

```c
// stratifier.c:6146-6151
/* Submit share to upstream pool in proxy mode. We submit valid and
 * stale shares and filter out the rest. */
if (wb && wb->proxy && submit) {
    submit_share(client, id, nonce2, ntime, nonce);
}
```

### Share Data Sent:
- `jobid` - upstream workbase ID
- `nonce2` - combined extranonce
- `ntime` - timestamp
- `nonce` - found nonce
- `client_id` - local tracking
- `proxy/subproxy` identifiers

## Hashrate Tracking

### Local (At Your Node):
- Each user/worker has own hashrate tracking
- Decaying averages: 1m, 5m, 15m, 1h, 1d, 7d
- You validate all shares locally

### Upstream:
- Receives individual shares with metadata
- Trusts your local validation
- Cannot verify hashes (different templates!)
- Only validates actual block solves

## Use Cases

1. **Large miner with redundancy**
   - Generate work locally from own bitcoind
   - Submit to upstream for additional rewards/stats
   - Can solo mine if upstream dies

2. **Pool federation**
   - Multiple node-mode ckpools connect to each other
   - Decentralized pool architecture
   - Share workinfo via `mining.node`

3. **Full validation**
   - Get full block data (not just work)
   - Validate entire blocks locally
   - Keep 100% of block rewards

## Important Distinctions

### Node Mode vs Proxy Mode:

| Aspect | Node Mode | Proxy Mode |
|--------|-----------|------------|
| **Block rewards** | You keep 100% | Upstream keeps 100% |
| **Coinbase** | Your address | Upstream's address |
| **Template** | Yours (different) | Upstream's (same) |
| **Validation** | You validate, upstream trusts | Upstream validates |
| **Shares submitted** | All valid shares | All valid shares |
| **Upstream validation** | Trust-based | Cryptographic |

### What Upstream CAN Validate:

- ✓ Block solves (network difficulty) - anyone can validate
- ✓ Your hashrate reporting (trust-based)
- ✗ Individual share hashes (different templates!)
- ✗ Your local difficulty settings

## Security Considerations

- **No certificates** - trust via IP/port + method + auth
- **Firewall required** - nodeserver ports typically internal only
- **Trusted network** - assumes node operators are trustworthy
- **Sybil risk** - malicious nodes could fake hashrate (but why? rewards go to node!)

## Rust Proxy Implementation Notes

To support node mode, implement:
1. Send `mining.node` on connect (instead of `mining.subscribe`)
2. Standard `mining.authorize` with credentials
3. Generate work from local bitcoind (or use provided workinfo)
4. Validate shares locally
5. Submit all valid shares upstream
6. Track hashrate per user/worker
7. Keep block rewards locally

## Code References

- `stratifier.c:6556-6573` - mining.node handling
- `stratifier.c:6146-6151` - share submission upstream
- `stratifier.c:5873-5890` - submit_share function
- `generator.c:1370-1425` - auth_stratum for proxy auth
- `stratifier.c:305-306` - client->trusted and client->node flags
