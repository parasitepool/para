use {super::*, pool_config::PoolConfig};

mod pool_config;

#[derive(Parser, Debug)]
pub(crate) struct Pool {
    #[command(flatten)]
    pub(crate) config: PoolConfig,
}

impl Pool {
    pub(crate) async fn run(&self) -> Result {
        let address = self.config.address();
        let port = self.config.port();

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

        let extranonce1 = "abcdef12".to_string();

        match msg {
            Message::Request { id, method, params } => {
                if method == "mining.subscribe" {
                    // TODO: this will panic if params empty
                    let user_agent = &params[0];
                    info!("Received subscribe from {miner} with user agent {user_agent}");
                    let result = SubscribeResult {
                        subscriptions: Vec::new(), //TODO
                        extranonce1: extranonce1.clone(),
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

                    let address = Address::from_str(
                        username
                            .to_string()
                            .trim_matches('"')
                            .split('.')
                            .collect::<Vec<_>>()[0],
                    )?
                    .require_network(self.config.chain().network())?;

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

                    let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
                        address,
                        extranonce1,
                        EXTRANONCE2_SIZE,
                        gbt.height,
                        gbt.coinbase_value,
                        gbt.default_witness_commitment,
                    )
                    .with_aux(gbt.coinbaseaux.into_iter().collect())
                    .with_randomiser(true)
                    .with_timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
                    .with_pool_sig("|parasite|".into())
                    .build()?;

                    let set_difficulty = SetDifficulty(vec![Difficulty(1)]);

                    let message = Message::Notification {
                        method: "mining.set_difficulty".into(),
                        params: json!(set_difficulty),
                    };

                    let frame = serde_json::to_string(&message)? + "\n";
                    tcp_writer.write_all(frame.as_bytes()).await?;
                    tcp_writer.flush().await?;

                    let notify = Notify {
                        job_id: "def123".into(), // TODO
                        prevhash: PrevHash::from(gbt.previous_block_hash),
                        coinb1,
                        coinb2,
                        merkle_branches: stratum::merkle_branches(
                            gbt.transactions
                                .into_iter()
                                .map(|result| result.txid)
                                .collect(),
                        ),
                        version: Version(block::Version::from_consensus(
                            gbt.version.try_into().unwrap(),
                        )),
                        nbits: Nbits::from_str(&hex::encode(gbt.bits))?, // TODO: inefficient
                        ntime: Ntime::try_from(gbt.current_time)
                            .expect("should fit into u32 until the year 2106"),
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
            // TODO: Use own GetBlockTemplateResult so I can deserialize directly from hex myself.
            // Use BTreeMap instead of HashMap
            .call::<GetBlockTemplateResult>("getblocktemplate", &[params])?)
    }
}
