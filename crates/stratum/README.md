<h1 align=center><code>stratum</code></h1>

<div align=center>
  <a href=https://crates.io/crates/stratum>
    <img src=https://img.shields.io/crates/v/stratum.svg alt="crates.io version">
  </a>
  <a href=https://github.com/parasitepool/para/actions/workflows/ci.yaml>
    <img src=https://github.com/parasitepool/para/actions/workflows/ci.yaml/badge.svg alt="build status">
  </a>
  <a href=https://crates.io/crates/stratum>
    <img src=https://img.shields.io/crates/d/stratum.svg alt=downloads>
  </a>
</div>
<br>

`stratum` is a Rust library for the Stratum mining protocol. It provides types
for protocol messages, some helpers, and an optional async client for connecting
to mining pools. This is experimental software with no warranty. See
[LICENSE](LICENSE) for more details.

Stratum Messages
----------------

| Message                     | Type         | Status |
|-----------------------------|--------------|--------|
| `mining.authorize`          | Request      | ✅     |
| `mining.configure`          | Request      | ✅     |
| `mining.subscribe`          | Request      | ✅     |
| `mining.suggest_difficulty` | Request      | ✅     |
| `mining.submit`             | Request      | ✅     |
| `mining.notify`             | Notification | ✅     |
| `mining.set_difficulty`     | Notification | ✅     |

Types
-----

| Type         | Description                                    |
|--------------|------------------------------------------------|
| `Difficulty` | Mining difficulty with target conversion       |
| `Extranonce` | Extra nonce bytes with hex encoding            |
| `JobId`      | Job identifier (hex-encoded u64)               |
| `MerkleNode` | Merkle tree node with natural big-endian hex   |
| `Nbits`      | Compact target (nBits) from block header       |
| `Nonce`      | 32-bit nonce (hex-encoded)                     |
| `Ntime`      | Block timestamp (hex-encoded u32)              |
| `PrevHash`   | Previous block hash (word-swapped encoding)    |
| `Username`   | Worker identity with Bitcoin address parsing   |
| `Version`    | Block version with bitmask operations          |

Helpers
-------

| Function          | Description                                         |
|-------------------|-----------------------------------------------------|
| `format_si`       | Format a value with SI prefixes (K, M, G, T, ...)  |
| `parse_si`        | Parse SI-prefixed values (e.g., "1.5 TH/s")        |
| `merkle_root`     | Compute merkle root from coinbase and branches     |
| `merkle_branches` | Build merkle branches from non-coinbase txids      |

Feature Flags
-------------

| Flag    | Description                           | Default |
|---------|---------------------------------------|---------|
| `client`| Async Stratum client (requires tokio) | Off     |

Examples
--------

### Parsing Stratum Messages

```rust
use stratum::{Message, Notify, Submit, Difficulty, Id};

// Parse a mining.notify notification
let notify: Notify = serde_json::from_value(json!([
    "bf", "4d16b6f85af6e2198f44ae2a6de67f78487ae5611b77c6c0440b921e00000000",
    "01000000010000000000000000000000000000000000000000000000000000000000000000ffffffff",
    "ffffffff", "1a2b3c4d", [], "00000002", "1c2ac700", "62d5c9e3", false
])).unwrap();

// Parse a generic message
let msg: Message = serde_json::from_str(r#"{"id":1,"method":"mining.subscribe","params":[]}"#).unwrap();
```

### Async Client

```rust
use stratum::{Client, Username, Event, Difficulty};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let client = Client::new(
        "pool.example.com:3333".into(),
        Username::new("bc1qw508d6qejxtdg4y5r3zarvary0c5xw7kv8f3t4.worker"),
        None,
        "my-miner/1.0".into(),
        Duration::from_secs(30),
    );

    let mut events = client.connect().await?;
    client.subscribe().await?;
    client.authorize().await?;

    while let Ok(event) = events.recv().await {
        match event {
            Event::Notify(notify) => println!("new job: {:?}", notify.job_id),
            Event::SetDifficulty(diff) => println!("difficulty: {}", diff),
            Event::Disconnected => break,
        }
    }

    Ok(())
}
```
