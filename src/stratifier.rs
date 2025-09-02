use {super::*, crate::subcommand::pool::pool_config::PoolConfig};

pub(crate) enum State {
    Init,
    Subscribed,
    Authorized,
    Working,
}

pub(crate) struct Connection<R, W> {
    config: Arc<PoolConfig>,
    peer: SocketAddr,
    reader: R,
    writer: W,
    state: State,
}

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin + AsyncBufReadExt,
    W: AsyncWrite + Unpin,
{
    pub(crate) fn new(config: Arc<PoolConfig>, peer: SocketAddr, reader: R, writer: W) -> Self {
        Self {
            config,
            peer,
            reader,
            writer,
            state: State::Init,
        }
    }
    pub(crate) async fn serve(&mut self) -> Result {
        while let Some(msg) = self.read_message().await? {
            match (&mut self.state, msg) {
                (State::Init, Message::Request { id, method, params })
                    if method == "mining.subscribe" =>
                {
                    let subscribe = serde_json::from_value::<Subscribe>(params)?;
                    info!(
                        "SUBSCRIBE from {} with user agent {}",
                        self.peer, subscribe.user_agent
                    );

                    let extranonce1 = "abcdef12".to_string();
                    let result = SubscribeResult {
                        subscriptions: vec![("mining.notify".into(), "todo".into())],
                        extranonce1: extranonce1.clone(),
                        extranonce2_size: EXTRANONCE2_SIZE.try_into().unwrap(),
                    };

                    self.send(Message::Response {
                        id,
                        result: Some(json!(result)),
                        error: None,
                        reject_reason: None,
                    })
                    .await?;

                    self.state = State::Subscribed;
                }

                (State::Subscribed, Message::Request { id, method, params })
                    if method == "mining.authorize" =>
                {
                    let username = params
                        .get(0)
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| anyhow!("authorize params[0] missing"))?
                        .to_string();

                    let address = Address::from_str(
                        username
                            .trim_matches('"')
                            .split('.')
                            .next()
                            .ok_or_else(|| anyhow!("invalid username format"))?,
                    )?
                    .require_network(self.config.chain().network())?;

                    info!("AUTHORIZE from {} with username {}", self.peer, username);

                    self.send(Message::Response {
                        id,
                        result: Some(json!(true)),
                        error: None,
                        reject_reason: None,
                    })
                    .await?;

                    self.state = State::Authorized;

                    let gbt = self.gbt()?;

                    let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
                        address.clone(),
                        "abcd1234".into(), // TODO: extranonce1 has to be an even number of digits
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

                    self.send(Message::Notification {
                        method: "mining.set_difficulty".into(),
                        params: json!(SetDifficulty(vec![Difficulty(1)])),
                    })
                    .await?;

                    let notify = Notify {
                        job_id: "def123".into(), // TODO
                        prevhash: PrevHash::from(gbt.previous_block_hash),
                        coinb1,
                        coinb2,
                        merkle_branches: stratum::merkle_branches(
                            gbt.transactions.into_iter().map(|r| r.txid).collect(),
                        ),
                        version: Version(block::Version::from_consensus(
                            gbt.version.try_into().unwrap(),
                        )),
                        nbits: Nbits::from_str(&hex::encode(gbt.bits))?,
                        ntime: Ntime::try_from(gbt.current_time)
                            .expect("fits into u32 until ~2106"),
                        clean_jobs: true,
                    };

                    self.send(Message::Notification {
                        method: "mining.notify".into(),
                        params: json!(notify),
                    })
                    .await?;

                    self.state = State::Working;
                }

                (State::Working, Message::Request { id, method, params }) => {
                    match method.as_str() {
                        "mining.submit" => {
                            info!("submit from {}: {params}", self.peer);
                            self.send(Message::Response {
                                id,
                                result: Some(json!(true)),
                                error: None,
                                reject_reason: None,
                            })
                            .await?;
                        }
                        _ => {
                            info!(
                                "unknown request '{}' from {}: {}",
                                method, self.peer, params
                            );
                            self.send(Message::Response {
                                id,
                                result: None,
                                error: None,
                                reject_reason: None,
                            })
                            .await?;
                        }
                    }
                }
                _ => todo!(),
            }
        }

        bail!("Miner {} disconnected", self.peer)
    }

    async fn read_message(&mut self) -> Result<Option<Message>> {
        // TODO: this should def be sized
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) => {
                error!("User disconnected");
            }
            Ok(n) => info!("{n} bytes read"),
            Err(e) => {
                error!("Read error: {e}");
            }
        };

        match serde_json::from_str::<Message>(&line) {
            Ok(msg) => Ok(Some(msg)),
            Err(e) => {
                warn!("Invalid JSON message: {line:?} - {e}");
                Err(anyhow!("Invalid stratum message: {line:?}"))
            }
        }
    }

    async fn send(&mut self, message: Message) -> Result<()> {
        let frame = serde_json::to_string(&message)? + "\n";
        self.writer.write_all(frame.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    fn gbt(&self) -> Result<GetBlockTemplateResult> {
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
