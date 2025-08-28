use {super::*, pool_config::PoolConfig};

mod pool_config;

const EXTRANONCE1: &str = "abcd";

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self) -> Result {
        let config = &self.config;
        let client = config.bitcoin_rpc_client()?;

        client.get_blockchain_info()?;
        // println!("{:?}", client.get_block_template(mode, rules, capabilities));

        let address = config.address();
        let port = config.port();

        let listener = TcpListener::bind((address.clone(), port)).await?;

        info!("Listening on {address}:{port}");

        let (stream, miner) = listener.accept().await?;

        let (mut tcp_reader, mut tcp_writer) = {
            let (rx, tx) = stream.into_split();
            (BufReader::new(rx), BufWriter::new(tx))
        };

        info!("Accepted connection from {miner}");

        let mut line = String::new();

        line.clear();

        match tcp_reader.read_line(&mut line).await {
            Ok(0) => {
                bail!("Miner {miner} disconnected");
            }
            Ok(n) => n,
            Err(e) => {
                bail!("Read error: {e}");
            }
        };

        let msg: Message = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                bail!("Invalid JSON message: {line:?} - {e}");
            }
        };

        match msg {
            Message::Request { id, method, params } => {
                if method == "mining.subscribe" {
                    // TODO: this will panic if params empty
                    let user_agent = &params[0];
                    info!("Received subscribe from {miner} with user agent {user_agent}");
                    let result = SubscribeResult {
                        subscriptions: Vec::new(), //TODO
                        extranonce1: EXTRANONCE1.into(),
                        extranonce2_size: EXTRANONCE2_SIZE.try_into().unwrap(),
                    };

                    let message = Message::Response {
                        id,
                        result: Some(json!(result)),
                        error: None,
                        reject_reason: None,
                    };

                    let frame = serde_json::to_string(&message)? + "\n";
                    tcp_writer.write_all(frame.as_bytes()).await?;
                    tcp_writer.flush().await?;
                } else {
                    todo!()
                }
            }
            _ => todo!(),
        }

        line.clear();

        match tcp_reader.read_line(&mut line).await {
            Ok(0) => {
                bail!("Miner {miner} disconnected");
            }
            Ok(n) => n,
            Err(e) => {
                bail!("Read error: {e}");
            }
        };

        let msg: Message = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                bail!("Invalid JSON message: {line:?} - {e}");
            }
        };

        match msg {
            Message::Request { id, method, params } => {
                if method == "mining.authorize" {
                    // TODO: this will panic if params empty
                    let username = &params[0];

                    let address = Address::from_str(dbg!(
                        username
                            .to_string()
                            .trim_matches('"')
                            .split('.')
                            .collect::<Vec<_>>()[0]
                    ))?
                    .require_network(self.config.chain().network())?;
                    //
                    // TODO: validate address
                    info!("Received authorize from {miner} with username {username}");

                    let message = Message::Response {
                        id,
                        result: Some(json!(true)),
                        error: None,
                        reject_reason: None,
                    };

                    let frame = serde_json::to_string(&message)? + "\n";
                    tcp_writer.write_all(frame.as_bytes()).await?;
                    tcp_writer.flush().await?;

                    let gbt = self.gbt()?;
                    // I have to implement the Vec<u8> to CompactTarget algo (Uint256 bla bla)

                    let (coinbase_tx, coinb1, coinb2) = CoinbaseBuilder {
                        address,
                        aux: gbt.coinbaseaux,
                        extranonce1: EXTRANONCE1.into(),
                        extranonce2_size: EXTRANONCE2_SIZE,
                        height: gbt.height,
                        value: gbt.coinbase_value,
                        witness_commitment: gbt.default_witness_commitment,
                    }
                    .build()?;

                    // all of this is wrong
                    let notify = Notify {
                        job_id: "def123".into(), // TODO
                        prevhash: gbt.previous_block_hash.into(),
                        coinb1,
                        coinb2,
                        merkle_branch: Vec::new(),
                        version: Version(block::Version::from_consensus(
                            gbt.version.try_into().unwrap(),
                        )),
                        nbits: Nbits::from_str("1c2ac4af").unwrap(),
                        ntime: Ntime::from_str("12345678").unwrap(),
                        clean_jobs: true,
                    };

                    let message = Message::Notification {
                        method: "mining.notify".into(),
                        params: json!(notify),
                    };

                    let frame = serde_json::to_string(&message)? + "\n";
                    tcp_writer.write_all(frame.as_bytes()).await?;
                    tcp_writer.flush().await?;
                } else {
                    todo!()
                }
            }
            _ => todo!(),
        }

        line.clear();

        match tcp_reader.read_line(&mut line).await {
            Ok(0) => {
                bail!("Miner {miner} disconnected");
            }
            Ok(n) => n,
            Err(e) => {
                bail!("Read error: {e}");
            }
        };

        let msg: Message = match serde_json::from_str(&line) {
            Ok(msg) => msg,
            Err(e) => {
                bail!("Invalid JSON message: {line:?} - {e}");
            }
        };

        println!("> {line}");

        Ok(())
    }

    pub(crate) fn gbt(&self) -> Result<GetBlockTemplateResult> {
        // TODO: make signet configurable
        // TODO: what other capabilities and rules are there?
        let params = json!({
            "capabilities": ["coinbasetxn", "workid", "coinbase/append"],
            "rules": ["segwit", "signet"]
        });

        info!("Getting block template");

        Ok(self
            .config
            .bitcoin_rpc_client()?
            .call::<GetBlockTemplateResult>("getblocktemplate", &[params])?)
    }
}
