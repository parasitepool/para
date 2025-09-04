use {super::*, crate::subcommand::pool::pool_config::PoolConfig, bitcoin::Block};

#[derive(Debug)]
pub(crate) enum State {
    Init,
    Configured,
    Subscribed,
    Authorized,
    Working,
}

pub(crate) struct Connection<R, W> {
    config: Arc<PoolConfig>,
    worker: SocketAddr,
    reader: R,
    writer: W,
    state: State,
}

impl<R, W> Connection<R, W>
where
    R: AsyncRead + Unpin + AsyncBufReadExt,
    W: AsyncWrite + Unpin,
{
    pub(crate) fn new(config: Arc<PoolConfig>, worker: SocketAddr, reader: R, writer: W) -> Self {
        Self {
            config,
            worker,
            reader,
            writer,
            state: State::Init,
        }
    }
    pub(crate) async fn serve(&mut self) -> Result {
        let extranonce1 = "abcdef12".to_string();

        let gbt = self.gbt()?;
        let prevhash = PrevHash::from(gbt.previous_block_hash);
        let merkle_branches = stratum::merkle_branches(
            gbt.transactions
                .clone()
                .into_iter()
                .map(|r| r.txid)
                .collect(),
        );
        let version = Version(block::Version::from_consensus(
            gbt.version.try_into().unwrap(),
        ));

        let nbits = Nbits::from_str(&hex::encode(gbt.bits.clone()))?;
        let ntime = Ntime::try_from(gbt.current_time).expect("fits into u32 until ~2106");

        let mut coinb1_foo = None;
        let mut coinb2_foo = None;

        while let Some(message) = self.read_message().await? {
            let Message::Request { id, method, params } = message else {
                warn!("not handling anything other than requests");
                continue;
            };

            match (&mut self.state, method.as_str()) {
                (State::Init, "mining.configure") => {
                    info!("CONFIGURE from {}: {params}", self.worker);

                    let _configure = serde_json::from_value::<Configure>(params)?;

                    self.send(Message::Response {
                        id,
                        result: Some(json!({"version-rolling": true, "version-rolling.mask": self.config.version_mask()})),
                        error: None,
                        reject_reason: None,
                    })
                    .await?;
                    self.state = State::Configured;
                }
                (State::Init | State::Configured, "mining.subscribe") => {
                    debug!("SUBSCRIBE from {} with {}", self.worker, params);

                    let subscribe = serde_json::from_value::<Subscribe>(params)?;

                    debug!(
                        "SUBSCRIBE from {} with user agent {}",
                        self.worker, subscribe.user_agent
                    );

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
                (State::Subscribed, "mining.authorize") => {
                    let authorize = serde_json::from_value::<Authorize>(params)?;

                    let address = Address::from_str(
                        authorize
                            .username
                            .trim_matches('"')
                            .split('.')
                            .next()
                            .ok_or_else(|| anyhow!("invalid username format"))?,
                    )?
                    .require_network(self.config.chain().network())
                    .context(format!(
                        "invalid username {} for worker {}",
                        authorize.username, self.worker
                    ))?;

                    info!(
                        "AUTHORIZE from {} with username {}",
                        self.worker, authorize.username
                    );

                    self.send(Message::Response {
                        id,
                        result: Some(json!(true)),
                        error: None,
                        reject_reason: None,
                    })
                    .await?;

                    self.state = State::Authorized;

                    info!("Sending set difficulty");

                    self.send(Message::Notification {
                        method: "mining.set_difficulty".into(),
                        params: json!(SetDifficulty(Difficulty(1))),
                    })
                    .await?;

                    let (_coinbase_tx, coinb1, coinb2) = CoinbaseBuilder::new(
                        address.clone(),
                        extranonce1.clone(),
                        EXTRANONCE2_SIZE,
                        gbt.height,
                        gbt.coinbase_value,
                        gbt.default_witness_commitment.clone(),
                    )
                    .with_aux(gbt.coinbaseaux.clone().into_iter().collect())
                    .with_randomiser(true)
                    .with_timestamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
                    .with_pool_sig("|parasite|".into())
                    .build()?;

                    coinb1_foo = Some(coinb1.clone());
                    coinb2_foo = Some(coinb2.clone());

                    let notify = Notify {
                        job_id: "def123".into(), // TODO
                        prevhash: prevhash.clone(),
                        coinb1,
                        coinb2,
                        merkle_branches: merkle_branches.clone(),
                        version,
                        nbits,
                        ntime,
                        clean_jobs: true,
                    };

                    info!("Sending NOTIFY");

                    self.send(Message::Notification {
                        method: "mining.notify".into(),
                        params: json!(notify),
                    })
                    .await?;

                    info!("Sent NOTIFY");

                    self.state = State::Working;
                }

                (State::Working, "mining.submit") => {
                    info!("SUBMIT from {} with params {}", self.worker, params);

                    let submit = serde_json::from_value::<Submit>(params)?;

                    let version = if let Some(version_bits) = submit.version_bits {
                        assert!(version_bits != 0.into());
                        // assert!((!self.config.version_mask() & version_bits) != 0.into());

                        version | version_bits
                    } else {
                        version
                    };

                    let header = Header {
                        version: version.into(),
                        prev_blockhash: prevhash.clone().into(),
                        merkle_root: stratum::merkle_root(
                            &coinb1_foo.clone().unwrap(),
                            &coinb2_foo.clone().unwrap(),
                            &extranonce1,
                            &submit.extranonce2,
                            &merkle_branches,
                        )?,
                        time: submit.ntime.into(),
                        bits: nbits.into(),
                        nonce: submit.nonce.into(),
                    };

                    let blockhash = header.validate_pow(Target::from_compact(nbits.into()))?;

                    info!("Block with hash {blockhash} mets PoW");

                    let coinbase_bin = hex::decode(format!(
                        "{}{}{}{}",
                        coinb1_foo.clone().unwrap(),
                        extranonce1,
                        submit.extranonce2,
                        coinb2_foo.clone().unwrap()
                    ))?;
                    let mut cursor = bitcoin::io::Cursor::new(&coinbase_bin);
                    let coinbase_tx =
                        bitcoin::Transaction::consensus_decode_from_finite_reader(&mut cursor)?;

                    let txdata = vec![coinbase_tx]
                        .into_iter()
                        .chain(
                            gbt.clone()
                                .transactions
                                .iter()
                                .map(|result| {
                                    Transaction::consensus_decode(&mut result.raw_tx.as_slice())
                                        .unwrap()
                                })
                                .collect::<Vec<Transaction>>(),
                        )
                        .collect();

                    let block = Block { header, txdata };

                    info!("submitting block solve");

                    self.config.bitcoin_rpc_client()?.submit_block(&block)?;

                    self.send(Message::Response {
                        id,
                        result: Some(json!(true)),
                        error: None,
                        reject_reason: None,
                    })
                    .await?;
                }
                (state, method) => {
                    // TODO: log state and method, try to parse and display
                    dbg!(&state);
                    dbg!(&method);
                    dbg!(&params);
                    info!("{params}");
                }
            }
        }

        Ok(())
    }

    async fn read_message(&mut self) -> Result<Option<Message>> {
        // TODO: this should def be sized
        let mut line = String::new();
        match self.reader.read_line(&mut line).await {
            Ok(0) => {
                info!("Worker {} disconnected", self.worker);
                // return Ok(None);
            }
            Ok(n) => info!("{n} bytes read"),
            Err(e) => {
                error!("Read error: {e}");
            }
        };

        match serde_json::from_str::<Message>(&line) {
            Ok(msg) => Ok(Some(msg)),
            Err(e) => Err(anyhow!("Invalid stratum message: {line:?}: {e}")),
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
